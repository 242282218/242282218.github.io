//! 撤下、删除、恢复与 URL 改名的状态编排。
//!
//! 核心事实：Git 的两个分支之间**没有跨分支原子事务**。因此：
//! - 执行前先记录带操作 ID 的本地意向与远端原头；
//! - 删除前必须先把正文与图片备份到本地回收区；
//! - 先做 `writing`，再做 `main`；任一步失败都保留回收副本并显示精确状态；
//! - 重试时读取最新远端头、确认已完成步骤并补剩余操作，幂等；
//! - 绝不把「只完成一半」显示成删除成功。

use crate::git;
use crate::images;
use crate::local_store::{
    ArticleBaseline, LocalStore, RecoveryDraft, TrashEntry, TrashImage, TrashKind,
};
use crate::model::{ErrorCode, Result, WriterError};
use crate::paths;
use crate::publish::PublishEngine;
use crate::sync::{SyncEngine, MAIN_BRANCH, WRITING_BRANCH};
use crate::util;
use crate::workspace::Workspace;
use std::collections::BTreeMap;

/// 撤下、删除与恢复的编排器。
pub struct TrashEngine<'w, 's> {
    workspace: &'w Workspace,
    store: &'s LocalStore,
    sync: SyncEngine<'w, 's>,
    publish: PublishEngine<'w, 's>,
}

/// 单篇删除前的安全评估，供界面展示具体影响。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteAssessment {
    pub article_id: String,
    pub title: String,
    pub markdown_rel_path: String,
    /// 将随文章删除的独占图片（无其他文章引用）。
    pub exclusive_images: Vec<String>,
    /// 因被其他文章引用而保留的图片。
    pub protected_images: Vec<String>,
    pub on_writing: bool,
    pub on_main: bool,
    pub was_published: bool,
    /// 两个分支当前的头（用于操作记录与幂等重试）。
    pub writing_head: Option<String>,
    pub main_head: Option<String>,
    /// 各分支上该文章 Markdown 的内容哈希（重试时核对是否被他人改写）。
    pub writing_markdown_hash: Option<String>,
    pub main_markdown_hash: Option<String>,
}

/// 撤下（不是删除）的结果。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WithdrawOutcome {
    pub op_id: String,
    pub writing_done: bool,
    pub main_done: bool,
    pub main_commit: Option<String>,
}

/// 删除 / 恢复的结果。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteOutcome {
    pub op_id: String,
    pub writing_done: bool,
    pub main_done: bool,
    pub removed_images: Vec<String>,
    pub kept_images: Vec<String>,
    /// 面向用户的分支完成状态文案。
    pub branch_state: String,
    /// 失败时保留的可重试信息。
    pub last_error: Option<String>,
}

impl DeleteOutcome {
    fn from_entry(entry: &TrashEntry) -> Self {
        Self {
            op_id: entry.op_id.clone(),
            writing_done: entry.writing_done,
            main_done: entry.main_done,
            removed_images: entry.images.iter().map(|i| i.rel_path.clone()).collect(),
            kept_images: Vec::new(),
            branch_state: entry.branch_state_text(),
            last_error: entry.last_error.clone(),
        }
    }
}

impl<'w, 's> TrashEngine<'w, 's> {
    pub fn new(
        workspace: &'w Workspace,
        store: &'s LocalStore,
        expected_remote: &str,
    ) -> Self {
        Self {
            workspace,
            store,
            sync: SyncEngine::new(workspace, store, expected_remote),
            publish: PublishEngine::new(workspace, store, expected_remote),
        }
    }

    /// 评估删除影响：路径、两分支存在情况、独占与受保护的图片。
    pub fn assess_delete(&self, article_id: &str) -> Result<DeleteAssessment> {
        self.sync.verify_origin()?;
        paths::validate_existing_article_id(article_id)?;

        let markdown_rel_path = format!("{}{}.md", paths::BLOG_DIR_PREFIX, article_id);
        paths::validate_managed_markdown(&markdown_rel_path)?;

        let local = self.workspace.read(article_id, &BTreeMap::new())?;
        let all_images: Vec<String> = images::article_images(self.workspace.root(), article_id)?
            .into_iter()
            .map(|i| i.rel_path)
            .collect();

        // 逐篇扫描**本地与两个远端分支上全部受管 Markdown**的引用，任何不确定
        // 的图片默认保留。远端读取失败时不放行删除，退化为「全部保留」。
        let protected = crate::publish::images_referenced_on_branches(
            self.workspace,
            &self.sync,
            article_id,
            &all_images,
        )
        .unwrap_or_else(|_| all_images.clone());
        let exclusive: Vec<String> =
            all_images.iter().filter(|p| !protected.contains(p)).cloned().collect();

        let writing_head = self.sync.fetch(WRITING_BRANCH)?;
        let writing_text = match &writing_head {
            Some(rev) => self.sync.markdown_at(rev, article_id)?,
            None => None,
        };
        let on_writing = writing_text.is_some();
        let main_head = self.sync.fetch(MAIN_BRANCH)?;
        let main_text = match &main_head {
            Some(rev) => self.sync.markdown_at(rev, article_id)?,
            None => None,
        };
        let on_main = main_text.is_some();
        let was_published = self.publish.main_published_state(article_id)?.unwrap_or(false);

        Ok(DeleteAssessment {
            article_id: article_id.to_string(),
            title: local.meta.title,
            markdown_rel_path,
            exclusive_images: exclusive,
            protected_images: protected,
            on_writing,
            on_main,
            was_published,
            writing_head,
            main_head,
            writing_markdown_hash: writing_text
                .as_deref()
                .map(|t| util::hash_bytes(t.as_bytes())),
            main_markdown_hash: main_text.as_deref().map(|t| util::hash_bytes(t.as_bytes())),
        })
    }

    /// 撤下：只把 `main` 上该篇设为 `draft: true`，写作分支的新稿保留。
    ///
    /// 推荐在主站保留草稿文件，便于再次发布。
    pub fn withdraw(&self, article_id: &str) -> Result<WithdrawOutcome> {
        self.sync.verify_origin()?;
        paths::validate_existing_article_id(article_id)?;
        let markdown_rel_path = format!("{}{}.md", paths::BLOG_DIR_PREFIX, article_id);

        let op_id = util::new_operation_id();
        let main_head = self.sync.fetch(MAIN_BRANCH)?.ok_or_else(|| {
            WriterError::new(ErrorCode::GitFailed, "远端缺少 main 分支，无法撤下")
        })?;
        let source = self.sync.markdown_at(&main_head, article_id)?.ok_or_else(|| {
            WriterError::new(ErrorCode::ArticleNotFound, "main 上不存在这篇文章，无需撤下")
        })?;

        // 生成撤下版本：仅 draft 置为 true。
        let parsed = crate::article_io::parse_markdown(&source)?;
        let mut new_lines: Vec<String> = parsed
            .front_matter
            .split('\n')
            .map(|l| l.strip_suffix('\r').unwrap_or(l).to_string())
            .collect();
        let draft_idx = new_lines.iter().position(|l| l.starts_with("draft:"));
        match draft_idx {
            Some(idx) => new_lines[idx] = "draft: true".to_string(),
            None => new_lines.push("draft: true".to_string()),
        }
        let front_matter = new_lines.join(&parsed.fm_newline);
        let withdrawn_text = parsed.render_with(&front_matter, &parsed.body);

        // 撤下不是删除：正文与图片都保留，也**不进回收区**（回收区只放可恢复的
        // 删除条目），因此这里不写 TrashEntry；状态由基线的 main_published 表达。
        let title = self
            .workspace
            .read(article_id, &BTreeMap::new())
            .map(|c| c.meta.title)
            .unwrap_or_else(|_| article_id.to_string());

        // 只改 main。
        let commit = self.push_single_path_change(
            MAIN_BRANCH,
            &main_head,
            &markdown_rel_path,
            withdrawn_text.as_bytes(),
            &crate::publish::withdraw_commit_message(&title),
        )?;

        self.store.update_baseline(article_id, |b: &mut ArticleBaseline| {
            b.main_commit = Some(commit.clone());
            b.main_published = Some(false);
        })?;

        Ok(WithdrawOutcome {
            op_id,
            writing_done: true,
            main_done: true,
            main_commit: Some(commit),
        })
    }

    /// 删除：先本地完整备份，再依次处理 `writing` 与 `main`。
    pub fn delete(&self, article_id: &str) -> Result<DeleteOutcome> {
        self.sync.verify_origin()?;
        let assessment = self.assess_delete(article_id)?;

        let op_id = util::new_operation_id();
        let entry_dir = self.store.trash_entry_dir(&op_id);

        // 1. 先备份正文、图片、元数据与来源版本。备份失败即中止，不进入删除。
        let report = self.workspace.backup_to_trash(&entry_dir, article_id)?;
        if !report.has_markdown {
            return Err(WriterError::new(
                ErrorCode::IoFailed,
                "未能备份文章正文，已中止删除",
            ));
        }

        // 内容基线：删除前该文章在两个分支上各自的内容哈希。重试时用来识别
        // 「远端同路径已被他人重建／改写」，避免旧操作误删新内容。
        let source_writing_markdown_hash = assessment.writing_markdown_hash.clone();
        let source_main_markdown_hash = assessment.main_markdown_hash.clone();

        let entry = TrashEntry {
            op_id: op_id.clone(),
            kind: TrashKind::Deleted,
            article_id: article_id.to_string(),
            title: assessment.title.clone(),
            deleted_at: util::today_local_date(),
            markdown_rel_path: assessment.markdown_rel_path.clone(),
            has_markdown: true,
            markdown_backup_name: Some(report.markdown_backup_name.clone()),
            images: report.images.clone(),
            // 只有确认无其他文章引用的图片才允许从远端删除；恢复仍用全部副本。
            exclusive_images: assessment.exclusive_images.clone(),
            source_writing_markdown_hash,
            source_main_markdown_hash,
            writing_done: false,
            main_done: false,
            was_published: assessment.was_published,
            source_writing_sha: assessment.writing_head.clone(),
            source_main_sha: assessment.main_head.clone(),
            last_error: None,
        };
        self.store.upsert_trash(entry.clone())?;

        let mut outcome = DeleteOutcome::from_entry(&entry);
        outcome.kept_images = assessment.protected_images.clone();

        // 2. writing 分支删除。
        let writing_done = match self.delete_paths_from_branch(
            WRITING_BRANCH,
            article_id,
            &assessment.markdown_rel_path,
            &assessment.exclusive_images,
            &assessment.title,
        ) {
            Ok(_) => true,
            Err(err) => {
                if err.code == ErrorCode::ArticleNotFound {
                    // 远端本来就没有，视为已完成。
                    true
                } else {
                    let entry = TrashEntry {
                        writing_done: false,
                        last_error: Some(format!("写作分支：{}", err.message)),
                        ..entry.clone()
                    };
                    self.store.upsert_trash(entry.clone())?;
                    outcome.last_error = entry.last_error.clone();
                    outcome.branch_state = entry.branch_state_text();
                    // 任何一步失败都保留回收副本并显示精确状态。
                    return Ok(outcome);
                }
            }
        };
        let entry = TrashEntry { writing_done, ..entry };
        self.store.upsert_trash(entry.clone())?;
        outcome.writing_done = true;
        outcome.branch_state = entry.branch_state_text();

        // 3. main 分支删除。
        let main_done = match self.delete_paths_from_branch(
            MAIN_BRANCH,
            article_id,
            &assessment.markdown_rel_path,
            &assessment.exclusive_images,
            &assessment.title,
        ) {
            Ok(_) => true,
            Err(err) => {
                if err.code == ErrorCode::ArticleNotFound {
                    true
                } else {
                    let entry = TrashEntry {
                        main_done: false,
                        last_error: Some(format!("网站分支：{}", err.message)),
                        ..entry.clone()
                    };
                    self.store.upsert_trash(entry.clone())?;
                    outcome.last_error = entry.last_error.clone();
                    outcome.branch_state = entry.branch_state_text();
                    return Ok(outcome);
                }
            }
        };

        // 4. 两个分支都成功后才删除本地正文；图片仅清理独占且无引用者。
        let entry = TrashEntry { main_done, ..entry };
        self.store.upsert_trash(entry.clone())?;
        outcome.main_done = true;
        outcome.branch_state = entry.branch_state_text();

        self.workspace.remove_markdown(article_id)?;
        for rel in &assessment.exclusive_images {
            images::remove_image(self.workspace.root(), rel)?;
        }
        self.store.update_baseline(article_id, |b: &mut ArticleBaseline| {
            b.writing_commit = None;
            b.writing_hash = None;
            b.main_commit = None;
            b.main_hash = None;
            b.main_published = None;
        })?;

        Ok(outcome)
    }

    /// 重试一次未完成的多分支删除。
    ///
    /// 会重新读取最新远端头，逐分支确认已完成步骤并**只补剩余操作**；
    /// 若远端该路径已被他人重建或改写，则停止并要求按新操作核对差异。
    pub fn retry_delete(&self, op_id: &str) -> Result<DeleteOutcome> {
        paths::validate_operation_id(op_id)?;
        self.sync.verify_origin()?;
        let entry = self
            .store
            .load_trash()
            .into_iter()
            .find(|e| e.op_id == op_id)
            .ok_or_else(|| WriterError::new(ErrorCode::InvalidArgument, "找不到该操作记录"))?;

        let article_id = entry.article_id.clone();
        paths::validate_existing_article_id(&article_id)?;
        let mut current = entry.clone();

        // 已经全部完成的操作不再重复执行：重复调用会把用户已经恢复出来的
        // 本地文件再删一次，也可能重复清理图片。
        if current.fully_done() {
            return Ok(DeleteOutcome::from_entry(&current));
        }

        // 安全核对：远端该路径若已被重建或改写，不能继续删除。
        // 判定依据是**该分支上文章内容的内容基线**，而不是「远端是否存在」——
        // 上次推送失败时远端仍是原文，属于可继续的正常情况；内容不一致才说明
        // 有人在这之后动了同一个路径，必须停下来按新操作核对。
        for (branch, done, baseline) in [
            (WRITING_BRANCH, current.writing_done, current.source_writing_markdown_hash.clone()),
            (MAIN_BRANCH, current.main_done, current.source_main_markdown_hash.clone()),
        ] {
            if done {
                continue;
            }
            let Some(rev) = self.sync.fetch(branch)? else {
                continue;
            };
            let Some(text) = self.sync.markdown_at(&rev, &article_id)? else {
                continue;
            };
            let remote_hash = util::hash_bytes(text.as_bytes());
            let matches_baseline = match baseline.as_deref() {
                Some(expected) => expected == remote_hash,
                // 未记录基线（旧格式条目）：无法判定，按可继续处理。
                None => true,
            };
            if !matches_baseline {
                return Err(WriterError::new(
                    ErrorCode::RemoteChanged,
                    format!(
                        "{} 上的同路径文章在本次删除之后发生了变化，已停止重试；请按一次新的删除操作重新核对差异",
                        branch
                    ),
                )
                .with_detail(article_id.clone()));
            }
        }

        // 补写作分支。只删除**独占且无引用**的图片副本。
        if !current.writing_done {
            match self.delete_paths_from_branch(
                WRITING_BRANCH,
                &article_id,
                &current.markdown_rel_path,
                &current.exclusive_images,
                &current.title,
            ) {
                Ok(_) => {
                    current.writing_done = true;
                    current.last_error = None;
                }
                Err(err) if err.code == ErrorCode::ArticleNotFound => {
                    current.writing_done = true;
                }
                Err(err) => {
                    current.last_error = Some(format!("写作分支：{}", err.message));
                    self.store.upsert_trash(current.clone())?;
                    return Ok(DeleteOutcome::from_entry(&current));
                }
            }
            self.store.upsert_trash(current.clone())?;
        }

        // 补网站分支。
        if !current.main_done {
            match self.delete_paths_from_branch(
                MAIN_BRANCH,
                &article_id,
                &current.markdown_rel_path,
                &current.exclusive_images,
                &current.title,
            ) {
                Ok(_) => {
                    current.main_done = true;
                    current.last_error = None;
                }
                Err(err) if err.code == ErrorCode::ArticleNotFound => {
                    current.main_done = true;
                }
                Err(err) => {
                    current.last_error = Some(format!("网站分支：{}", err.message));
                    self.store.upsert_trash(current.clone())?;
                    return Ok(DeleteOutcome::from_entry(&current));
                }
            }
            self.store.upsert_trash(current.clone())?;
        }

        // 两个分支都完成后再清理本地文件；只清理独占图片。
        let _ = self.workspace.remove_markdown(&article_id);
        for rel in &current.exclusive_images {
            let _ = images::remove_image(self.workspace.root(), rel);
        }

        Ok(DeleteOutcome::from_entry(&current))
    }

    /// 从指定分支删除该文章的 Markdown 与指定图片（隔离索引 + 普通推送）。
    fn delete_paths_from_branch(
        &self,
        branch: &str,
        article_id: &str,
        markdown_rel_path: &str,
        image_paths: &[String],
        title: &str,
    ) -> Result<String> {
        crate::sync::validate_branch(branch)?;
        paths::validate_managed_markdown(markdown_rel_path)?;
        for rel in image_paths {
            paths::validate_managed_rel_path(rel)?;
        }

        let Some(head) = self.sync.fetch(branch)? else {
            return Err(WriterError::new(ErrorCode::ArticleNotFound, "远端分支不存在"));
        };

        let exists = self.sync.markdown_at(&head, article_id)?.is_some();
        if !exists && image_paths.is_empty() {
            return Err(WriterError::new(ErrorCode::ArticleNotFound, "远端该文章已不存在"));
        }
        if !exists {
            // 正文已不在，但可能仍有残留图片需要清理。
        }

        let index_file = git::temp_path("delete-index");
        let index_env = index_file.to_string_lossy().into_owned();
        let result = (|| -> Result<(String, Vec<String>)> {
            git::git_with_env(
                self.workspace.root(),
                &["read-tree", &head],
                &[("GIT_INDEX_FILE", index_env.clone())],
            )?;

            let mut to_remove = vec![markdown_rel_path.to_string()];
            to_remove.extend(image_paths.iter().cloned());
            for rel in &to_remove {
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

            // 白名单核对。
            let changed =
                self.diff_name_only(&head, &tree)?;
            if let Err(offenders) = crate::sync::all_paths_allowed(&changed) {
                return Err(WriterError::new(
                    ErrorCode::GitFailed,
                    "删除操作会改动受管目录之外的文件，已停止",
                )
                .with_detail(offenders.join("、")));
            }
            // 只允许删除本篇及其图片。
            let allowed_prefix = format!("{}{}/", paths::IMAGE_DIR_PREFIX, article_id);
            for rel in &changed {
                if rel != markdown_rel_path && !rel.starts_with(&allowed_prefix) {
                    return Err(WriterError::new(
                        ErrorCode::GitFailed,
                        "删除操作会波及其他文章，已停止",
                    )
                    .with_detail(rel.clone()));
                }
            }

            let message = format!(
                "{}\n\nArticle: {article_id}",
                crate::publish::delete_commit_message(title)
            );
            let commit = git::git(
                self.workspace.root(),
                &["commit-tree", &tree, "-p", &head, "-m", &message],
            )?
            .stdout_trimmed()
            .to_string();
            Ok((commit, changed))
        })();

        let _ = std::fs::remove_file(&index_file);
        let (commit, _changed) = result?;

        // 推送前核头。
        let current = self.sync.fetch(branch)?;
        if current.as_deref() != Some(head.as_str()) {
            return Err(WriterError::new(
                ErrorCode::PushRejected,
                "推送前发现远端已更新，已停止；可重试以基于最新版本补做",
            ));
        }

        let refspec = format!("{commit}:refs/heads/{branch}");
        git::git(self.workspace.root(), &["push", "origin", &refspec])?;

        let verified = self.sync.fetch(branch)?;
        if verified.as_deref() != Some(commit.as_str()) {
            return Err(WriterError::new(
                ErrorCode::GitFailed,
                "推送后未能确认远端分支指向本次提交",
            ));
        }
        Ok(commit)
    }

    /// 在指定分支上替换单个文件内容（用于撤下）。
    fn push_single_path_change(
        &self,
        branch: &str,
        head: &str,
        rel_path: &str,
        contents: &[u8],
        message: &str,
    ) -> Result<String> {
        crate::sync::validate_branch(branch)?;
        paths::validate_managed_rel_path(rel_path)?;

        let index_file = git::temp_path("change-index");
        let index_env = index_file.to_string_lossy().into_owned();
        let result = (|| -> Result<String> {
            git::git_with_env(
                self.workspace.root(),
                &["read-tree", head],
                &[("GIT_INDEX_FILE", index_env.clone())],
            )?;
            let blob = self.write_blob(contents)?;
            git::git_with_env(
                self.workspace.root(),
                &["update-index", "--add", "--cacheinfo", "100644", &blob, rel_path],
                &[("GIT_INDEX_FILE", index_env.clone())],
            )?;
            let tree = git::git_with_env(
                self.workspace.root(),
                &["write-tree"],
                &[("GIT_INDEX_FILE", index_env.clone())],
            )?
            .stdout_trimmed()
            .to_string();

            let changed = self.diff_name_only(head, &tree)?;
            if let Err(offenders) = crate::sync::all_paths_allowed(&changed) {
                return Err(WriterError::new(
                    ErrorCode::GitFailed,
                    "操作会改动受管目录之外的文件，已停止",
                )
                .with_detail(offenders.join("、")));
            }
            let full_message = format!("{message}\n\nArticle: {rel_path}");
            Ok(git::git(
                self.workspace.root(),
                &["commit-tree", &tree, "-p", head, "-m", &full_message],
            )?
            .stdout_trimmed()
            .to_string())
        })();

        let _ = std::fs::remove_file(&index_file);
        let commit = result?;

        let current = self.sync.fetch(branch)?;
        if current.as_deref() != Some(head) {
            return Err(WriterError::new(
                ErrorCode::PushRejected,
                "推送前发现远端已更新，已停止",
            ));
        }
        let refspec = format!("{commit}:refs/heads/{branch}");
        git::git(self.workspace.root(), &["push", "origin", &refspec])?;
        Ok(commit)
    }

    fn write_blob(&self, bytes: &[u8]) -> Result<String> {
        crate::publish::write_blob_helper(self.workspace, bytes)
    }

    fn diff_name_only(&self, from: &str, to: &str) -> Result<Vec<String>> {
        let out = git::git(self.workspace.root(), &["diff", "--name-only", "-z", from, to])?;
        Ok(out
            .stdout
            .split('\0')
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect())
    }

    /// 从回收区恢复为**未发布**的本地工作稿。
    ///
    /// 恢复后先本地可编辑且 `draft: true`，用户需再手动同步和发布。
    /// 若当前同路径已被占用（含大小写冲突），停止并报错，不覆盖当前文章。
    pub fn restore(&self, op_id: &str) -> Result<crate::model::ArticleContent> {
        paths::validate_operation_id(op_id)?;
        let entry = self
            .store
            .load_trash()
            .into_iter()
            .find(|e| e.op_id == op_id)
            .ok_or_else(|| WriterError::new(ErrorCode::InvalidArgument, "找不到该回收记录"))?;
        if !entry.has_markdown {
            return Err(WriterError::new(
                ErrorCode::InvalidArgument,
                "该记录没有可恢复的正文副本",
            ));
        }
        let article_id = entry.article_id.clone();
        paths::validate_existing_article_id(&article_id)?;

        // 冲突检查：本地路径被占用时不得覆盖。
        let target = self.workspace.markdown_abs_path(&article_id)?;
        if target.exists() {
            return Err(WriterError::new(
                ErrorCode::ArticleExists,
                format!("当前工作区已存在 {article_id}，恢复会覆盖现有文章，已停止"),
            )
            .with_detail(article_id.clone()));
        }
        // 远端同路径被他人占用时同样停止。
        if let Some(rev) = self.sync.fetch(WRITING_BRANCH)? {
            if self.sync.markdown_at(&rev, &article_id)?.is_some() {
                let baseline = self.store.load_versions().get(&article_id).cloned().unwrap_or_default();
                if baseline.writing_hash.is_none() {
                    return Err(WriterError::new(
                        ErrorCode::ArticleExists,
                        "写作分支上已有人建立了同一路径的文章，请改用新的 URL 标识再恢复",
                    ));
                }
            }
        }

        let entry_dir = self.store.trash_entry_dir(&entry.op_id);
        // 按记录的确切副本文件名读取；旧条目没有该字段时按文章 ID 现算，
        // **不**在目录里模糊匹配任意 .md，避免恢复到错误内容。
        let backup_name = entry.markdown_backup_name.clone().unwrap_or_else(|| {
            format!(
                "{}.md",
                paths::sanitize_image_basename(&article_id).unwrap_or_else(|| "article".into())
            )
        });
        let backup = std::fs::read(entry_dir.join(&backup_name)).map_err(|_| {
            WriterError::new(
                ErrorCode::IoFailed,
                "回收区中的正文备份不可读，无法恢复（未改动的本地文件保持原样）",
            )
            .with_detail(backup_name)
        })?;

        let text = String::from_utf8_lossy(&backup).into_owned();
        // 恢复为未发布稿：draft 置为 true。
        let parsed = crate::article_io::parse_markdown(&text)?;
        let mut lines: Vec<String> = parsed
            .front_matter
            .split('\n')
            .map(|l| l.strip_suffix('\r').unwrap_or(l).to_string())
            .collect();
        match lines.iter().position(|l| l.starts_with("draft:")) {
            Some(idx) => lines[idx] = "draft: true".to_string(),
            None => lines.push("draft: true".to_string()),
        }
        let front_matter = lines.join(&parsed.fm_newline);
        let restored_text = parsed.render_with(&front_matter, &parsed.body);
        self.workspace.write_raw(&article_id, restored_text.as_bytes())?;

        // 恢复图片副本。
        for image in &entry.images {
            let src = entry_dir.join(&image.backup_name);
            if let Ok(bytes) = std::fs::read(&src) {
                let abs = self.workspace.root().join(&image.rel_path);
                if let Some(parent) = abs.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = crate::article_io::atomic_write(&abs, &bytes);
            }
        }

        // 恢复后的本地版本与远端无关：清掉同步基线，需用户重新同步与发布。
        self.store.update_baseline(&article_id, |b: &mut ArticleBaseline| {
            b.writing_commit = None;
            b.writing_hash = None;
            b.main_commit = None;
            b.main_hash = None;
            b.main_published = None;
            b.edit_base_hash = Some(util::hash_bytes(restored_text.as_bytes()));
            b.writing_image_hashes = entry
                .images
                .iter()
                .map(|i| (i.rel_path.clone(), i.content_hash.clone()))
                .collect();
        })?;

        self.workspace.read(&article_id, &BTreeMap::new())
    }

    /// 列出回收区条目。
    pub fn list(&self) -> Vec<TrashEntry> {
        self.store.load_trash()
    }

    /// 删除回收记录（用户确认不再需要副本时）。
    ///
    /// `op_id` 会拼成回收区目录名（`<数据目录>/trash/<op_id>`），因此先做格式
    /// 校验，并且只有索引中确实存在该条目时才清理目录——避免任何构造出来的
    /// `op_id`（例如 `..`）让目录解析到回收区之外并被递归删除。
    pub fn purge(&self, op_id: &str) -> Result<()> {
        paths::validate_operation_id(op_id)?;
        let mut entries = self.store.load_trash();
        if !entries.iter().any(|e| e.op_id == op_id) {
            return Err(WriterError::new(
                ErrorCode::InvalidArgument,
                "找不到该回收记录，未做任何删除",
            ));
        }
        entries.retain(|e| e.op_id != op_id);
        self.store.save_trash(&entries)?;
        let dir = self.store.trash_entry_dir(op_id);
        if dir.is_dir() {
            let _ = std::fs::remove_dir_all(&dir);
        }
        Ok(())
    }

    /// 写入一份崩溃恢复副本。
    pub fn write_recovery(&self, article_id: &str, markdown: &str) -> Result<()> {
        self.store.write_recovery(&RecoveryDraft {
            article_id: article_id.to_string(),
            markdown_rel_path: format!("{}{}.md", paths::BLOG_DIR_PREFIX, article_id),
            markdown: markdown.to_string(),
            saved_at_unix: util::unix_seconds(),
        })
    }

    /// 清除崩溃恢复副本。
    pub fn clear_recovery(&self, article_id: &str) {
        self.store.clear_recovery(article_id);
    }

    /// 列出待处理的崩溃恢复副本。
    pub fn pending_recovery(&self) -> Vec<RecoveryDraft> {
        self.store.list_recovery()
    }
}

/// 供上层判定删除是否真的完成。
pub fn delete_is_complete(outcome: &DeleteOutcome) -> bool {
    outcome.writing_done && outcome.main_done
}

/// 图片独占判定（供测试与上层复用）。
pub fn exclusive_images(images: &[TrashImage], protected: &[String]) -> Vec<String> {
    images
        .iter()
        .map(|i| i.rel_path.clone())
        .filter(|p| !protected.contains(p))
        .collect()
}

#[cfg(test)]
mod integration {
    use super::*;
    use crate::testkit::{sample_markdown, sample_markdown_with_body, tiny_png, TestEnv};

    fn engine<'a>(env: &'a TestEnv) -> TrashEngine<'a, 'a> {
        TrashEngine::new(
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

    fn write_article(env: &TestEnv, id: &str, title: &str, draft: bool) {
        std::fs::write(
            env.path().join(format!("src/content/blog/{id}.md")),
            sample_markdown(title, draft),
        )
        .unwrap();
    }

    /// A7：撤下 A 后站点不再有它（main 上 draft: true），写作稿还在。
    #[test]
    fn a7_withdraw_removes_from_site_but_keeps_writing_draft() {
        let env = TestEnv::new();
        write_article(&env, "wd-a", "待撤下", true);
        let se = sync_engine(&env);
        se.push_article("wd-a", "同步", false).unwrap();
        crate::publish::PublishEngine::new(
            &env.workspace,
            &env.store,
            &format!("file://{}", env.remote.to_string_lossy().replace('\\', "/")),
        )
        .publish("wd-a", "发布", &[], None)
        .unwrap();

        // 本地改出新稿并同步到写作分支（撤下不应丢掉它）。
        let mut new_meta = crate::model::ArticleMeta {
            title: "待撤下".to_string(),
            description: "新稿摘要".to_string(),
            pub_date: "2026-09-23".to_string(),
            updated_date: None,
            tags: vec!["测试样稿".to_string()],
            draft: true,
        };
        new_meta.description = "新稿摘要".to_string();
        env.workspace.save("wd-a", &new_meta, "撤下期间的新稿正文\n", None).unwrap();
        se.push_article("wd-a", "同步新稿", false).unwrap();

        let te = engine(&env);
        let outcome = te.withdraw("wd-a").unwrap();
        assert!(outcome.main_done);

        // main 上该篇存在但 draft: true（构建时被过滤）。
        let main_text = env.remote_file_direct("main", "src/content/blog/wd-a.md").unwrap();
        assert!(main_text.contains("draft: true"), "撤下后 main 应为草稿：{main_text}");
        assert!(main_text.contains("测试样稿正文"), "撤下只改公开标志，正文保留");
        // 写作分支上的新稿仍在。
        assert!(env
            .remote_file_direct("writing", "src/content/blog/wd-a.md")
            .unwrap()
            .contains("撤下期间的新稿正文"));
    }

    /// 撤下可以直接反向再发布。
    #[test]
    fn withdraw_can_be_republished() {
        let env = TestEnv::new();
        write_article(&env, "wd-b", "可再发布", true);
        let se = sync_engine(&env);
        se.push_article("wd-b", "同步", false).unwrap();
        let pe = crate::publish::PublishEngine::new(
            &env.workspace,
            &env.store,
            &format!("file://{}", env.remote.to_string_lossy().replace('\\', "/")),
        );
        pe.publish("wd-b", "发布", &[], None).unwrap();
        engine(&env).withdraw("wd-b").unwrap();

        // 再发布：需要先同步（本地 draft: true 与 main draft: true 内容一致，
        // 但发布需要一个有效的写作快照）。
        se.push_article("wd-b", "再同步", false).unwrap();
        pe.publish("wd-b", "再发布", &[], None).unwrap();
        assert!(env
            .remote_file_direct("main", "src/content/blog/wd-b.md")
            .unwrap()
            .contains("draft: false"));
    }

    /// A8：删除时第二个分支推送失败 → 保留副本、状态精确、重试后完成。
    #[test]
    fn a8_partial_delete_keeps_backup_and_retry_completes() {
        let env = TestEnv::new();
        // A 已发布，B 也存在。
        write_article(&env, "del-a", "待删除 A", true);
        write_article(&env, "del-b", "保留 B", true);
        let se = sync_engine(&env);
        se.push_article("del-a", "同步 A", false).unwrap();
        se.push_article("del-b", "同步 B", false).unwrap();
        let pe = crate::publish::PublishEngine::new(
            &env.workspace,
            &env.store,
            &format!("file://{}", env.remote.to_string_lossy().replace('\\', "/")),
        );
        pe.publish("del-a", "发布 A", &[], None).unwrap();

        // 让 main 的推送被远端拒绝（真实的部分成功场景）。
        env.reject_pushes_to("main");

        let te = engine(&env);
        let outcome = te.delete("del-a").unwrap();

        // writing 已删、main 未删：状态必须精确，且不得声称删除成功。
        assert!(outcome.writing_done, "{outcome:?}");
        assert!(!outcome.main_done, "main 推送应失败：{outcome:?}");
        assert!(!delete_is_complete(&outcome));
        assert!(outcome.branch_state.contains("网站仍在"), "{}", outcome.branch_state);
        assert!(env.remote_file_direct("writing", "src/content/blog/del-a.md").is_none());
        assert!(env.remote_file_direct("main", "src/content/blog/del-a.md").is_some());

        // 回收副本可读。
        let entry = env
            .store
            .load_trash()
            .into_iter()
            .find(|e| e.op_id == outcome.op_id)
            .unwrap();
        assert!(entry.has_markdown);
        let backup_dir = env.store.trash_entry_dir(&entry.op_id);
        assert!(std::fs::read_dir(&backup_dir).unwrap().count() > 0, "必须有可恢复副本");

        // 重试：只补 main，不重删 writing，也不误删 B。
        env.clear_push_hooks();
        let retried = te.retry_delete(&outcome.op_id).unwrap();
        assert!(retried.writing_done && retried.main_done, "{retried:?}");
        assert!(delete_is_complete(&retried));
        assert!(env.remote_file_direct("main", "src/content/blog/del-a.md").is_none());
        assert!(
            env.remote_file_direct("writing", "src/content/blog/del-b.md").is_some(),
            "B 不得被误删"
        );
        assert!(env.remote_file_direct("main", "src/content/blog/del-b.md").is_none());
    }

    /// A9：A、B 引用同图，删除 A → 被 B 引用的图在所有分支保留。
    #[test]
    fn a9_shared_image_survives_deletion_of_one_owner() {
        let env = TestEnv::new();
        crate::images::import_image_bytes(env.path(), "shared-a", "共图", &tiny_png()).unwrap();
        let images = crate::images::article_images(env.path(), "shared-a").unwrap();
        let url = crate::images::url_path_for(&images[0].rel_path).unwrap();
        let shared_rel = images[0].rel_path.clone();

        // A 拥有并引用；B 也引用同一张图。
        std::fs::write(
            env.path().join("src/content/blog/shared-a.md"),
            sample_markdown_with_body("共享图 A", true, &format!("![图]({url})")),
        )
        .unwrap();
        std::fs::write(
            env.path().join("src/content/blog/shared-b.md"),
            sample_markdown_with_body("引用者 B", true, &format!("![图]({url})")),
        )
        .unwrap();

        let se = sync_engine(&env);
        se.push_article("shared-a", "同步 A", false).unwrap();
        se.push_article("shared-b", "同步 B", false).unwrap();
        // A 发布到 main：让共享图片真实存在于**两个**分支，这样删除 A 之后
        // 「main 上不得删掉被 B 引用的图」才是有效断言（此前该断言缺失）。
        crate::publish::PublishEngine::new(
            &env.workspace,
            &env.store,
            &format!("file://{}", env.remote.to_string_lossy().replace('\\', "/")),
        )
        .publish("shared-a", "发布 A", &[], None)
        .unwrap();
        assert!(
            env.remote_file_direct("main", &shared_rel).is_some(),
            "前置条件：共享图片应已随 A 发布到 main"
        );

        let te = engine(&env);
        let assessment = te.assess_delete("shared-a").unwrap();
        assert!(
            assessment.protected_images.contains(&shared_rel),
            "被 B 引用的图必须列入保护：{assessment:?}"
        );
        assert!(assessment.exclusive_images.is_empty());

        let outcome = te.delete("shared-a").unwrap();
        assert!(delete_is_complete(&outcome));
        assert!(env.remote_file_direct("writing", "src/content/blog/shared-a.md").is_none());
        assert!(env.remote_file_direct("main", "src/content/blog/shared-a.md").is_none());
        // 图片在**两个**分支都必须保留（B 仍引用它）。
        assert!(env.remote_file_direct("writing", &shared_rel).is_some(), "写作分支图应保留");
        assert!(
            env.remote_file_direct("main", &shared_rel).is_some(),
            "main 上被 B 引用的共享图不得被清理"
        );
        // 本地文件也保留。
        assert!(env.path().join(&shared_rel).exists());
    }

    /// 独占且无引用的图片应被清理。
    #[test]
    fn exclusive_unreferenced_images_are_cleaned_up() {
        let env = TestEnv::new();
        crate::images::import_image_bytes(env.path(), "solo-a", "独图", &tiny_png()).unwrap();
        let images = crate::images::article_images(env.path(), "solo-a").unwrap();
        let rel = images[0].rel_path.clone();
        // 正文不引用图片（孤儿图）。
        std::fs::write(
            env.path().join("src/content/blog/solo-a.md"),
            sample_markdown("孤儿图", true),
        )
        .unwrap();

        let se = sync_engine(&env);
        se.push_article("solo-a", "同步", false).unwrap();

        let te = engine(&env);
        let assessment = te.assess_delete("solo-a").unwrap();
        assert_eq!(assessment.exclusive_images, vec![rel.clone()]);

        let outcome = te.delete("solo-a").unwrap();
        assert!(delete_is_complete(&outcome));
        assert!(env.remote_file_direct("writing", &rel).is_none());
        assert!(!env.path().join(&rel).exists());
    }

    /// A10：从未发布文章删除后恢复 → 本地为未发布草稿，不立即回到 main 或网站。
    #[test]
    fn a10_restore_yields_unpublished_local_draft() {
        let env = TestEnv::new();
        write_article(&env, "restore-a", "待恢复", true);
        let se = sync_engine(&env);
        se.push_article("restore-a", "同步", false).unwrap();

        let te = engine(&env);
        let outcome = te.delete("restore-a").unwrap();
        assert!(delete_is_complete(&outcome));
        assert!(env.remote_file_direct("writing", "src/content/blog/restore-a.md").is_none());

        // 恢复。
        let restored = te.restore(&outcome.op_id).unwrap();
        assert!(restored.meta.draft, "恢复后必须是未发布草稿");
        assert_eq!(restored.meta.title, "待恢复");
        // 不立即出现在远端。
        assert!(env.remote_file_direct("writing", "src/content/blog/restore-a.md").is_none());
        assert!(env.remote_file_direct("main", "src/content/blog/restore-a.md").is_none());
    }

    /// 恢复时本地同路径被占用 → 停止且不覆盖。
    #[test]
    fn restore_refuses_to_overwrite_existing_article() {
        let env = TestEnv::new();
        write_article(&env, "occupy-a", "原文章", true);
        let se = sync_engine(&env);
        se.push_article("occupy-a", "同步", false).unwrap();

        let te = engine(&env);
        let outcome = te.delete("occupy-a").unwrap();

        // 新建同路径文章。
        std::fs::write(
            env.path().join("src/content/blog/occupy-a.md"),
            sample_markdown("新的同路径文章", true),
        )
        .unwrap();

        let err = te.restore(&outcome.op_id).unwrap_err();
        assert_eq!(err.code, ErrorCode::ArticleExists);
        // 现有文章未被覆盖。
        assert_eq!(
            env.workspace.read("occupy-a", &BTreeMap::new()).unwrap().meta.title,
            "新的同路径文章"
        );
    }

    /// 删除不会抹去 Git 历史：被删提交仍可从远端取回。
    #[test]
    fn delete_keeps_git_history() {
        let env = TestEnv::new();
        write_article(&env, "hist-a", "历史保留", true);
        let se = sync_engine(&env);
        se.push_article("hist-a", "同步", false).unwrap();
        let before_delete = env.remote_head("writing").unwrap();

        engine(&env).delete("hist-a").unwrap();
        // 被删提交仍在远端对象库中可读。
        assert!(env.remote_file_direct("writing", "src/content/blog/hist-a.md").is_none());
        let envs = env.envs();
        let out = std::process::Command::new("git")
            .current_dir(&env.remote)
            .envs(envs.iter().map(|(k, v)| (*k, v.clone())))
            .args(["show", &format!("{before_delete}:src/content/blog/hist-a.md")])
            .output()
            .unwrap();
        assert!(out.status.success(), "Git 历史应仍可恢复该文件");
        assert!(String::from_utf8_lossy(&out.stdout).contains("历史保留"));
    }

    /// A11：改标题不动 URL；改 URL 是独立操作。
    #[test]
    fn a11_renaming_title_does_not_change_url() {
        let env = TestEnv::new();
        write_article(&env, "url-a", "原标题", true);
        let se = sync_engine(&env);
        se.push_article("url-a", "同步", false).unwrap();

        // 只改标题。
        let mut meta = env.workspace.read("url-a", &BTreeMap::new()).unwrap().meta;
        meta.title = "改过的标题".to_string();
        env.workspace.save("url-a", &meta, "正文\n", None).unwrap();

        // 文件路径（= URL）不变。
        assert!(env.path().join("src/content/blog/url-a.md").exists());
        se.push_article("url-a", "改标题", false).unwrap();
        assert!(env.remote_file_direct("writing", "src/content/blog/url-a.md").is_some());
        assert_eq!(
            env.workspace.read("url-a", &BTreeMap::new()).unwrap().meta.title,
            "改过的标题"
        );
    }

    /// A12：导入四篇示例只影响被逐篇导入的文件，源目录与 Git 状态不变。
    #[test]
    fn a12_import_is_explicit_per_file_and_leaves_source_untouched() {
        let env = TestEnv::new();
        // 模拟「当前开发目录」中的未跟踪示例。
        let source_dir = tempfile::tempdir().unwrap();
        let names = [
            "reading-code-example",
            "organizing-clues-example",
            "small-notes-example",
            "unfinished-attempt-example",
        ];
        for (i, name) in names.iter().enumerate() {
            std::fs::write(
                source_dir.path().join(format!("{name}.md")),
                sample_markdown(&format!("示例 {i}"), false),
            )
            .unwrap();
        }
        let source_before: Vec<String> = {
            let mut v: Vec<String> = std::fs::read_dir(source_dir.path())
                .unwrap()
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            v.sort();
            v
        };

        // 只导入其中的两篇。
        let imported = ["reading-code-example", "small-notes-example"];
        for name in imported {
            env.workspace
                .import_markdown_file(&source_dir.path().join(format!("{name}.md")), name)
                .unwrap();
        }

        let scan = env.workspace.scan(&env.store, &BTreeMap::new()).unwrap();
        let ids: Vec<String> = scan.iter().map(|s| s.id.clone()).collect();
        assert_eq!(ids.len(), 2, "只有明确导入的文件进入工作区：{ids:?}");
        for name in imported {
            assert!(ids.contains(&name.to_string()));
        }
        for name in names.iter().filter(|n| !imported.contains(n)) {
            assert!(!ids.contains(&name.to_string()), "{name} 不应被导入");
        }

        // 源目录内容与顺序完全不变。
        let source_after: Vec<String> = {
            let mut v: Vec<String> = std::fs::read_dir(source_dir.path())
                .unwrap()
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            v.sort();
            v
        };
        assert_eq!(source_before, source_after);
        // 工作副本没有产生意外的 Git 改动（导入的文件是未跟踪的新文件）。
        let dirty = env.managed_dirty();
        assert!(
            dirty.iter().all(|l| l.contains("reading-code-example") || l.contains("small-notes-example")),
            "只应有被导入文件的改动：{dirty:?}"
        );
    }

    /// 删除评估会列出准确的分支影响与图片清单。
    #[test]
    fn delete_assessment_reports_branch_impacts() {
        let env = TestEnv::new();
        write_article(&env, "assess-a", "评估", true);
        let se = sync_engine(&env);
        se.push_article("assess-a", "同步", false).unwrap();
        let te = engine(&env);
        let a = te.assess_delete("assess-a").unwrap();
        assert!(a.on_writing);
        assert!(!a.on_main);
        assert!(!a.was_published);
        assert_eq!(a.markdown_rel_path, "src/content/blog/assess-a.md");

        crate::publish::PublishEngine::new(
            &env.workspace,
            &env.store,
            &format!("file://{}", env.remote.to_string_lossy().replace('\\', "/")),
        )
        .publish("assess-a", "发布", &[], None)
        .unwrap();
        let b = te.assess_delete("assess-a").unwrap();
        assert!(b.on_main && b.was_published);
        assert!(b.writing_head.is_some() && b.main_head.is_some());
    }

    /// 回收区列表与清理。
    #[test]
    fn trash_listing_and_purge() {
        let env = TestEnv::new();
        write_article(&env, "purge-a", "待清理", true);
        let se = sync_engine(&env);
        se.push_article("purge-a", "同步", false).unwrap();
        let te = engine(&env);
        let outcome = te.delete("purge-a").unwrap();

        assert_eq!(te.list().len(), 1);
        assert!(te.list()[0].article_id == "purge-a");
        te.purge(&outcome.op_id).unwrap();
        assert!(te.list().is_empty());
        assert!(!env.store.trash_entry_dir(&outcome.op_id).exists());
    }

    /// 崩溃恢复副本的读写。
    #[test]
    fn recovery_drafts_are_persisted_and_cleared() {
        let env = TestEnv::new();
        write_article(&env, "crash-a", "崩溃恢复", true);
        let te = engine(&env);
        let text = std::fs::read_to_string(env.path().join("src/content/blog/crash-a.md")).unwrap();
        te.write_recovery("crash-a", &text).unwrap();

        let pending = te.pending_recovery();
        assert_eq!(pending.len(), 1);
        assert!(pending[0].markdown.contains("崩溃恢复"));

        te.clear_recovery("crash-a");
        assert!(te.pending_recovery().is_empty());
    }

    /// 独占图片过滤辅助函数。
    #[test]
    fn exclusive_image_filtering() {
        let images = vec![
            TrashImage {
                rel_path: "public/blog/a/one.png".to_string(),
                backup_name: "one.png".to_string(),
                size: 1,
                content_hash: "h1".to_string(),
            },
            TrashImage {
                rel_path: "public/blog/a/two.png".to_string(),
                backup_name: "two.png".to_string(),
                size: 2,
                content_hash: "h2".to_string(),
            },
        ];
        let protected = vec!["public/blog/a/two.png".to_string()];
        assert_eq!(exclusive_images(&images, &protected), vec!["public/blog/a/one.png"]);
    }

    /// 删除草稿（未发布）只影响写作分支。
    #[test]
    fn deleting_unpublished_draft_only_affects_writing() {
        let env = TestEnv::new();
        write_article(&env, "draft-del", "仅草稿", true);
        let se = sync_engine(&env);
        se.push_article("draft-del", "同步", false).unwrap();
        let main_before = env.remote_head("main").unwrap();

        let outcome = engine(&env).delete("draft-del").unwrap();
        assert!(delete_is_complete(&outcome));
        assert!(env.remote_file_direct("writing", "src/content/blog/draft-del.md").is_none());
        assert_eq!(env.remote_head("main").unwrap(), main_before, "main 不应被改动");
    }

    /// 跨文章引用检测使用相对路径写法时同样有效。
    #[test]
    fn protects_images_referenced_with_relative_paths() {
        let env = TestEnv::new();
        crate::images::import_image_bytes(env.path(), "rel-a", "相对图", &tiny_png()).unwrap();
        let images = crate::images::article_images(env.path(), "rel-a").unwrap();
        let file_name = std::path::Path::new(&images[0].rel_path)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();

        std::fs::write(
            env.path().join("src/content/blog/rel-a.md"),
            sample_markdown_with_body("拥有者", true, &format!("![图](./{file_name})")),
        )
        .unwrap();
        // B 用相对写法引用同一文件名。
        std::fs::write(
            env.path().join("src/content/blog/rel-b.md"),
            sample_markdown_with_body("引用者", true, &format!("![图]({file_name})")),
        )
        .unwrap();

        let protected = crate::publish::images_referenced_elsewhere(
            &env.workspace,
            "rel-a",
            &[images[0].rel_path.clone()],
        )
        .unwrap();
        assert_eq!(protected, vec![images[0].rel_path.clone()]);
    }

    /// P1 回归：图片只被**远端分支上**（本地尚无）的文章引用时也必须保留。
    ///
    /// 旧实现只扫描本地工作区，远端分支上他人新增的引用者看不到，会把共享图片
    /// 误判为独占图片并从远端删除，导致那篇文章的配图 404。方案要求删除前扫描
    /// 主站与写作分支**全部受管 Markdown** 的引用，不确定的默认留下。
    #[test]
    fn protects_images_referenced_only_on_remote_branch() {
        let env = TestEnv::new();
        // A 拥有并引用一张图，随后同步（图片进入 writing）并发布。
        crate::images::import_image_bytes(env.path(), "rb-a", "共图", &tiny_png()).unwrap();
        let images = crate::images::article_images(env.path(), "rb-a").unwrap();
        let url = crate::images::url_path_for(&images[0].rel_path).unwrap();
        let shared_rel = images[0].rel_path.clone();
        std::fs::write(
            env.path().join("src/content/blog/rb-a.md"),
            sample_markdown_with_body("拥有者 A", true, &format!("![图]({url})")),
        )
        .unwrap();
        let se = sync_engine(&env);
        se.push_article("rb-a", "同步 A", false).unwrap();
        crate::publish::PublishEngine::new(
            &env.workspace,
            &env.store,
            &format!("file://{}", env.remote.to_string_lossy().replace('\\', "/")),
        )
        .publish("rb-a", "发布 A", &[], None)
        .unwrap();

        // 他处设备在写作分支上新增了一篇引用同一张图的文章（本地看不到它）。
        let other = env.other_device("remote-ref");
        other.fetch("writing");
        other.checkout_new_branch("writing", "origin/writing");
        other.write_and_commit(
            "src/content/blog/rb-b.md",
            &sample_markdown_with_body("远端引用者 B", true, &format!("![图]({url})")),
            "他处引用同一张图",
        );
        other.push("writing");
        assert!(!env.path().join("src/content/blog/rb-b.md").exists(), "前置条件：B 不在本地");

        let te = engine(&env);
        let assessment = te.assess_delete("rb-a").unwrap();
        assert!(
            assessment.protected_images.contains(&shared_rel),
            "远端分支上被引用的图片必须列入保护：{assessment:?}"
        );
        assert!(assessment.exclusive_images.is_empty());

        let outcome = te.delete("rb-a").unwrap();
        assert!(delete_is_complete(&outcome));
        // 图片必须在两个分支都保留，供远端那篇引用者使用。
        assert!(
            env.remote_file_direct("writing", &shared_rel).is_some(),
            "写作分支上被远端文章引用的图片不得删除"
        );
        assert!(
            env.remote_file_direct("main", &shared_rel).is_some(),
            "main 上被引用的图片不得删除"
        );
    }

    /// P0-1：`purge` 只在索引中确有该条目时才动目录，且拒绝穿越形式的 `op_id`。
    ///
    /// 回归的是真实缺陷：`op_id` 会拼成 `<数据目录>/trash/<op_id>`，此前无校验，
    /// `op_id = ".."` 会让 `remove_dir_all` 递归删掉整个应用数据目录。
    #[test]
    fn purge_rejects_traversal_and_unknown_op_id() {
        let env = TestEnv::new();
        write_article(&env, "pg-a", "待清理", true);
        let se = sync_engine(&env);
        se.push_article("pg-a", "同步", false).unwrap();
        let te = engine(&env);
        let outcome = te.delete("pg-a").unwrap();
        assert!(delete_is_complete(&outcome));

        // 数据目录中的哨兵文件：任何越界删除都会让它消失。
        let data_root = env.store.root().to_path_buf();
        std::fs::write(data_root.join("canary.txt"), b"keep me").unwrap();

        for evil in ["..", "../..", ".", "", "op-../x", "op-1-a1b2c3d4e5f6/../.."] {
            let err = te.purge(evil).unwrap_err();
            assert!(
                matches!(err.code, ErrorCode::InvalidArgument),
                "op_id={evil:?} 必须被拒绝：{err:?}"
            );
        }
        assert!(data_root.exists(), "数据目录不得被删除");
        assert!(data_root.join("canary.txt").exists(), "数据目录中的文件不得被删除");
        assert!(data_root.join("config.json").exists(), "配置不得被删除");

        // 索引中不存在的（但格式合法）操作 ID 同样不得触发删除。
        assert!(te.purge("op-1758970000-a1b2c3d4e5f6").is_err());
        assert!(data_root.join("canary.txt").exists());

        // 正常路径：清理成功，且只影响该条目。
        te.purge(&outcome.op_id).unwrap();
        assert!(!env.store.trash_entry_dir(&outcome.op_id).exists());
        assert!(te.list().is_empty());
        assert!(data_root.join("canary.txt").exists(), "清理单条记录不得影响数据目录其它内容");
    }

    /// P1：重试删除**不得**清理被其他文章引用的共享图片。
    ///
    /// 首次删除用独占集，重试此前却用「该文章目录下全部图片」，会在重试路径
    /// 绕过 A9 的保护，删掉 B 仍在引用的图。
    #[test]
    fn retry_delete_does_not_remove_shared_images() {
        let env = TestEnv::new();
        crate::images::import_image_bytes(env.path(), "retry-a", "共图", &tiny_png()).unwrap();
        let images = crate::images::article_images(env.path(), "retry-a").unwrap();
        let url = crate::images::url_path_for(&images[0].rel_path).unwrap();
        let shared_rel = images[0].rel_path.clone();

        std::fs::write(
            env.path().join("src/content/blog/retry-a.md"),
            sample_markdown_with_body("拥有者", true, &format!("![图]({url})")),
        )
        .unwrap();
        std::fs::write(
            env.path().join("src/content/blog/retry-b.md"),
            sample_markdown_with_body("引用者", true, &format!("![图]({url})")),
        )
        .unwrap();

        let se = sync_engine(&env);
        se.push_article("retry-a", "同步 A", false).unwrap();
        se.push_article("retry-b", "同步 B", false).unwrap();
        // A 已发布，这样删除才需要动 main；让 main 推送被拒即可制造
        // 「写作分支已删／网站仍在」的部分成功状态。
        crate::publish::PublishEngine::new(
            &env.workspace,
            &env.store,
            &format!("file://{}", env.remote.to_string_lossy().replace('\\', "/")),
        )
        .publish("retry-a", "发布 A", &[], None)
        .unwrap();
        assert!(env.remote_file_direct("main", &shared_rel).is_some());

        env.reject_pushes_to("main");
        let te = engine(&env);
        let outcome = te.delete("retry-a").unwrap();
        assert!(outcome.writing_done && !outcome.main_done, "{outcome:?}");

        env.clear_push_hooks();
        let retried = te.retry_delete(&outcome.op_id).unwrap();
        assert!(delete_is_complete(&retried));

        // 被 B 引用的共享图片必须仍在两个分支上。旧实现的重试用「该文章目录下
        // 全部图片」而非常独占集，会把 main 上这张图删掉，导致 B 的配图 404。
        assert!(
            env.remote_file_direct("main", &shared_rel).is_some(),
            "重试不得删除被 B 引用的 main 分支图片"
        );
        assert!(
            env.remote_file_direct("writing", &shared_rel).is_some(),
            "重试不得删除被 B 引用的写作分支图片"
        );
        assert!(env.path().join(&shared_rel).exists(), "本地共享图片不得被清理");
    }

    /// P1：重试前若远端同路径已被他人改写／重建，必须停止，不得继续删除。
    #[test]
    fn retry_delete_stops_when_remote_path_was_rewritten() {
        let env = TestEnv::new();
        write_article(&env, "rw-a", "原文章", true);
        let se = sync_engine(&env);
        se.push_article("rw-a", "同步", false).unwrap();
        crate::publish::PublishEngine::new(
            &env.workspace,
            &env.store,
            &format!("file://{}", env.remote.to_string_lossy().replace('\\', "/")),
        )
        .publish("rw-a", "发布", &[], None)
        .unwrap();

        env.reject_pushes_to("main");
        let te = engine(&env);
        let outcome = te.delete("rw-a").unwrap();
        assert!(outcome.writing_done && !outcome.main_done, "{outcome:?}");
        env.clear_push_hooks();

        // 另一个「设备」在 main 上把同路径改写成了不同的内容。
        let other = env.other_device("rewriter");
        other.write_and_commit(
            "src/content/blog/rw-a.md",
            &sample_markdown_with_body("被他人重建", false, "完全不同的新内容\n"),
            "外部重建同路径",
        );
        other.push("main");

        // 重试必须停下来报冲突，而不是把别人新建的内容删掉。
        let err = te.retry_delete(&outcome.op_id).unwrap_err();
        assert_eq!(err.code, ErrorCode::RemoteChanged, "{err:?}");
        assert!(
            env.remote_file_direct("main", "src/content/blog/rw-a.md")
                .unwrap()
                .contains("完全不同的新内容"),
            "他人重建的 main 内容不得被旧操作删除"
        );
    }

    /// 已完成的删除操作重复调用 `retry_delete` 应该是无副作用的空操作。
    ///
    /// 否则用户从回收区恢复文章后，再次重试会把恢复出来的文件删掉。
    #[test]
    fn retry_delete_on_completed_op_is_a_noop() {
        let env = TestEnv::new();
        write_article(&env, "noop-a", "已完成", true);
        let se = sync_engine(&env);
        se.push_article("noop-a", "同步", false).unwrap();

        let te = engine(&env);
        let outcome = te.delete("noop-a").unwrap();
        assert!(delete_is_complete(&outcome));

        // 用户从回收区恢复该文章。
        let restored = te.restore(&outcome.op_id).unwrap();
        assert!(restored.meta.draft);
        let restored_abs = env.path().join("src/content/blog/noop-a.md");
        assert!(restored_abs.exists());

        // 再次重试：已完成的记录不得再动本地文件。
        let again = te.retry_delete(&outcome.op_id).unwrap();
        assert!(delete_is_complete(&again));
        assert!(restored_abs.exists(), "已完成的删除不得重复执行并删掉恢复出的文件");
        assert!(env.remote_file_direct("writing", "src/content/blog/noop-a.md").is_none());
    }
}
