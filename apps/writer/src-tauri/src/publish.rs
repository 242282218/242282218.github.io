//! 按篇发布：把一篇已同步到 `writing` 的文章（及其图片）单独发布到 `main`。
//!
//! 关键不变量：
//! - 只覆盖**该文章路径**及其新增/变更/已确认不再引用的专属图片；
//! - 绝不合并整条 `writing`，也不 cherry-pick 含多篇文章的同步提交；
//! - 构造提交前后都用 `git diff --name-only` 核对变更清单在允许范围内；
//! - 普通推送到 `main`；若其间主分支移动，则基于最新树重新构造；
//! - 「已提交发布」与「网站已上线」分开表达，部署结论必须来自工作流查询。

use crate::git;
use crate::images;
use crate::local_store::{ArticleBaseline, LocalStore};
use crate::model::{ErrorCode, Result, WriterError};
use crate::paths;
use crate::sync::{SyncEngine, MAIN_BRANCH, WRITING_BRANCH};
use crate::util;
use crate::workspace::Workspace;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// 发布候选。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishCandidate {
    pub article_id: String,
    /// 将被写入 `main` 的 Markdown 全文（`draft: false`）。
    pub markdown: String,
    /// 作为来源的写作分支内容哈希（含其 `draft` 值）。
    pub source_writing_hash: String,
    /// 需要写入 `main` 的图片相对路径。
    pub image_paths: Vec<String>,
    /// 需要从 `main` 移除的旧图片（已确认不再被任何文章引用）。
    pub removed_image_paths: Vec<String>,
    /// 是否需要在 `main` 上删除旧路径（改 URL 后的再发布）。
    pub remove_old_markdown_path: Option<String>,
}

/// 发布前的预检结果，供界面展示四个可核对事实。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishPrecheck {
    pub article_id: String,
    pub title: String,
    pub image_file_count: usize,
    pub image_total_bytes: u64,
    pub target_repo: String,
    pub target_branch: String,
    pub main_head: String,
    /// `main` 上现有内容的哈希（无则为 None）。
    pub main_hash: Option<String>,
    pub writing_hash: String,
    /// 与在线版本是否有差异。
    pub differs_from_online: bool,
}

/// 发布结果。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishOutcome {
    pub commit: String,
    pub main_hash: String,
    /// 实际写入的变更清单（已通过白名单校验）。
    pub changed_paths: Vec<String>,
}

/// 发布或撤下之后，界面需要区分的状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DeployState {
    /// 已推送到 main，但尚无部署结论。
    Submitted,
    Deploying,
    Live,
    Failed,
}

/// 发布引擎。
pub struct PublishEngine<'w, 's> {
    workspace: &'w Workspace,
    store: &'s LocalStore,
    sync: SyncEngine<'w, 's>,
    expected_remote: String,
    /// 是否在发布前运行站点构建验收（方案 §5.4 第 4 步）。
    ///
    /// **生产构建恒为 `true`**：发布前必须在隔离副本上跑通
    /// `pnpm test` / `pnpm check` / `pnpm build`，失败即停止发布。
    ///
    /// 单元测试默认关闭：测试夹具是极简站点，没有也不应联网安装 `node_modules`，
    /// 否则每个发布相关用例都会退化成一个真实 Astro 构建。需要验证这道闸门
    /// 本身的用例用 [`PublishEngine::with_build_check`] 显式打开。
    verify_build: bool,
}

impl<'w, 's> PublishEngine<'w, 's> {
    pub fn new(
        workspace: &'w Workspace,
        store: &'s LocalStore,
        expected_remote: &str,
    ) -> Self {
        Self {
            workspace,
            store,
            sync: SyncEngine::new(workspace, store, expected_remote),
            expected_remote: expected_remote.to_string(),
            verify_build: !cfg!(test),
        }
    }

    /// 显式要求发布前运行站点构建验收（供测试验证闸门本身）。
    #[cfg(test)]
    pub fn with_build_check(mut self) -> Self {
        self.verify_build = true;
        self
    }

    /// 发布前预检：核对当前文章、图片数、目标仓库与分支、与在线版本的差异。
    ///
    /// 若本地尚未成功同步到 `writing`，这里直接报错要求先同步——
    /// 没有成功保存到 `writing` 不进入发布。
    pub fn precheck(&self, article_id: &str) -> Result<PublishPrecheck> {
        self.sync.verify_origin()?;
        paths::validate_existing_article_id(article_id)?;

        let assessment = self.sync.assess(article_id)?;
        let baseline = self.store.load_versions().get(article_id).cloned().unwrap_or_default();

        let local = self.workspace.read(article_id, &BTreeMap::new())?;
        let images_list = images::article_images(self.workspace.root(), article_id)?;
        let image_total_bytes: u64 = images_list.iter().map(|i| i.size).sum();

        let writing_head = self.sync.fetch(WRITING_BRANCH)?;
        let writing_markdown = match (&writing_head, &assessment.writing_hash) {
            (Some(rev), Some(_)) => self.sync.markdown_at(rev, article_id)?,
            _ => None,
        };
        // 必须存在已成功同步的写作快照。
        let writing_hash = match (&writing_markdown, &baseline.writing_hash) {
            (Some(text), known) => {
                let hash = util::hash_bytes(text.as_bytes());
                if known.as_deref() != Some(hash.as_str()) {
                    return Err(WriterError::new(
                        ErrorCode::RemoteChanged,
                        "写作分支上的内容与上次成功同步的记录不一致，请先重新同步再发布",
                    ));
                }
                hash
            }
            (None, _) => {
                return Err(WriterError::new(
                    ErrorCode::RemoteChanged,
                    "这篇文章尚未成功保存到写作分支，请先执行「同步到写作分支」",
                ))
            }
        };

        let main_head = self.sync.fetch(MAIN_BRANCH)?.ok_or_else(|| {
            WriterError::new(ErrorCode::GitFailed, "远端缺少 main 分支，无法发布")
        })?;
        let main_markdown = self.sync.markdown_at(&main_head, article_id)?;
        let main_hash = main_markdown.as_ref().map(|t| util::hash_bytes(t.as_bytes()));

        // 与在线版本比较时忽略 draft 字段差异：发布的正是把 draft 置为 false。
        let published = self.publishable_markdown(&local.meta, &local.body)?;
        let differs_from_online = match &main_markdown {
            None => true,
            Some(text) => text.trim_end() != published.trim_end(),
        };

        Ok(PublishPrecheck {
            article_id: article_id.to_string(),
            title: local.meta.title,
            image_file_count: images_list.len(),
            image_total_bytes,
            target_repo: self.expected_remote.clone(),
            target_branch: MAIN_BRANCH.to_string(),
            main_head,
            main_hash,
            writing_hash,
            differs_from_online,
        })
    }

    /// 把元数据渲染为「发布版」Markdown：`draft: false`，其余字段保持不变。
    fn publishable_markdown(
        &self,
        meta: &crate::model::ArticleMeta,
        body: &str,
    ) -> Result<String> {
        let mut published = meta.clone();
        published.draft = false;
        // 沿用文章的字段顺序与未知字段：以本地文件为模板改 draft。
        let text = crate::article_io::compose_markdown(&published, body);
        Ok(text)
    }

    /// 构造发布候选：从**已验证的写作快照**提取该篇 Markdown 与图片。
    ///
    /// 使用写作分支上的原文（而不是本地未同步内容），保证发布的正是已验证版本。
    pub fn build_candidate(&self, article_id: &str) -> Result<PublishCandidate> {
        paths::validate_existing_article_id(article_id)?;
        let writing_head = self.sync.fetch(WRITING_BRANCH)?.ok_or_else(|| {
            WriterError::new(ErrorCode::RemoteChanged, "写作分支不存在，请先同步")
        })?;
        let source = self.sync.markdown_at(&writing_head, article_id)?.ok_or_else(|| {
            WriterError::new(ErrorCode::RemoteChanged, "写作分支上没有这篇文章，请先同步")
        })?;

        // 主站文章设 draft: false，其余 front matter 与正文保持原样。
        let parsed = crate::article_io::parse_markdown(&source)?;
        let map = crate::article_io::parse_front_matter_map(&parsed.front_matter)?;
        let meta = crate::article_io::meta_from_map(&map)?;
        let mut published_meta = meta;
        published_meta.draft = false;
        let front_matter =
            crate::article_io::apply_meta(&parsed.front_matter, &parsed.fm_newline, &published_meta, None);
        let markdown = parsed.render_with(&front_matter, &parsed.body);

        // 图片取自写作分支上该文章的专属目录。
        let writing_images = self.sync.image_hashes_at(&writing_head, article_id)?;
        let image_paths: Vec<String> = writing_images.keys().cloned().collect();

        Ok(PublishCandidate {
            article_id: article_id.to_string(),
            markdown,
            source_writing_hash: util::hash_bytes(source.as_bytes()),
            image_paths,
            removed_image_paths: Vec::new(),
            remove_old_markdown_path: None,
        })
    }

    /// 执行按篇发布。
    ///
    /// `removed_image_paths` 与 `remove_old_markdown_path` 由上层在确认安全后传入。
    pub fn publish(
        &self,
        article_id: &str,
        commit_message: &str,
        removed_image_paths: &[String],
        remove_old_markdown_path: Option<&str>,
    ) -> Result<PublishOutcome> {
        self.sync.verify_origin()?;
        paths::validate_existing_article_id(article_id)?;

        // 发布前先确认已成功同步；未同步不进入发布。
        let _ = self.precheck(article_id)?;

        let mut candidate = self.build_candidate(article_id)?;
        candidate.removed_image_paths = removed_image_paths.to_vec();
        candidate.remove_old_markdown_path = remove_old_markdown_path.map(str::to_string);

        // 发布前最后一次构建验收：在隔离副本上跑站点命令，通过后才推 main。
        if self.verify_build {
            self.verify_site_build(&candidate)?;
        }

        self.push_candidate(&candidate, commit_message)
    }

    /// 发布前站点构建验收（方案 §5.4 第 4 步）。
    ///
    /// 在应用专属临时目录中以最新 `main` 为基线建立隔离副本，覆盖本篇候选内容，
    /// 运行 `pnpm test` / `pnpm check` / `pnpm build`；失败即停止发布。
    ///
    /// 缺少 Node/pnpm 或依赖时**不静默放行**：明确报出缺项，让用户先补齐前置条件
    /// 或显式跳过（当前实现为拒绝发布，宁可保守）。
    fn verify_site_build(&self, candidate: &PublishCandidate) -> Result<()> {
        let overlay = crate::preview::PreviewOverlay {
            article_id: candidate.article_id.clone(),
            markdown: candidate.markdown.clone(),
            images: candidate
                .image_paths
                .iter()
                .map(|rel| {
                    let abs = self.workspace.root().join(rel);
                    std::fs::read(&abs).map(|bytes| (rel.clone(), bytes)).map_err(|e| {
                        WriterError::new(
                            ErrorCode::IoFailed,
                            format!("读取待发布图片失败：{e}"),
                        )
                    })
                })
                .collect::<Result<BTreeMap<String, Vec<u8>>>>()?,
            // 发布版本就是 draft: false，不需要在副本中模拟公开。
            simulate_public: true,
        };

        let engine = crate::preview::PreviewEngine::new(
            self.workspace,
            self.store.preview_dir(),
            &self.expected_remote,
        );
        let worktree = engine.prepare_worktree("publish-check")?;
        // 无论验收成功与否都清理本次创建的临时副本。
        let outcome = (|| -> Result<()> {
            engine.apply_overlay(&worktree, &overlay)?;
            // 尽力复用工作区已安装的依赖（目录联接，不复制、不联网）。
            //
            // **不**在闸门里自动 `pnpm install`：发布路径不应默默访问网络。
            // 工作区没有依赖时脚本会自行失败，闸门如实报错并停止发布——这比
            // 悄悄下载依赖更可预期。
            let _ = engine.link_dependencies(&worktree);
            engine.run_site_checks(&worktree)
        })();
        let _ = crate::util::remove_dir_all_no_follow(&worktree);
        outcome
    }

    /// 在隔离索引中基于最新 `main` 构造并推送单文章提交。
    fn push_candidate(
        &self,
        candidate: &PublishCandidate,
        commit_message: &str,
    ) -> Result<PublishOutcome> {
        let markdown_rel = format!("{}{}.md", paths::BLOG_DIR_PREFIX, candidate.article_id);

        // 组装本次要写入 `main` 的全部路径，并先行白名单校验。
        let mut write_paths: Vec<String> = vec![markdown_rel.clone()];
        write_paths.extend(candidate.image_paths.iter().cloned());
        let mut remove_paths: Vec<String> = candidate.removed_image_paths.clone();
        if let Some(old) = &candidate.remove_old_markdown_path {
            remove_paths.push(old.clone());
        }
        for rel in write_paths.iter().chain(remove_paths.iter()) {
            paths::validate_managed_rel_path(rel)?;
        }
        match crate::sync::all_paths_allowed(&write_paths) {
            Ok(()) => {}
            Err(offenders) => {
                return Err(WriterError::new(
                    ErrorCode::GitFailed,
                    "发布候选包含受管目录之外的文件，已停止发布",
                )
                .with_detail(offenders.join("、")))
            }
        }

        let main_head = self.sync.fetch(MAIN_BRANCH)?.ok_or_else(|| {
            WriterError::new(ErrorCode::GitFailed, "远端缺少 main 分支，无法发布")
        })?;

        let index_file = git::temp_path("publish-index");
        let index_env = index_file.to_string_lossy().into_owned();
        let result = (|| -> Result<(String, Vec<String>)> {
            // 以最新 main 的树为起点，只叠加这一篇的改动。
            git::git_with_env(
                self.workspace.root(),
                &["read-tree", &main_head],
                &[("GIT_INDEX_FILE", index_env.clone())],
            )?;

            // 关键：把工作树中的候选内容读入隔离索引，而不是先改写工作树。
            // 通过 `git hash-object -w` 写入 blob，再 `update-index` 挂到路径上，
            // 这样主工作树与当前分支完全不受影响。
            let markdown_blob = self.write_blob(candidate.markdown.as_bytes())?;
            git::git_with_env(
                self.workspace.root(),
                &["update-index", "--add", "--cacheinfo", "100644", &markdown_blob, &markdown_rel],
                &[("GIT_INDEX_FILE", index_env.clone())],
            )?;

            for rel in &candidate.image_paths {
                let abs = self.workspace.root().join(rel);
                let bytes = std::fs::read(&abs).map_err(|e| {
                    WriterError::new(ErrorCode::IoFailed, format!("读取待发布图片失败：{e}"))
                })?;
                // 图片必须仍属于这篇且经文件头确认。
                if util::ImageKind::from_magic(&bytes).is_none() {
                    return Err(WriterError::new(
                        ErrorCode::ImageUnsupported,
                        "待发布的图片未通过文件头校验，已停止发布",
                    ));
                }
                let blob = self.write_blob(&bytes)?;
                git::git_with_env(
                    self.workspace.root(),
                    &["update-index", "--add", "--cacheinfo", "100644", &blob, rel],
                    &[("GIT_INDEX_FILE", index_env.clone())],
                )?;
            }

            for rel in candidate
                .removed_image_paths
                .iter()
                .chain(candidate.remove_old_markdown_path.iter())
            {
                // 从索引中移除；文件不存在时 git 会报错，容错处理。
                let _ = git::git_with_env(
                    self.workspace.root(),
                    &["update-index", "--force-remove", rel],
                    &[("GIT_INDEX_FILE", index_env.clone())],
                );
            }

            let tree = git::git_with_env(
                self.workspace.root(),
                &["write-tree"],
                &[("GIT_INDEX_FILE", index_env.clone())],
            )?
            .stdout_trimmed()
            .to_string();

            // 差异白名单核对：本次提交相对 main 的变更必须全部在允许范围。
            let changed = self.diff_name_only_between(&main_head, &tree)?;
            match crate::sync::all_paths_allowed(&changed) {
                Ok(()) => {}
                Err(offenders) => {
                    return Err(WriterError::new(
                        ErrorCode::GitFailed,
                        "本次发布将改动受管目录之外的文件，已停止发布",
                    )
                    .with_detail(offenders.join("、")))
                }
            }
            // 变更必须只涉及这一篇及其图片，不能带出别的文章。
            for rel in &changed {
                if !rel.starts_with(&format!("{}{}", paths::IMAGE_DIR_PREFIX, candidate.article_id))
                    && rel != &markdown_rel
                {
                    return Err(WriterError::new(
                        ErrorCode::GitFailed,
                        "本次发布将带出其他文章或图片，已停止发布",
                    )
                    .with_detail(rel.clone()));
                }
            }

            let message =
                format!("{commit_message}\n\nArticle: {}", candidate.article_id);
            let commit = git::git(
                self.workspace.root(),
                &["commit-tree", &tree, "-p", &main_head, "-m", &message],
            )?
            .stdout_trimmed()
            .to_string();
            Ok((commit, changed))
        })();

        let _ = std::fs::remove_file(&index_file);
        let (commit, changed_paths) = result?;

        // 推送前再次核头，确保仍是快进。
        let current_main = self.sync.fetch(MAIN_BRANCH)?;
        if current_main.as_deref() != Some(main_head.as_str()) {
            return Err(WriterError::new(
                ErrorCode::PushRejected,
                "推送前发现 main 已更新，已停止发布。请重试以基于最新版本重新构造提交",
            ));
        }

        let refspec = format!("{commit}:refs/heads/{MAIN_BRANCH}");
        git::git(self.workspace.root(), &["push", "origin", &refspec])?;

        let verified = self.sync.fetch(MAIN_BRANCH)?;
        if verified.as_deref() != Some(commit.as_str()) {
            return Err(WriterError::new(
                ErrorCode::GitFailed,
                "推送后未能确认远端 main 指向本次提交，请到仓库确认后再决定是否重试",
            ));
        }

        let published_markdown_hash = util::hash_bytes(candidate.markdown.as_bytes());
        let image_hashes: BTreeMap<String, String> = candidate
            .image_paths
            .iter()
            .filter_map(|rel| {
                std::fs::read(self.workspace.root().join(rel))
                    .ok()
                    .map(|bytes| (rel.clone(), util::hash_bytes(&bytes)))
            })
            .collect();

        let baseline_hash = published_markdown_hash.clone();
        let writing_source_hash = candidate.source_writing_hash.clone();
        let writing_image_hashes = image_hashes.clone();
        self.store.update_baseline(&candidate.article_id, |b: &mut ArticleBaseline| {
            b.main_commit = Some(commit.clone());
            b.main_hash = Some(baseline_hash.clone());
            b.main_published = Some(true);
            b.main_image_hashes = image_hashes.clone();
            // `writing` 基线跟踪的是写作分支内容（含其 `draft` 值），
            // 不能被发布版哈希覆盖，否则下一次同步会误判远端已变化。
            b.writing_hash = Some(writing_source_hash.clone());
            b.writing_image_hashes = writing_image_hashes.clone();
            b.edit_base_hash = Some(writing_source_hash.clone());
            // 新一次发布尚无部署结论：清空旧结论，避免上一次的「已上线」
            // 被错误地沿用到这次的新提交上。
            b.deployment_url = None;
            b.deployed_commit = None;
            b.deploy_failed_commit = None;
            b.deploying_commit = None;
        })?;

        Ok(PublishOutcome { commit, main_hash: published_markdown_hash, changed_paths })
    }

    /// 把字节写入 Git 对象库并返回 blob 的 SHA。
    fn write_blob(&self, bytes: &[u8]) -> Result<String> {
        write_blob_helper(self.workspace, bytes)
    }

    /// 列出两个树对象之间变更的文件路径。
    fn diff_name_only_between(&self, from_tree: &str, to_tree: &str) -> Result<Vec<String>> {
        let out = git::git(
            self.workspace.root(),
            &["diff", "--name-only", "-z", from_tree, to_tree],
        )?;
        Ok(out
            .stdout
            .split('\0')
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect())
    }

    /// 判定部署状态。
    ///
    /// 只有工作流明确成功才算「网站已上线」；查询不到时返回 `Submitted`
    /// （界面显示「部署状态待确认」并提供运行链接），绝不靠推送成功猜测。
    pub fn deploy_state(
        &self,
        article_id: &str,
        workflow_conclusion: Option<WorkflowConclusion>,
    ) -> Result<DeployState> {
        let baseline = self.store.load_versions().get(article_id).cloned().unwrap_or_default();
        let Some(main_commit) = baseline.main_commit else {
            return Ok(DeployState::Submitted);
        };
        let state = match workflow_conclusion {
            None => DeployState::Submitted,
            Some(conclusion) => match conclusion {
                WorkflowConclusion::Success { head_sha } => {
                    if head_sha.as_deref() == Some(main_commit.as_str()) {
                        DeployState::Live
                    } else {
                        // 工作流成功但对应的是别的提交。
                        DeployState::Submitted
                    }
                }
                WorkflowConclusion::InProgress { .. } => DeployState::Deploying,
                WorkflowConclusion::Failure { head_sha } => {
                    if head_sha.as_deref() == Some(main_commit.as_str()) {
                        DeployState::Failed
                    } else {
                        DeployState::Submitted
                    }
                }
            },
        };
        Ok(state)
    }

    /// 记录一次已确认的部署结论。
    ///
    /// 结论按**工作流运行对应的 `main` 提交**（`head_sha`）存储；界面用它与
    /// 当前 `main` 头比较，只有一致时对应状态才成立。工作流未给出具体提交时
    /// 按本机记录的发布提交归属，避免结论悬空。
    pub fn record_deploy_conclusion(
        &self,
        article_id: &str,
        conclusion: &WorkflowConclusion,
        run_url: Option<&str>,
    ) -> Result<()> {
        let baseline = self.store.load_versions().get(article_id).cloned().unwrap_or_default();
        let fallback = baseline.main_commit.clone();
        let resolve = move |head_sha: &Option<String>| head_sha.clone().or_else(|| fallback.clone());
        self.store.update_baseline(article_id, move |b: &mut ArticleBaseline| {
            // 每次记录都先清空三类结论，保证只有一个「当前」结论生效。
            b.deployed_commit = None;
            b.deploy_failed_commit = None;
            b.deploying_commit = None;
            match conclusion {
                WorkflowConclusion::Success { head_sha } => {
                    b.deployed_commit = resolve(head_sha);
                    b.deployment_url = run_url.map(str::to_string);
                }
                WorkflowConclusion::Failure { head_sha } => {
                    // 保留 main_commit，明确表达「已提交但未上线」。
                    b.deploy_failed_commit = resolve(head_sha);
                    b.deployment_url = run_url.map(str::to_string);
                }
                WorkflowConclusion::InProgress { head_sha } => {
                    b.deploying_commit = resolve(head_sha);
                    b.deployment_url = run_url.map(str::to_string);
                }
            }
        })?;
        Ok(())
    }

    /// 读取某个提交上的文章内容（用于解析冲突或撤下判定）。
    pub fn markdown_on_main(&self, article_id: &str) -> Result<Option<String>> {
        let Some(rev) = self.sync.fetch(MAIN_BRANCH)? else {
            return Ok(None);
        };
        self.sync.markdown_at(&rev, article_id)
    }

    /// 判断 `main` 上这篇文章当前是否公开。
    pub fn main_published_state(&self, article_id: &str) -> Result<Option<bool>> {
        let Some(text) = self.markdown_on_main(article_id)? else {
            return Ok(None);
        };
        let parsed = crate::article_io::parse_markdown(&text)?;
        let map = crate::article_io::parse_front_matter_map(&parsed.front_matter)?;
        let meta = crate::article_io::meta_from_map(&map)?;
        Ok(Some(!meta.draft))
    }

    /// 检查 `main` 是否包含指定路径（供删除前确认）。
    pub fn main_has_path(&self, rel_path: &str) -> Result<bool> {
        paths::validate_managed_rel_path(rel_path)?;
        let Some(rev) = self.sync.fetch(MAIN_BRANCH)? else {
            return Ok(false);
        };
        Ok(git::show_file(self.workspace.root(), &rev, rel_path)?.is_some())
    }

    /// 工作树是否存在未提交的受管改动。
    ///
    /// 这是发布前的诊断信息，**不是**发布闸门：隔离索引构造的提交不依赖工作树
    /// 是否干净。返回是否有未提交改动。
    pub fn managed_worktree_dirty(&self) -> Result<bool> {
        let dirty = git::status_porcelain(self.workspace.root(), None)?;
        Ok(!dirty.is_empty())
    }
}

/// 工作流查询结论。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum WorkflowConclusion {
    Success { head_sha: Option<String> },
    InProgress { head_sha: Option<String> },
    Failure { head_sha: Option<String> },
}

impl WorkflowConclusion {
    /// 该结论对应的提交（工作流运行的 `head_sha`）。
    pub fn head_sha(&self) -> Option<&str> {
        match self {
            Self::Success { head_sha } | Self::InProgress { head_sha } | Self::Failure { head_sha } => {
                head_sha.as_deref()
            }
        }
    }

    /// 从 GitHub Actions 工作流运行 JSON 解析结论。
    ///
    /// 只读取非敏感字段；`conclusion`/`status` 为空时视为进行中。
    pub fn from_workflow_run(json: &serde_json::Value) -> Option<Self> {
        let run = json.get("workflow_runs")?.as_array()?.first()?;
        let head_sha = run.get("head_sha").and_then(|v| v.as_str()).map(str::to_string);
        let status = run.get("status").and_then(|v| v.as_str()).unwrap_or("");
        let conclusion = run.get("conclusion").and_then(|v| v.as_str());
        Some(match (status, conclusion) {
            (_, Some("success")) => Self::Success { head_sha },
            (_, Some("failure")) | (_, Some("timed_out")) | (_, Some("startup_failure")) => {
                Self::Failure { head_sha }
            }
            ("completed", None) => Self::Failure { head_sha },
            _ => Self::InProgress { head_sha },
        })
    }
}

/// 提交信息前缀，便于在 Git 历史中辨认软件产生的提交。
pub fn publish_commit_message(title: &str) -> String {
    format!("发布文章：{title}")
}

/// 把字节写入工作区 Git 对象库并返回 blob 的 SHA。
///
/// 供发布与删除的隔离索引构造复用：内容直接进对象库，不改动工作树。
pub fn write_blob_helper(workspace: &Workspace, bytes: &[u8]) -> Result<String> {
    use std::io::Write;
    use std::process::Stdio;
    // stdin 必须保留管道：blob 内容经 stdin 传给 `hash-object -w --stdin`。
    let mut child = crate::util::program_command("git")
        .current_dir(workspace.root())
        .args(["hash-object", "-w", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| WriterError::new(ErrorCode::ToolchainMissing, "未找到 git 可执行文件"))?;
    if let Some(stdin) = child.stdin.as_mut() {
        stdin
            .write_all(bytes)
            .map_err(|e| WriterError::new(ErrorCode::IoFailed, format!("写入 Git 对象失败：{e}")))?;
    }
    let out = child
        .wait_with_output()
        .map_err(|e| WriterError::new(ErrorCode::GitFailed, format!("Git 写入对象失败：{e}")))?;
    if !out.status.success() {
        return Err(WriterError::new(ErrorCode::GitFailed, "Git 写入对象失败"));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// 撤下提交信息。
pub fn withdraw_commit_message(title: &str) -> String {
    format!("从网站撤下文章：{title}")
}

/// 删除提交信息。
pub fn delete_commit_message(title: &str) -> String {
    format!("删除文章：{title}")
}

/// 校验一组路径是否都属于某篇文章（含其图片目录）。
///
/// 判据：等于该文章的 Markdown 路径，或位于该文章的图片目录内。
/// 其它任何路径都返回 `false`（包括其他文章与其它受管目录）。
pub fn paths_belong_to_article(article_id: &str, rel_paths: &[String]) -> bool {
    let markdown = format!("{}{}.md", paths::BLOG_DIR_PREFIX, article_id);
    let image_dir = format!("{}{}/", paths::IMAGE_DIR_PREFIX, article_id);
    rel_paths.iter().all(|p| p == &markdown || p.starts_with(&image_dir))
}

/// 判断某路径是否是给定文章的 Markdown 文件。
pub fn is_article_markdown(article_id: &str, rel_path: &str) -> bool {
    rel_path == format!("{}{}.md", paths::BLOG_DIR_PREFIX, article_id)
}

/// 收集一组图片路径中被其他文章引用的那些（用于删除前保护）。
///
/// 只扫描**本地工作区**的受管 Markdown。远端分支上可能存在本地还没有的文章，
/// 因此调用方（删除）应优先使用 [`images_referenced_on_branches`]，把远端
/// 引用一并纳入；本函数保留给本地预览等只有本地视图的场景。
pub fn images_referenced_elsewhere(
    workspace: &Workspace,
    article_id: &str,
    image_rel_paths: &[String],
) -> Result<Vec<String>> {
    let texts = workspace.markdown_texts()?;
    protect_from_texts(article_id, image_rel_paths, &texts)
}

/// 收集一组图片路径中**被本地或任一远部分支上的其他文章引用**的那些。
///
/// 方案要求删除前逐篇扫描主站与写作分支**全部受管 Markdown**的引用，任何
/// 不确定的图片默认留下。远端分支上可能存在本地尚未拉取的文章，只扫本地会
/// 漏判并把共享图片当成独占图片删掉。
///
/// 远端读取失败时返回错误，由调用方按「默认保留」处理，而不是放行删除。
pub fn images_referenced_on_branches(
    workspace: &Workspace,
    sync: &crate::sync::SyncEngine<'_, '_>,
    article_id: &str,
    image_rel_paths: &[String],
) -> Result<Vec<String>> {
    let mut texts = workspace.markdown_texts()?;
    let mut seen: BTreeSet<String> = texts.iter().map(|(id, _)| id.clone()).collect();
    for branch in [crate::sync::WRITING_BRANCH, crate::sync::MAIN_BRANCH] {
        let Some(head) = sync.fetch(branch)? else {
            continue;
        };
        for rel in crate::git::list_tree(workspace.root(), &head, crate::paths::BLOG_DIR_PREFIX, ".md")?
        {
            let Ok(id) = paths::article_id_from_rel_path(&rel) else {
                continue;
            };
            // 本地已有该文章时以本地为准（本地是作者正在编辑的版本）。
            if !seen.insert(id.clone()) {
                continue;
            }
            if let Ok(Some(bytes)) = crate::git::show_file(workspace.root(), &head, &rel) {
                if let Ok(text) = String::from_utf8(bytes) {
                    texts.push((id, text));
                }
            }
        }
    }
    protect_from_texts(article_id, image_rel_paths, &texts)
}

fn protect_from_texts(
    article_id: &str,
    image_rel_paths: &[String],
    texts: &[(String, String)],
) -> Result<Vec<String>> {
    let mut protected = BTreeSet::new();
    for rel in image_rel_paths {
        let file_name = Path::new(rel).file_name().and_then(|n| n.to_str()).unwrap_or("");
        let url = images::url_path_for(rel)?;
        let owners = images::find_references_in_texts(texts, &url, file_name);
        // 只要存在本文章之外的引用者，就不是独占图片 → 必须保护。
        // 复用 `is_exclusively_owned` 保持与帮助函数同一判据。
        if !images::is_exclusively_owned(&owners, article_id) {
            protected.insert(rel.clone());
        }
    }
    Ok(protected.into_iter().collect())
}

#[cfg(test)]
mod integration {
    use super::*;
    use crate::testkit::{sample_markdown, sample_markdown_with_body, tiny_png, TestEnv};

    fn engine<'a>(env: &'a TestEnv) -> PublishEngine<'a, 'a> {
        PublishEngine::new(
            &env.workspace,
            &env.store,
            &format!("file://{}", env.remote.to_string_lossy().replace('\\', "/")),
        )
    }

    fn sync_engine<'a>(env: &'a TestEnv) -> SyncEngine<'a, 'a> {
        SyncEngine::new(
            &env.workspace,
            &env.store,
            &format!("file://{}", env.remote.to_string_lossy().replace('\\', "/")),
        )
    }

    /// A5：写作分支有 A、B 草稿，发布 A → main 只包含 A；B 仍未发布。
    #[test]
    fn a5_publishing_a_does_not_include_b() {
        let env = TestEnv::new();
        std::fs::write(
            env.path().join("src/content/blog/pub-a.md"),
            sample_markdown_with_body("文章 A", true, "A 的正文"),
        )
        .unwrap();
        std::fs::write(
            env.path().join("src/content/blog/pub-b.md"),
            sample_markdown_with_body("文章 B", true, "B 的正文"),
        )
        .unwrap();

        let se = sync_engine(&env);
        se.push_article("pub-a", "同步 A", false).unwrap();
        se.push_article("pub-b", "同步 B", false).unwrap();

        let pe = engine(&env);
        let precheck = pe.precheck("pub-a").unwrap();
        assert_eq!(precheck.title, "文章 A");
        assert!(precheck.differs_from_online);

        let outcome = pe.publish("pub-a", "发布 A", &[], None).unwrap();

        // main 上有 A 且 draft 已置为 false。
        let a_on_main = env
            .remote_file_direct("main", "src/content/blog/pub-a.md")
            .expect("A 应发布到 main");
        assert!(a_on_main.contains("draft: false"), "{a_on_main}");
        assert!(a_on_main.contains("A 的正文"));

        // main 上没有 B。
        assert!(
            env.remote_file_direct("main", "src/content/blog/pub-b.md").is_none(),
            "B 绝不进入 main"
        );

        // 变更清单只涉及 A。
        assert_eq!(outcome.changed_paths, vec!["src/content/blog/pub-a.md"]);
        // writing 上两篇都还在。
        assert!(env.remote_file_direct("writing", "src/content/blog/pub-b.md").is_some());
        assert!(env.remote_file_direct("writing", "src/content/blog/pub-a.md").is_some());
        // writing 上 A 仍是草稿。
        assert!(env
            .remote_file_direct("writing", "src/content/blog/pub-a.md")
            .unwrap()
            .contains("draft: true"));
    }

    /// 发布必须带出该文章的图片，且不带走其他文章的图片。
    #[test]
    fn publish_carries_only_its_own_images() {
        let env = TestEnv::new();
        // A 有图，B 也有图。
        crate::images::import_image_bytes(env.path(), "img-a", "a-fig", &tiny_png()).unwrap();
        crate::images::import_image_bytes(env.path(), "img-b", "b-fig", &tiny_png()).unwrap();
        let a_images = crate::images::article_images(env.path(), "img-a").unwrap();
        let a_url = crate::images::url_path_for(&a_images[0].rel_path).unwrap();
        std::fs::write(
            env.path().join("src/content/blog/img-a.md"),
            sample_markdown_with_body("图 A", true, &format!("![a]({a_url})")),
        )
        .unwrap();
        std::fs::write(
            env.path().join("src/content/blog/img-b.md"),
            sample_markdown("图 B", true),
        )
        .unwrap();

        let se = sync_engine(&env);
        se.push_article("img-a", "同步 A", false).unwrap();
        se.push_article("img-b", "同步 B", false).unwrap();

        engine(&env).publish("img-a", "发布 A", &[], None).unwrap();

        let main_a_images = env.remote_ls("main", "public/blog/img-a");
        assert_eq!(main_a_images.len(), 1, "A 的图片应发布：{main_a_images:?}");
        assert!(
            env.remote_ls("main", "public/blog/img-b").is_empty(),
            "B 的图片不得进入 main"
        );
    }

    /// 未同步到 writing 的文章不允许发布。
    #[test]
    fn publish_requires_prior_sync() {
        let env = TestEnv::new();
        std::fs::write(
            env.path().join("src/content/blog/unsynced.md"),
            sample_markdown("未同步", true),
        )
        .unwrap();

        let pe = engine(&env);
        let err = pe.precheck("unsynced").unwrap_err();
        assert_eq!(err.code, ErrorCode::RemoteChanged);
        assert!(err.message.contains("同步"));
        assert!(pe.publish("unsynced", "发布", &[], None).is_err());
        assert!(env.remote_file_direct("main", "src/content/blog/unsynced.md").is_none());
    }

    /// A6：工作流失败时显示「已提交但未上线」，不谎称已上线。
    #[test]
    fn a6_failed_deploy_reports_submitted_not_live() {
        let env = TestEnv::new();
        std::fs::write(env.path().join("src/content/blog/fail-a.md"), sample_markdown("失败文章", true))
            .unwrap();
        let se = sync_engine(&env);
        se.push_article("fail-a", "同步", false).unwrap();
        let pe = engine(&env);
        let outcome = pe.publish("fail-a", "发布", &[], None).unwrap();

        // 查询不到工作流 → 待确认，不是已上线。
        assert_eq!(pe.deploy_state("fail-a", None).unwrap(), DeployState::Submitted);

        // 工作流失败且对应本次提交 → 部署失败。
        let failure = WorkflowConclusion::Failure { head_sha: Some(outcome.commit.clone()) };
        assert_eq!(
            pe.deploy_state("fail-a", Some(failure.clone())).unwrap(),
            DeployState::Failed
        );
        pe.record_deploy_conclusion("fail-a", &failure, Some("https://example.invalid/run/1"))
            .unwrap();
        let baseline = env.store.load_versions().get("fail-a").cloned().unwrap();
        assert_eq!(baseline.main_commit.as_deref(), Some(outcome.commit.as_str()), "必须保留已提交事实");
        assert!(baseline.deployment_url.is_some(), "应保留运行记录链接");
        // 不允许被当作已上线：deployment_url 只在成功时作为站点地址语义使用，
        // 这里通过 deploy_state 再次确认。
        assert_ne!(pe.deploy_state("fail-a", Some(failure)).unwrap(), DeployState::Live);

        // 工作流成功且对应本次提交 → 已上线。
        let success = WorkflowConclusion::Success { head_sha: Some(outcome.commit.clone()) };
        assert_eq!(pe.deploy_state("fail-a", Some(success)).unwrap(), DeployState::Live);
    }

    /// 工作流成功但针对别的提交时不标记为已上线。
    #[test]
    fn deploy_success_for_other_commit_is_not_ours() {
        let env = TestEnv::new();
        std::fs::write(env.path().join("src/content/blog/x-a.md"), sample_markdown("X", true))
            .unwrap();
        let se = sync_engine(&env);
        se.push_article("x-a", "同步", false).unwrap();
        let pe = engine(&env);
        pe.publish("x-a", "发布", &[], None).unwrap();

        let other = WorkflowConclusion::Success { head_sha: Some("0".repeat(40)) };
        assert_eq!(pe.deploy_state("x-a", Some(other)).unwrap(), DeployState::Submitted);
    }

    /// 已发布文章的编辑期间，main 旧文件不改，直到确认发布。
    #[test]
    fn editing_published_article_keeps_old_main_until_republish() {
        let env = TestEnv::new();
        std::fs::write(env.path().join("src/content/blog/edit-a.md"), sample_markdown("可编辑", true))
            .unwrap();
        let se = sync_engine(&env);
        se.push_article("edit-a", "初同步", false).unwrap();
        let pe = engine(&env);
        pe.publish("edit-a", "首次发布", &[], None).unwrap();
        let main_after_publish = env
            .remote_file_direct("main", "src/content/blog/edit-a.md")
            .unwrap();

        // 本地改新稿并同步，但不发布。
        std::fs::write(
            env.path().join("src/content/blog/edit-a.md"),
            sample_markdown_with_body("可编辑", true, "新稿正文"),
        )
        .unwrap();
        se.push_article("edit-a", "同步新稿", false).unwrap();

        let main_now = env
            .remote_file_direct("main", "src/content/blog/edit-a.md")
            .unwrap();
        assert_eq!(main_now, main_after_publish, "未发布前 main 保持不变");
        assert!(!main_now.contains("新稿正文"));
    }

    /// 发布期间 main 被外部推进时，不允许强推旧树。
    #[test]
    fn concurrent_main_advance_blocks_stale_publish() {
        let env = TestEnv::new();
        std::fs::write(env.path().join("src/content/blog/race-p.md"), sample_markdown("竞态发布", true))
            .unwrap();
        let se = sync_engine(&env);
        se.push_article("race-p", "同步", false).unwrap();

        let pe = engine(&env);
        let precheck = pe.precheck("race-p").unwrap();
        assert_eq!(precheck.main_head, env.remote_head("main").unwrap());

        // 他处设备推进 main。
        let other = env.other_device("other-publish");
        other.fetch("main");
        other.checkout_new_branch("main", "origin/main");
        other.write_and_commit(
            "src/content/blog/other-pub.md",
            &sample_markdown("他处已发布", false),
            "他处发布",
        );
        other.push("main");

        // 重新从最新 main 构造发布：必须成功且保留他处内容。
        let outcome = pe.publish("race-p", "发布", &[], None).unwrap();
        assert!(env
            .remote_file_direct("main", "src/content/blog/other-pub.md")
            .is_some(), "他处已发布内容不得被覆盖");
        assert_eq!(env.remote_head("main").unwrap(), outcome.commit);
        // 父提交必须是他处推进后的 main 头（快进关系）。
        let parent = env.git(&["rev-parse", &format!("{}^", outcome.commit)]);
        assert_ne!(parent, precheck.main_head, "必须基于最新 main 构造");
    }

    /// 草稿不会因同步而出现在 main；发布后 main 才出现。
    #[test]
    fn draft_filtering_and_publication_transition() {
        let env = TestEnv::new();
        std::fs::write(env.path().join("src/content/blog/filter-a.md"), sample_markdown("过滤", true))
            .unwrap();
        let se = sync_engine(&env);
        se.push_article("filter-a", "同步草稿", false).unwrap();
        assert!(
            env.remote_file_direct("main", "src/content/blog/filter-a.md").is_none(),
            "草稿不得出现在 main"
        );

        engine(&env).publish("filter-a", "发布", &[], None).unwrap();
        let published = env.remote_file_direct("main", "src/content/blog/filter-a.md").unwrap();
        assert!(published.contains("draft: false"));
    }

    /// 工作流结论解析容忍缺字段。
    #[test]
    fn parses_workflow_conclusion_json() {
        let success = serde_json::json!({
            "workflow_runs": [{ "head_sha": "abc", "status": "completed", "conclusion": "success" }]
        });
        assert_eq!(
            WorkflowConclusion::from_workflow_run(&success),
            Some(WorkflowConclusion::Success { head_sha: Some("abc".to_string()) })
        );

        let running = serde_json::json!({
            "workflow_runs": [{ "head_sha": "abc", "status": "in_progress", "conclusion": null }]
        });
        assert!(matches!(
            WorkflowConclusion::from_workflow_run(&running),
            Some(WorkflowConclusion::InProgress { .. })
        ));

        let failed = serde_json::json!({
            "workflow_runs": [{ "head_sha": "abc", "status": "completed", "conclusion": "failure" }]
        });
        assert!(matches!(
            WorkflowConclusion::from_workflow_run(&failed),
            Some(WorkflowConclusion::Failure { .. })
        ));

        // 空运行列表。
        assert_eq!(WorkflowConclusion::from_workflow_run(&serde_json::json!({})), None);
        assert_eq!(
            WorkflowConclusion::from_workflow_run(&serde_json::json!({ "workflow_runs": [] })),
            None
        );
    }

    /// 保护被其他文章引用的图片。
    #[test]
    fn shared_images_are_protected_from_cleanup() {
        let env = TestEnv::new();
        crate::images::import_image_bytes(env.path(), "owner-a", "shared", &tiny_png()).unwrap();
        let images = crate::images::article_images(env.path(), "owner-a").unwrap();
        let url = crate::images::url_path_for(&images[0].rel_path).unwrap();

        // A 引用该图。
        std::fs::write(
            env.path().join("src/content/blog/owner-a.md"),
            sample_markdown_with_body("拥有者 A", true, &format!("![x]({url})")),
        )
        .unwrap();
        // B 也引用同一张图（跨文章引用）。
        std::fs::write(
            env.path().join("src/content/blog/owner-b.md"),
            sample_markdown_with_body("引用者 B", true, &format!("![x]({url})")),
        )
        .unwrap();

        let protected = images_referenced_elsewhere(
            &env.workspace,
            "owner-a",
            &[images[0].rel_path.clone()],
        )
        .unwrap();
        assert_eq!(protected, vec![images[0].rel_path.clone()], "被 B 引用的图必须受保护");

        // 只有 A 引用时可以清理。
        std::fs::write(
            env.path().join("src/content/blog/owner-b.md"),
            sample_markdown("引用者 B", true),
        )
        .unwrap();
        let unprotected = images_referenced_elsewhere(
            &env.workspace,
            "owner-a",
            &[images[0].rel_path.clone()],
        )
        .unwrap();
        assert!(unprotected.is_empty());
    }

    /// 提交信息辅助函数。
    #[test]
    fn commit_message_helpers_are_readable() {
        assert_eq!(publish_commit_message("第一篇文章"), "发布文章：第一篇文章");
        assert_eq!(withdraw_commit_message("A"), "从网站撤下文章：A");
        assert_eq!(delete_commit_message("A"), "删除文章：A");
    }

    /// 路径归属判断。
    #[test]
    fn path_ownership_checks() {
        assert!(is_article_markdown("a", "src/content/blog/a.md"));
        assert!(!is_article_markdown("a", "src/content/blog/b.md"));
        assert!(paths_belong_to_article(
            "a",
            &["src/content/blog/a.md".to_string(), "public/blog/a/f.png".to_string()]
        ));
        assert!(!paths_belong_to_article("a", &["src/pages/index.astro".to_string()]));
    }
}
