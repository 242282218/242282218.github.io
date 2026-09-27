//! 远程保存：把单篇文章及其专属图片作为**一次提交**推到 `writing` 分支。
//!
//! 关键不变量：
//! - 提交在**隔离索引**（`GIT_INDEX_FILE` 指向临时文件）中构造，因此主工作树
//!   的暂存区、当前分支和未选中文件都不会被改动；
//! - 提交只包含这篇 Markdown 与已确认属于它的图片，不使用无路径约束的
//!   `git add .`；
//! - 永远只推 `writing`，**同步绝不触碰 `main`**；
//! - 只做普通快进推送，从不 `--force`；被拒绝时重新读取远端并进入冲突流程。

use crate::git;
use crate::images;
use crate::local_store::{ArticleBaseline, LocalStore};
use crate::model::{ErrorCode, Result, WriterError};
use crate::paths;
use crate::util;
use crate::workspace::Workspace;
use std::collections::{BTreeMap, BTreeSet};

/// 写作分支名。软件受管的分支只有这一个。
pub const WRITING_BRANCH: &str = "writing";
/// 网站发布分支名。
pub const MAIN_BRANCH: &str = "main";

/// `writing` 分支的初始化方式。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WritingInit {
    /// 远端已存在 `writing`。
    Exists { head: String },
    /// 远端没有 `writing`，需要以 `main` 为起点创建。
    WillCreateFromMain { main_head: String },
}

/// 单篇文章的同步判定结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SyncDecision {
    /// 本地与 `writing` 内容一致，无需推送。
    UpToDate,
    /// 可以安全推送。
    Ready,
    /// 远端该篇（或其图片）已变化，必须停下来由用户决定。
    RemoteChanged,
}

/// 冲突的具体来源，用于界面说明。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum ConflictSource {
    Markdown,
    Image { rel_path: String },
}

/// 一次同步的评估结果。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncAssessment {
    pub article_id: String,
    pub decision: SyncDecision,
    pub local_hash: String,
    pub writing_hash: Option<String>,
    pub base_hash: Option<String>,
    /// `writing` 上该篇的原文（用于差异展示）。
    pub writing_markdown: Option<String>,
    /// 本地该篇的原文。
    pub local_markdown: String,
    pub conflicts: Vec<ConflictSource>,
    /// 需要随本次提交一起写入的图片相对路径。
    pub image_paths: Vec<String>,
}

impl SyncAssessment {
    /// 是否需要用户先处理冲突。
    pub fn needs_user_decision(&self) -> bool {
        self.decision == SyncDecision::RemoteChanged
    }
}

/// 远程保存引擎。
pub struct SyncEngine<'w, 's> {
    workspace: &'w Workspace,
    store: &'s LocalStore,
    /// 期望的远端 URL（已归一化）。
    expected_remote: String,
}

impl<'w, 's> SyncEngine<'w, 's> {
    pub fn new(workspace: &'w Workspace, store: &'s LocalStore, expected_remote: &str) -> Self {
        Self {
            workspace,
            store,
            expected_remote: git::normalize_remote_url(expected_remote),
        }
    }

    /// 校验 `origin` 指向的仓库与配置一致。
    ///
    /// 这一步保证软件只会对用户确认过的仓库执行写操作，避免用户输入的任意
    /// 路径变成 Git 命令的目标。
    pub fn verify_origin(&self) -> Result<()> {
        let out = git::git(self.workspace.root(), &["remote", "get-url", "origin"])?;
        let actual = git::normalize_remote_url(out.stdout_trimmed());
        if actual != self.expected_remote {
            return Err(WriterError::new(
                ErrorCode::InvalidArgument,
                "工作目录的 origin 与配置的目标仓库不一致。切换仓库需要建立新的工作区，不能原地改动 origin",
            )
            .with_detail(format!("实际 origin：{}", git::redact_credentials(out.stdout_trimmed()))));
        }
        Ok(())
    }

    /// 确认当前分支是软件受管的写作分支或主分支之一，避免误操作开发者分支。
    pub fn current_branch(&self) -> Result<String> {
        let out = git::git(self.workspace.root(), &["rev-parse", "--abbrev-ref", "HEAD"])?;
        Ok(out.stdout_trimmed().to_string())
    }

    /// 读取远端某个分支的头；分支不存在时返回 `None`。
    pub fn remote_head(&self, branch: &str) -> Result<Option<String>> {
        validate_branch(branch)?;
        let refspec = format!("refs/heads/{branch}");
        let out =
            git::git(self.workspace.root(), &["ls-remote", "--heads", "origin", &refspec])?;
        let head = out
            .stdout
            .lines()
            .find_map(|line| line.split_whitespace().next())
            .map(str::to_string);
        Ok(head)
    }

    /// 判断远端 `writing` 是否已存在，不存在时说明将由 `main` 初始化。
    pub fn writing_init(&self) -> Result<WritingInit> {
        self.verify_origin()?;
        if let Some(head) = self.remote_head(WRITING_BRANCH)? {
            return Ok(WritingInit::Exists { head });
        }
        let main_head = self.remote_head(MAIN_BRANCH)?.ok_or_else(|| {
            WriterError::new(ErrorCode::GitFailed, "远端缺少 main 分支，无法初始化写作分支")
        })?;
        Ok(WritingInit::WillCreateFromMain { main_head })
    }

    /// 抓取远端分支到本地跟踪引用（不改变工作树与当前分支）。
    ///
    /// 远端还没有该分支时返回 `Ok(None)`，这是首次同步的正常情况。
    pub fn fetch(&self, branch: &str) -> Result<Option<String>> {
        validate_branch(branch)?;
        let refspec = format!("+refs/heads/{branch}:refs/remotes/origin/{branch}");
        // `--no-tags` 与显式 refspec 保证不顺手改动其它分支的引用。
        match git::git(self.workspace.root(), &["fetch", "--no-tags", "origin", &refspec]) {
            Ok(_) => {}
            Err(err) if mentions_missing_remote_ref(&err) => return Ok(None),
            Err(err) => return Err(err),
        }
        let tracking = format!("refs/remotes/origin/{branch}");
        if git::ref_exists(self.workspace.root(), &tracking) {
            Ok(Some(git::rev_parse(self.workspace.root(), &tracking)?))
        } else {
            Ok(None)
        }
    }

    /// 读取某个 ref 下指定文章的 Markdown 原文。
    pub fn markdown_at(&self, rev: &str, article_id: &str) -> Result<Option<String>> {
        let rel = format!("{}{}.md", paths::BLOG_DIR_PREFIX, article_id);
        match git::show_file(self.workspace.root(), rev, &rel)? {
            Some(bytes) => Ok(Some(String::from_utf8_lossy(&bytes).into_owned())),
            None => Ok(None),
        }
    }

    /// 读取某个 ref 下某篇文章全部图片的（相对路径 → 内容哈希）。
    ///
    /// 只统计经文件头确认的图片文件，与本地扫描口径保持一致。
    pub fn image_hashes_at(&self, rev: &str, article_id: &str) -> Result<BTreeMap<String, String>> {
        let dir = format!("{}{}", paths::IMAGE_DIR_PREFIX, article_id);
        let listing = git::list_tree(self.workspace.root(), rev, &dir, "")?;
        let mut out = BTreeMap::new();
        for rel in listing {
            if paths::validate_managed_rel_path(&rel).is_err() {
                continue;
            }
            if !rel.starts_with(&format!("{dir}/")) {
                continue;
            }
            let Some(bytes) = git::show_file(self.workspace.root(), rev, &rel)? else {
                continue;
            };
            // 与本地一致：只接受经 Magic Bytes 确认的图片。
            if crate::util::ImageKind::from_magic(&bytes).is_none() {
                continue;
            }
            out.insert(rel, util::hash_bytes(&bytes));
        }
        Ok(out)
    }

    /// 评估单篇文章能否安全同步。
    ///
    /// 判定基于「本地编辑起始点」基线：远端该篇或图片相对基线发生变化时
    /// 停下并展示差异，不自动覆盖任何一方。
    pub fn assess(&self, article_id: &str) -> Result<SyncAssessment> {
        self.verify_origin()?;
        paths::validate_existing_article_id(article_id)?;

        let local_abs = self.workspace.markdown_abs_path(article_id)?;
        // 局部哈希取**磁盘原文**，与 `record_sync_success` 的基线基准一致。
        let local_markdown = std::fs::read_to_string(&local_abs).map_err(|e| {
            WriterError::new(ErrorCode::ArticleNotFound, format!("读取本地文章失败：{e}"))
        })?;
        let local_hash = util::hash_bytes(local_markdown.as_bytes());

        let baseline = self.store.load_versions().get(article_id).cloned().unwrap_or_default();
        let base_hash = baseline.edit_base_hash.clone();

        let writing_head = self.fetch(WRITING_BRANCH)?;
        let (writing_hash, writing_markdown) = match &writing_head {
            Some(rev) => match self.markdown_at(rev, article_id)? {
                Some(text) => (Some(util::hash_bytes(text.as_bytes())), Some(text)),
                None => (None, None),
            },
            None => (None, None),
        };

        let local_images = images::article_images(self.workspace.root(), article_id)?;
        let local_image_hashes: BTreeMap<String, String> = local_images
            .iter()
            .map(|i| (i.rel_path.clone(), i.content_hash.clone()))
            .collect();

        // 远端图片集合与哈希（仅当远端已有该文章时读取）。
        let remote_image_hashes = match &writing_head {
            Some(rev) => self.image_hashes_at(rev, article_id)?,
            None => BTreeMap::new(),
        };

        // ---- 逐项比较三处状态 ----
        // Markdown：本地是否相对基线移动、远端是否相对上次同步移动。
        let local_markdown_moved = match (&base_hash, &local_hash) {
            (Some(base), current) => base != current,
            // 尚无基线（首次同步）时，只要远端已有不同内容就视为本地有内容要推。
            (None, _) => true,
        };
        let remote_markdown_moved = match (&writing_hash, &baseline.writing_hash) {
            (Some(current), Some(known)) => current != known,
            (Some(_), None) => true,
            (None, _) => false,
        };

        // 图片：按路径逐个比较「本地 vs 远端」与「远端 vs 上次同步」。
        let mut conflicts = Vec::new();
        let mut images_changed_remotely = false;
        let all_image_paths: BTreeSet<&String> =
            local_image_hashes.keys().chain(remote_image_hashes.keys()).collect();
        for rel in all_image_paths {
            let local = local_image_hashes.get(rel);
            let remote = remote_image_hashes.get(rel);
            if local == remote {
                continue;
            }
            let known = baseline.writing_image_hashes.get(rel);
            let remote_moved = match (remote, known) {
                (Some(current), Some(k)) => current != k,
                (Some(_), None) => true,
                // 远端已没有该图（被他人删除）也是变化。
                (None, Some(_)) => true,
                (None, None) => false,
            };
            if !remote_moved {
                continue;
            }
            images_changed_remotely = true;
            if local.is_some() {
                // 双方都有但内容不同（或一方新增）→ 无法文本合并的图片冲突。
                conflicts.push(ConflictSource::Image { rel_path: rel.clone() });
            }
        }

        let local_moved = local_markdown_moved || local_image_hashes != baseline.writing_image_hashes;
        let remote_images_match_local = local_image_hashes == remote_image_hashes;

        let decision = if writing_hash.as_deref() == Some(local_hash.as_str())
            && remote_images_match_local
        {
            // Markdown 与图片都与远端一致 → 无需提交。
            SyncDecision::UpToDate
        } else if remote_markdown_moved && local_markdown_moved {
            // 两边都改了 Markdown → 必须由用户决定。
            if !conflicts.iter().any(|c| matches!(c, ConflictSource::Markdown)) {
                conflicts.push(ConflictSource::Markdown);
            }
            SyncDecision::RemoteChanged
        } else if remote_markdown_moved && writing_hash.is_some() && base_hash.is_some() {
            // 本地 Markdown 未动但远端已改：不静默覆盖任何一方。
            if !conflicts.iter().any(|c| matches!(c, ConflictSource::Markdown)) {
                conflicts.push(ConflictSource::Markdown);
            }
            SyncDecision::RemoteChanged
        } else if !conflicts.is_empty() {
            // 图片冲突：远端图片已变且本地对该图也有内容。
            SyncDecision::RemoteChanged
        } else if images_changed_remotely && !local_moved {
            // 远端图片已变、本地未动：交由用户确认后再处理。
            SyncDecision::RemoteChanged
        } else {
            SyncDecision::Ready
        };

        let image_paths = local_images.iter().map(|i| i.rel_path.clone()).collect();
        Ok(SyncAssessment {
            article_id: article_id.to_string(),
            decision,
            local_hash,
            writing_hash,
            base_hash,
            writing_markdown,
            local_markdown,
            conflicts,
            image_paths,
        })
    }

    /// 执行同步：在隔离索引中构造仅含该篇的提交并快进推送到 `writing`。
    ///
    /// 前置条件：本地已保存且 [`Self::assess`] 的结论不是 `RemoteChanged`。
    ///
    /// `adopt_local`：用户在冲突界面显式选择「采用本地并覆盖远端」时为 `true`。
    /// 这是**唯一**允许覆盖远端同篇内容的分支；为 `false` 时遇到 `RemoteChanged`
    /// 一律报错，交由差异界面处理。
    pub fn push_article(
        &self,
        article_id: &str,
        commit_message: &str,
        adopt_local: bool,
    ) -> Result<SyncOutcome> {
        self.verify_origin()?;
        paths::validate_existing_article_id(article_id)?;

        let assessment = self.assess(article_id)?;
        match assessment.decision {
            SyncDecision::UpToDate => {
                return Ok(SyncOutcome {
                    pushed_commit: None,
                    writing_hash: assessment.writing_hash,
                    created_branch: false,
                })
            }
            SyncDecision::RemoteChanged if !adopt_local => {
                return Err(WriterError::new(
                    ErrorCode::RemoteChanged,
                    "远端这篇文章已变化，需要先在差异界面选择处理方式",
                ));
            }
            _ => {}
        }

        let init = self.writing_init()?;
        let (parent, created_branch) = match &init {
            WritingInit::Exists { head } => (head.clone(), false),
            WritingInit::WillCreateFromMain { main_head } => (main_head.clone(), true),
        };

        // 远端在评估之后仍在移动时，以最新头为父提交重新构造。
        let markdown_rel = format!("{}{}.md", paths::BLOG_DIR_PREFIX, article_id);
        let image_rels = assessment.image_paths.clone();

        let commit = self.build_commit(
            &parent,
            &markdown_rel,
            &image_rels,
            article_id,
            commit_message,
        )?;

        // 推送前再次核头：父提交必须仍是远端 `writing` 的头，才算快进。
        let current_head = self.remote_head(WRITING_BRANCH)?;
        let expected = if created_branch { None } else { Some(parent.as_str()) };
        if current_head.as_deref() != expected {
            return Err(WriterError::new(
                ErrorCode::PushRejected,
                "推送前发现远端写作分支已更新，已停止推送。请重新同步以基于最新版本构造提交",
            ));
        }

        let refspec = format!("{commit}:refs/heads/{WRITING_BRANCH}");
        // 普通推送：绝不使用 --force。被拒绝（非快进）时重新读取远端头并
        // 给出可操作提示，让用户回到差异界面基于最新版本重来，而不是在这里
        // 静默 merge/rebase 用户文章。
        git::git(self.workspace.root(), &["push", "origin", &refspec]).map_err(|err| {
            if err.code == ErrorCode::PushRejected {
                let latest = self.remote_head(WRITING_BRANCH).ok().flatten();
                let detail = latest.unwrap_or_else(|| "（无法读取远端头）".to_string());
                WriterError::new(
                    ErrorCode::PushRejected,
                    "推送被拒绝：远端写作分支已前进。已重新读取远端版本，请重新同步以基于最新版本构造本次提交",
                )
                .with_detail(detail)
            } else {
                err
            }
        })?;

        // 推送后以远端 ref 核实结果，再更新基线。
        let verified = self.remote_head(WRITING_BRANCH)?;
        if verified.as_deref() != Some(commit.as_str()) {
            return Err(WriterError::new(
                ErrorCode::GitFailed,
                "推送后未能确认远端写作分支指向本次提交，已保留本地改动与重试入口",
            ));
        }

        self.record_sync_success(article_id, &commit)?;
        Ok(SyncOutcome {
            pushed_commit: Some(commit.clone()),
            writing_hash: Some(assessment.local_hash),
            created_branch,
        })
    }

    /// 在隔离索引中构造一个只改动指定文章（及其图片）的提交。
    ///
    /// `parent` 必须是当前远端头，从而提交天然是快进关系。
    ///
    /// 写入索引一律用 `hash-object -w` + `update-index --cacheinfo`，而不是
    /// `git add -- <path>`：`git add` 的路径参数仍会按 pathspec **通配**展开，
    /// 若仓库中存在含 `[`、`*` 等字符的文件名，一次同步可能把别的文章一并
    /// 提交进去（`--` 只终止选项解析，不关闭通配）。`--cacheinfo` 接收的是
    /// 字面路径与对象 ID，不存在通配面。
    fn build_commit(
        &self,
        parent: &str,
        markdown_rel: &str,
        image_rels: &[String],
        article_id: &str,
        commit_message: &str,
    ) -> Result<String> {
        paths::validate_managed_markdown(markdown_rel)?;
        for rel in image_rels {
            paths::validate_managed_rel_path(rel)?;
        }

        let index_file = crate::git::temp_path("index");
        let index_env = index_file.to_string_lossy().into_owned();
        // 隔离索引从父提交的树开始，因此提交内容 = 父树 + 本次改动。
        let result = (|| -> Result<String> {
            git::git_with_env(
                self.workspace.root(),
                &["read-tree", parent],
                &[("GIT_INDEX_FILE", index_env.clone())],
            )?;

            for rel in std::iter::once(markdown_rel).chain(image_rels.iter().map(String::as_str)) {
                let abs = self.workspace.root().join(rel);
                let bytes = std::fs::read(&abs).map_err(|e| {
                    WriterError::new(ErrorCode::IoFailed, format!("读取待同步文件失败：{e}"))
                })?;
                // 非 Markdown 的受管路径只能是图片，必须是经文件头确认的格式。
                if rel != markdown_rel && util::ImageKind::from_magic(&bytes).is_none() {
                    return Err(WriterError::new(
                        ErrorCode::ImageUnsupported,
                        "待同步的图片未通过文件头校验，已停止同步",
                    )
                    .with_detail(rel.to_string()));
                }
                let blob = crate::publish::write_blob_helper(self.workspace, &bytes)?;
                git::git_with_env(
                    self.workspace.root(),
                    &["update-index", "--add", "--cacheinfo", "100644", &blob, rel],
                    &[("GIT_INDEX_FILE", index_env.clone())],
                )?;
            }

            let tree = git::git_with_env(
                self.workspace.root(),
                &["write-tree"],
                &[("GIT_INDEX_FILE", index_env.clone())],
            )?
            .stdout_trimmed()
            .to_string();

            // 事后白名单核对：本次提交相对父提交的变更必须全部在允许范围，
            // 且只涉及这一篇及其图片。与发布路径同样的双重断言，避免
            // 「输入路径正确」成为唯一保证。
            let changed = self.diff_name_only_between(parent, &tree)?;
            if let Err(offenders) = all_paths_allowed(&changed) {
                return Err(WriterError::new(
                    ErrorCode::GitFailed,
                    "本次同步将改动受管目录之外的文件，已停止同步",
                )
                .with_detail(offenders.join("、")));
            }
            let image_dir = format!("{}{}/", paths::IMAGE_DIR_PREFIX, article_id);
            for rel in &changed {
                if rel != markdown_rel && !rel.starts_with(&image_dir) {
                    return Err(WriterError::new(
                        ErrorCode::GitFailed,
                        "本次同步将带出其他文章或图片，已停止同步",
                    )
                    .with_detail(rel.clone()));
                }
            }

            let message = format!("{commit_message}\n\nArticle: {article_id}");
            let commit = git::git(
                self.workspace.root(),
                &["commit-tree", &tree, "-p", parent, "-m", &message],
            )?
            .stdout_trimmed()
            .to_string();
            Ok(commit)
        })();

        let _ = std::fs::remove_file(&index_file);
        result
    }

    /// 列出两个树对象之间变更的文件路径。
    fn diff_name_only_between(&self, from_tree: &str, to_tree: &str) -> Result<Vec<String>> {
        let out = git::git(
            self.workspace.root(),
            &["diff-tree", "--no-commit-id", "--name-only", "-r", "-z", from_tree, to_tree],
        )?;
        Ok(out
            .stdout
            .split('\0')
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect())
    }

    /// 记录一次成功同步的基线与图片哈希。
    pub fn record_sync_success(&self, article_id: &str, commit: &str) -> Result<()> {
        let markdown_abs = self.workspace.markdown_abs_path(article_id)?;
        let content_hash = util::hash_file(&markdown_abs)?;
        let image_hashes: BTreeMap<String, String> = images::article_images(self.workspace.root(), article_id)?
            .into_iter()
            .map(|i| (i.rel_path, i.content_hash))
            .collect();
        self.store.update_baseline(article_id, move |baseline: &mut ArticleBaseline| {
            baseline.writing_commit = Some(commit.to_string());
            baseline.writing_hash = Some(content_hash.clone());
            baseline.edit_base_hash = Some(content_hash);
            baseline.writing_image_hashes = image_hashes;
            baseline.last_edited_unix = Some(util::unix_seconds());
        })?;
        Ok(())
    }

    /// 记录本地编辑起始点，供后续冲突检测使用。
    pub fn record_edit_base(&self, article_id: &str) -> Result<()> {
        let markdown_abs = self.workspace.markdown_abs_path(article_id)?;
        let content_hash = util::hash_file(&markdown_abs)?;
        let image_hashes: BTreeMap<String, String> = images::article_images(self.workspace.root(), article_id)?
            .into_iter()
            .map(|i| (i.rel_path, i.content_hash))
            .collect();
        self.store.update_baseline(article_id, move |baseline: &mut ArticleBaseline| {
            baseline.edit_base_hash = Some(content_hash);
            baseline.edit_base_image_hashes = image_hashes;
        })?;
        Ok(())
    }
}

/// 一次成功同步的结果。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncOutcome {
    pub pushed_commit: Option<String>,
    pub writing_hash: Option<String>,
    /// 本次操作是否顺带创建了远端 `writing` 分支。
    pub created_branch: bool,
}

/// 判断错误是否只是「远端还没有这个分支」。
///
/// 首次同步前 `writing` 并不存在，这不是失败。
fn mentions_missing_remote_ref(err: &WriterError) -> bool {
    let haystack = err.detail.as_deref().unwrap_or("").to_lowercase();
    err.code == ErrorCode::GitFailed
        && (haystack.contains("couldn't find remote ref")
            || haystack.contains("could not find remote ref")
            || haystack.contains("doesn't exist")
            || haystack.contains("couldn't find remote branch"))
}

/// 校验分支名只使用软件受管的分支，避免把任意字符串拼进 refspec。
pub fn validate_branch(branch: &str) -> Result<()> {
    if branch == WRITING_BRANCH || branch == MAIN_BRANCH {
        Ok(())
    } else {
        Err(WriterError::new(
            ErrorCode::InvalidArgument,
            "只允许操作受管的写作分支与主分支",
        ))
    }
}

/// 判断路径是否在受管白名单内（供发布与删除复用）。
pub fn is_allowed_change(rel_path: &str) -> bool {
    paths::validate_managed_rel_path(rel_path).is_ok()
}

/// 校验一组变更路径是否全部落在允许清单内。
pub fn all_paths_allowed(paths: &[String]) -> std::result::Result<(), Vec<String>> {
    let offenders: Vec<String> =
        paths.iter().filter(|p| !is_allowed_change(p)).cloned().collect();
    if offenders.is_empty() {
        Ok(())
    } else {
        Err(offenders)
    }
}

/// 校验一个 ref 是否是完整的提交对象 ID（40 位十六进制）。
pub fn is_commit_sha(value: &str) -> bool {
    value.len() == 40 && value.chars().all(|c| c.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branch_validation_blocks_arbitrary_refs() {
        assert!(validate_branch(WRITING_BRANCH).is_ok());
        assert!(validate_branch(MAIN_BRANCH).is_ok());
        assert!(validate_branch("main; rm -rf /").is_err());
        assert!(validate_branch("feature/x").is_err());
    }

    #[test]
    fn allowed_paths_only_include_managed_dirs() {
        assert!(is_allowed_change("src/content/blog/a.md"));
        assert!(is_allowed_change("public/blog/a/f.png"));
        assert!(!is_allowed_change("src/pages/index.astro"));
        assert!(!is_allowed_change("package.json"));
        assert!(!is_allowed_change(".github/workflows/deploy.yml"));
        assert!(!is_allowed_change("src/content/config.ts"));

        let ok = vec!["src/content/blog/a.md".to_string()];
        assert!(all_paths_allowed(&ok).is_ok());

        let bad = vec![
            "src/content/blog/a.md".to_string(),
            "src/pages/index.astro".to_string(),
        ];
        let offenders = all_paths_allowed(&bad).unwrap_err();
        assert_eq!(offenders, vec!["src/pages/index.astro"]);
    }

    #[test]
    fn commit_sha_detection() {
        assert!(is_commit_sha("0123456789abcdef0123456789abcdef01234567"));
        assert!(!is_commit_sha("0123456"));
        assert!(!is_commit_sha("zzzz456789abcdef0123456789abcdef01234567"));
    }
}

#[cfg(test)]
mod integration {
    use super::*;
    use crate::model::ArticleMeta;
    use crate::testkit::{sample_markdown, sample_markdown_with_body, tiny_png, TestEnv};

    fn meta(title: &str, draft: bool) -> ArticleMeta {
        ArticleMeta {
            title: title.to_string(),
            description: format!("{title} 的测试摘要"),
            pub_date: "2026-09-23".to_string(),
            updated_date: None,
            tags: vec!["测试样稿".to_string()],
            draft,
        }
    }

    fn engine<'a>(env: &'a TestEnv) -> SyncEngine<'a, 'a> {
        SyncEngine::new(
            &env.workspace,
            &env.store,
            &format!("file://{}", env.remote.to_string_lossy().replace('\\', "/")),
        )
    }

    /// A2：同步草稿只改 `writing`，`main` 不变。
    #[test]
    fn a2_sync_draft_only_changes_writing() {
        let env = TestEnv::new();
        env.workspace.create("draft-a", &meta("草稿 A", true), "草稿正文\n").unwrap();
        let main_before = env.remote_head("main").unwrap();

        let engine = engine(&env);
        let init = engine.writing_init().unwrap();
        assert!(matches!(init, WritingInit::WillCreateFromMain { .. }), "首次同步应从 main 创建");

        let outcome = engine.push_article("draft-a", "同步草稿 A", false).unwrap();
        assert!(outcome.created_branch, "应报告创建了写作分支");
        assert!(outcome.pushed_commit.is_some());

        // writing 上有这篇；main 完全没有变化。
        assert!(env
            .remote_file_direct("writing", "src/content/blog/draft-a.md")
            .is_some());
        assert_eq!(env.remote_head("main").unwrap(), main_before, "同步绝不能让 main 变化");
        assert!(env.remote_ls("main", "src/content/blog").iter().all(|p| !p.contains("draft-a")));
    }

    /// 同步不污染开发者当前分支，也不动工作树的暂存区。
    #[test]
    fn sync_does_not_touch_working_tree_or_current_branch() {
        let env = TestEnv::new();
        env.workspace.create("iso-a", &meta("隔离测试", true), "正文\n").unwrap();

        // 预先在受管范围内放一个未提交改动，它不应被顺手提交。
        std::fs::write(env.path().join("src/content/blog/.gitkeep"), "kept\n").unwrap();

        let branch_before = env.current_branch();
        let engine = engine(&env);
        engine.push_article("iso-a", "同步", false).unwrap();

        assert_eq!(env.current_branch(), branch_before, "当前分支不得改变");
        let dirty = env.managed_dirty();
        assert!(
            dirty.iter().any(|l| l.contains(".gitkeep")),
            "未被选中的改动必须保留在工作树中：{dirty:?}"
        );
        // writing 提交里不应包含 .gitkeep 的改动。
        let committed = env
            .remote_file_direct("writing", "src/content/blog/.gitkeep")
            .unwrap_or_default();
        assert_ne!(committed, "kept\n", "不在清单内的文件不得进入提交");
    }

    /// P1 回归：`git add` 的路径参数会被 pathspec 通配展开。
    ///
    /// 仓库里若存在含 `[` 的文件名（`a[bc].md`），同步 `a[bc]` 时旧实现会
    /// 同时把 `ab.md`、`ac.md` 的改动一并提交。改用 hash-object + update-index
    /// 的字面路径后，提交必须只包含被同步的那一篇。
    #[test]
    fn sync_does_not_stage_glob_siblings() {
        let env = TestEnv::new();
        // 三个文件名：一个是通配模式，两个是它会匹配到的兄弟文件。
        // `[` 不允许用于**新建**文章，但既有仓库文件可以是这种名字，
        // 因此这里直接落盘模拟已存在的文章。
        for name in ["a[bc]", "ab", "ac"] {
            std::fs::write(
                env.path().join(format!("src/content/blog/{name}.md")),
                crate::testkit::sample_markdown(&format!("改后 {name}"), true),
            )
            .unwrap();
        }

        let engine = engine(&env);
        let outcome = engine.push_article("a[bc]", "同步模式名文章", false).unwrap();
        assert!(outcome.pushed_commit.is_some());

        // 提交只能包含 a[bc].md；兄弟文件不得进入远端。
        let committed = env.remote_ls("writing", "src/content/blog");
        assert!(
            committed.iter().any(|p| p.ends_with("a[bc].md")),
            "被同步的文章应进入提交：{committed:?}"
        );
        assert!(
            !committed.iter().any(|p| p.ends_with("ab.md") || p.ends_with("ac.md")),
            "pathspec 通配不得把兄弟文件带进提交：{committed:?}"
        );
    }

    /// 重复同步同一内容时不做多余提交。
    #[test]
    fn up_to_date_sync_is_a_no_op() {
        let env = TestEnv::new();
        env.workspace.create("noop-a", &meta("无变化", true), "正文\n").unwrap();
        let engine = engine(&env);
        engine.push_article("noop-a", "第一次", false).unwrap();
        let head_after_first = env.remote_head("writing").unwrap();

        let outcome = engine.push_article("noop-a", "第二次", false).unwrap();
        assert!(outcome.pushed_commit.is_none(), "内容一致时不应产生新提交");
        assert_eq!(env.remote_head("writing").unwrap(), head_after_first);
    }

    /// 图片随文章一起进入同一次提交。
    #[test]
    fn images_travel_with_the_article_commit() {
        let env = TestEnv::new();
        crate::images::import_image_bytes(env.path(), "with-img", "图 1", &tiny_png()).unwrap();
        env.workspace
            .create("with-img", &meta("带图", true), "![图](/blog/with-img/x.png)\n")
            .unwrap();

        let engine = engine(&env);
        engine.push_article("with-img", "带图同步", false).unwrap();

        let images = env.remote_ls("writing", "public/blog/with-img");
        assert_eq!(images.len(), 1, "图片应随文章一起入库：{images:?}");
        assert!(images[0].ends_with(".png"), "实际路径：{images:?}");
        // main 上不应出现这篇文章的图片或文章本身。
        assert!(env.remote_ls("main", "public/blog/with-img").is_empty());
        assert!(env.remote_ls("main", "src/content/blog").iter().all(|p| !p.contains("with-img")));
    }

    /// A3：已发布文章的新稿同步后，网站仍是旧版（`main` 不动）。
    #[test]
    fn a3_syncing_new_draft_of_published_article_keeps_site_version() {
        let env = TestEnv::new();
        // 先在 main 上有一篇已发布文章。
        let published = sample_markdown("已发布文章", false);
        std::fs::write(env.path().join("src/content/blog/published-a.md"), &published).unwrap();
        env.commit_managed("发布文章 A");
        env.push_current("main");
        let main_head = env.remote_head("main").unwrap();

        // 本地改出新稿并同步到 writing。
        let mut meta_new = meta("已发布文章", false);
        meta_new.description = "改过的摘要".to_string();
        env.workspace
            .save("published-a", &meta_new, "改过的正文\n", None)
            .unwrap();
        let engine = engine(&env);
        engine.push_article("published-a", "新稿同步", false).unwrap();

        // writing 上是新稿，main 上仍是旧内容。
        let writing = env
            .remote_file_direct("writing", "src/content/blog/published-a.md")
            .unwrap();
        assert!(writing.contains("改过的正文"));
        let main = env.remote_file_direct("main", "src/content/blog/published-a.md").unwrap();
        assert!(main.contains("测试样稿正文"), "main 必须保持旧版本");
        assert!(!main.contains("改过的正文"));
        assert_eq!(env.remote_head("main").unwrap(), main_head);
    }

    /// A4：他处修改同篇远端后再同步 → 冲突而不是静默覆盖。
    #[test]
    fn a4_remote_change_produces_conflict_without_overwrite() {
        let env = TestEnv::new();
        env.workspace.create("shared-a", &meta("共享文章", true), "本地初稿\n").unwrap();
        let engine = engine(&env);
        engine.push_article("shared-a", "初次同步", false).unwrap();

        // 另一台设备在 writing 上改同一篇并推送。
        let other = env.other_device("other");
        other.fetch("writing");
        other.checkout_new_branch("writing", "origin/writing");
        other.write_and_commit(
            "src/content/blog/shared-a.md",
            &sample_markdown_with_body("共享文章", true, "远端设备改过的正文"),
            "远端修改",
        );
        other.push("writing");
        let remote_head_after_other = env.remote_head("writing").unwrap();

        // 本地也改这篇。
        let mut m = meta("共享文章", true);
        m.description = "本地改过的摘要".to_string();
        env.workspace.save("shared-a", &m, "本地改过的正文\n", None).unwrap();

        let assessment = engine.assess("shared-a").unwrap();
        assert_eq!(assessment.decision, SyncDecision::RemoteChanged, "{assessment:?}");
        assert!(assessment.needs_user_decision());
        assert!(assessment.conflicts.iter().any(|c| matches!(c, ConflictSource::Markdown)));
        // 差异界面需要双方原文。
        assert!(assessment.writing_markdown.as_deref().unwrap().contains("远端设备改过的正文"));
        assert!(assessment.local_markdown.contains("本地改过的正文"));

        // 未确认时不推送，远端保持他处设备的版本。
        let err = engine.push_article("shared-a", "不应成功", false).unwrap_err();
        assert_eq!(err.code, ErrorCode::RemoteChanged);
        assert_eq!(env.remote_head("writing").unwrap(), remote_head_after_other);
        let remote_text = env
            .remote_file_direct("writing", "src/content/blog/shared-a.md")
            .unwrap();
        assert!(remote_text.contains("远端设备改过的正文"), "远端不得被覆盖");
        // 本地内容也没丢。
        assert!(env.workspace.read("shared-a", &Default::default()).unwrap().body.contains("本地改过的正文"));
    }

    /// 同篇真冲突下用户显式选择「采用本地并覆盖远端」时：
    /// 这是**唯一**会改写远端既有内容的同步分支，必须
    /// 1) 覆盖前不动作、2) 确认后以最新远端头为父提交快进推送、
    /// 3) 远端内容被本篇本地版本替换、4) 不误删他处推进的其他文章。
    #[test]
    fn adopt_local_overwrites_remote_on_explicit_confirmation() {
        let env = TestEnv::new();
        env.workspace.create("adopt-ov", &meta("覆盖远端", true), "本地初稿\n").unwrap();
        let engine = engine(&env);
        engine.push_article("adopt-ov", "初次同步", false).unwrap();

        // 他处设备改**同一篇**，并顺带新增另一篇（用于确认不误伤）。
        let other = env.other_device("other-overwrite");
        other.fetch("writing");
        other.checkout_new_branch("writing", "origin/writing");
        other.write_and_commit(
            "src/content/blog/adopt-ov.md",
            &sample_markdown_with_body("覆盖远端", true, "远端设备的正文"),
            "远端改同篇",
        );
        other.write_and_commit(
            "src/content/blog/adopt-other.md",
            &sample_markdown("他处文章", true),
            "他处新增文章",
        );
        other.push("writing");
        let head_after_other = env.remote_head("writing").unwrap();

        // 本地也改这篇 → 真冲突。
        let mut m = meta("覆盖远端", true);
        m.description = "本地采纳版摘要".to_string();
        env.workspace.save("adopt-ov", &m, "本地采纳版正文\n", None).unwrap();
        assert_eq!(engine.assess("adopt-ov").unwrap().decision, SyncDecision::RemoteChanged);

        // 未确认前不得改动远端。
        assert_eq!(env.remote_head("writing").unwrap(), head_after_other);

        // 显式「采用本地」：允许覆盖远端同篇内容。
        let outcome = engine.push_article("adopt-ov", "采用本地覆盖", true).unwrap();
        assert!(outcome.pushed_commit.is_some(), "采用本地必须产生提交");

        // 远端同篇已被本地版本覆盖（这正是用户确认的结果）。
        let remote_text = env
            .remote_file_direct("writing", "src/content/blog/adopt-ov.md")
            .unwrap();
        assert!(remote_text.contains("本地采纳版正文"), "确认后远端应被本地版本覆盖");
        assert!(!remote_text.contains("远端设备的正文"), "旧远端正文已被替换");

        // 提交以最新远端头为父提交（快进），他处新增的文章不得被抹掉。
        let head_after_adopt = env.remote_head("writing").unwrap();
        assert_ne!(head_after_adopt, head_after_other);
        assert!(
            env.remote_file_direct("writing", "src/content/blog/adopt-other.md").is_some(),
            "采用本地不得删掉他处推进的其他文章"
        );

        // 覆盖后本地与远端一致，状态回到已同步。
        assert_eq!(engine.assess("adopt-ov").unwrap().decision, SyncDecision::UpToDate);
    }

    /// 采用本地时，提交以最新远端头为父提交并快进推送（不重置远端分支）。
    #[test]
    fn adopt_local_rebuilds_commit_on_latest_remote_head() {
        let env = TestEnv::new();
        env.workspace.create("adopt-a", &meta("采用本地", true), "初稿\n").unwrap();
        let engine = engine(&env);
        engine.push_article("adopt-a", "初", false).unwrap();

        // 他处设备改了**别的**文章，推动 writing 前进。
        let other = env.other_device("other-adopt");
        other.fetch("writing");
        other.checkout_new_branch("writing", "origin/writing");
        other.write_and_commit(
            "src/content/blog/other-b.md",
            &sample_markdown("另一篇", true),
            "他处新增文章",
        );
        other.push("writing");
        let other_head = env.remote_head("writing").unwrap();

        // 本地改自己的文章，远端这篇没动 → 应可安全同步，并带上他处的提交。
        let mut m = meta("采用本地", true);
        m.description = "本地更新".to_string();
        env.workspace.save("adopt-a", &m, "本地更新正文\n", None).unwrap();

        let assessment = engine.assess("adopt-a").unwrap();
        assert_eq!(assessment.decision, SyncDecision::Ready, "无关文章的推进不应误报冲突");

        let outcome = engine.push_article("adopt-a", "基于最新头重构", false).unwrap();
        let new_head = outcome.pushed_commit.clone().unwrap();
        // 新提交必须以他处设备的头为父提交（快进关系）。
        let parent = env.git(&["rev-parse", &format!("{new_head}^")]);
        assert_eq!(parent, other_head, "必须在他处设备的提交之上构造");
        // 无关文章仍然在 writing 上存在。
        assert!(env
            .remote_file_direct("writing", "src/content/blog/other-b.md")
            .is_some());
    }

    /// 远端被推进后仍以旧头推送 → 被拒绝，然后重新评估。
    #[test]
    fn stale_head_push_is_rejected_then_reassessed() {
        let env = TestEnv::new();
        env.workspace.create("race-a", &meta("竞态", true), "初稿\n").unwrap();
        let engine = engine(&env);
        engine.push_article("race-a", "初", false).unwrap();

        let assessment = engine.assess("race-a").unwrap();
        assert_eq!(assessment.decision, SyncDecision::UpToDate);

        // 他处设备推进 writing。
        let other = env.other_device("other-race");
        other.fetch("writing");
        other.checkout_new_branch("writing", "origin/writing");
        other.write_and_commit(
            "src/content/blog/race-c.md",
            &sample_markdown("他处文章", true),
            "推进",
        );
        other.push("writing");
        let advanced = env.remote_head("writing").unwrap();

        // 本地改这篇后同步：必须基于新头，且不破坏他处内容。
        let mut m = meta("竞态", true);
        m.description = "本地改动".to_string();
        env.workspace.save("race-a", &m, "本地改动正文\n", None).unwrap();
        engine.push_article("race-a", "重试", false).unwrap();

        assert_ne!(env.remote_head("writing").unwrap(), advanced);
        assert!(env
            .remote_file_direct("writing", "src/content/blog/race-c.md")
            .is_some());
        assert!(env
            .remote_file_direct("writing", "src/content/blog/race-a.md")
            .unwrap()
            .contains("本地改动正文"));
    }

    /// 远端图片变化构成图片冲突，且不假称可文本合并。
    #[test]
    fn image_conflict_is_reported_separately() {
        let env = TestEnv::new();
        crate::images::import_image_bytes(env.path(), "img-conflict", "fig", &tiny_png()).unwrap();
        let images = crate::images::article_images(env.path(), "img-conflict").unwrap();
        let image_rel = images[0].rel_path.clone();
        env.workspace
            .create("img-conflict", &meta("图片冲突", true), "正文\n")
            .unwrap();
        let engine = engine(&env);
        engine.push_article("img-conflict", "带图初版", false).unwrap();

        // 他处设备替换图片内容。
        let other = env.other_device("other-img");
        other.fetch("writing");
        other.checkout_new_branch("writing", "origin/writing");
        let abs = other.path.join(&image_rel);
        std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
        std::fs::write(&abs, crate::testkit::other_png()).unwrap();
        other.git(&["add", "-A", "--", &image_rel]);
        other.git(&["commit", "-m", "替换图片"]);
        other.push("writing");

        // 本地也替换图片。
        let local_abs = env.path().join(&image_rel);
        std::fs::write(&local_abs, crate::testkit::other_png()).unwrap();
        // 再造一次本地改动，确保与远端不同。
        let mut local_variant = crate::testkit::other_png();
        local_variant.push(7);
        std::fs::write(&local_abs, &local_variant).unwrap();

        let assessment = engine.assess("img-conflict").unwrap();
        assert_eq!(assessment.decision, SyncDecision::RemoteChanged);
        assert!(
            assessment
                .conflicts
                .iter()
                .any(|c| matches!(c, ConflictSource::Image { rel_path } if rel_path == &image_rel)),
            "图片冲突需单独报告：{:?}",
            assessment.conflicts
        );
    }

    /// 校验 origin 不一致时拒绝写操作（避免误推到其它仓库）。
    #[test]
    fn origin_mismatch_blocks_writes() {
        let env = TestEnv::new();
        env.workspace.create("guard-a", &meta("守卫", true), "正文\n").unwrap();
        let engine = SyncEngine::new(
            &env.workspace,
            &env.store,
            "https://github.com/someone/other-repo.git",
        );
        let err = engine.assess("guard-a").unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidArgument);
        assert!(engine.push_article("guard-a", "x", false).is_err());
    }

    /// 断网/远端不可达时给出可操作错误，本地内容不丢。
    #[test]
    fn offline_remote_reports_actionable_error() {
        let env = TestEnv::new();
        env.workspace.create("offline-a", &meta("离线", true), "正文\n").unwrap();
        // 把 origin 指向不存在的路径来模拟不可达（同时保持 URL 与配置一致）。
        let bogus = env.dir.path().join("does-not-exist.git");
        env.git(&["remote", "set-url", "origin", &format!("file://{}", bogus.to_string_lossy().replace('\\', "/"))]);
        let engine = SyncEngine::new(
            &env.workspace,
            &env.store,
            &format!("file://{}", bogus.to_string_lossy().replace('\\', "/")),
        );
        let err = engine.assess("offline-a").unwrap_err();
        assert!(
            matches!(err.code, ErrorCode::Offline | ErrorCode::GitFailed),
            "不可达应给出离线或 Git 错误，实际 {:?}",
            err.code
        );
        // 本地文件保持可读。
        assert_eq!(env.workspace.read("offline-a", &Default::default()).unwrap().body, "正文\n");
    }

    /// 同步后远端文件内容与本地逐字节一致（含中文与 front matter 原样）。
    #[test]
    fn synced_content_is_byte_identical() {
        let env = TestEnv::new();
        let original = sample_markdown_with_body("中文标题：测试", true, "第一段。\n\n## 小标题\n\n- 项目一\n- 项目二\n");
        std::fs::write(env.path().join("src/content/blog/bytes-a.md"), &original).unwrap();
        let engine = engine(&env);
        engine.push_article("bytes-a", "字节一致性", false).unwrap();

        let remote = env.remote_file_direct("writing", "src/content/blog/bytes-a.md").unwrap();
        assert_eq!(remote, original, "同步不得改动 Markdown 字节");
    }
}

