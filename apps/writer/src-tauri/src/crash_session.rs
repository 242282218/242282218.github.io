//! 异常关闭（崩溃）恢复的进程级验证。
//!
//! 这条路径无法只靠单元测试证明——它要验证的正是「进程**被外部强制结束**之后，
//! 磁盘上留下了什么、下次启动能不能取回」。因此这里提供几个子命令，配合
//! `scripts/test/crash-recovery.mjs` 用真实进程完成验收：
//!
//! ```text
//! --crash-session <数据目录>              建立隔离仓库、写文章、留下未落盘的编辑，然后挂起等被杀
//! --inspect-recovery <数据目录> [--json]  只读：报告恢复副本、磁盘内容与远端分支头
//! --restore-recovery <数据目录> <文章ID>  执行恢复并报告结果
//! ```
//!
//! 与自检相同，这些子命令都在**隔离的临时目录**中操作，不接触用户配置的真实仓库，
//! 也不访问网络。

use crate::app_commands as core;
use crate::local_store::{AppConfig, LocalStore};
use crate::model::{ArticleMeta, ErrorCode, Result, WriterError};
use crate::preview::ToolchainReport;
use crate::workspace::Workspace;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// 崩溃会话使用的文章标识。
pub const CRASH_ARTICLE_ID: &str = "crash-session-article";

/// 崩溃前「已保存」的正文。
pub const SAVED_BODY: &str = "已经保存到磁盘的正文。\n";

/// 崩溃前「已保存」的标题。
pub const SAVED_TITLE: &str = "崩溃会话：已保存的标题";

/// 崩溃前「未保存」的正文（只存在于恢复副本里）。
pub const UNSAVED_BODY: &str = "崩溃前尚未保存的正文，必须能被恢复。\n";

/// 崩溃前「未保存」的标题。
pub const UNSAVED_TITLE: &str = "崩溃会话：未保存的标题";

/// 只读检查的结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryInspection {
    /// 恢复副本中的文章标识列表。
    pub pending_article_ids: Vec<String>,
    /// 恢复副本里是否含未保存的正文（用于确认内容真的留下来了）。
    pub pending_contains_unsaved_body: bool,
    /// 磁盘文章是否**不含**未保存正文（确认崩溃前确实没落盘）。
    pub disk_lacks_unsaved_body: bool,
    /// 磁盘文章的标题（确认仍是崩溃前的旧值）。
    pub disk_title: Option<String>,
    /// `writing` 分支头的提交（`None` 表示分支不存在）。
    pub writing_head: Option<String>,
    /// `main` 分支头的提交。
    pub main_head: Option<String>,
    /// 环境检查，便于解释失败原因。
    pub toolchain: ToolchainReport,
}

/// 恢复执行的结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreResult {
    pub article_id: String,
    /// 恢复后的标题。
    pub title: String,
    /// 恢复后的正文。
    pub body: String,
    /// 恢复后是否还有待处理副本（内容与磁盘一致后应为空）。
    pub pending_after: Vec<String>,
    pub writing_head: Option<String>,
    pub main_head: Option<String>,
}

/// 打开（或建立）一个隔离的崩溃测试环境。
///
/// 数据目录为空时自动建立 bare 远端 + 工作副本 + 连接配置，因此
/// `--crash-session` 与后续的 `--inspect-recovery` 可以共用同一目录。
fn open_env(data_dir: &Path) -> Result<(LocalStore, AppConfig)> {
    let store = LocalStore::open_at(data_dir.to_path_buf())?;
    let mut config = store.load_config();

    let root = data_dir.join("crash-root");
    let remote = root.join("remote.git");
    let workspace_dir = root.join("workspace");

    if !workspace_dir.join(".git").exists() {
        seed(&root, &remote, &workspace_dir)?;
    }

    config.workspace_dir = workspace_dir.to_string_lossy().to_string();
    config.repo_url = format!("file://{}", remote.to_string_lossy().replace('\\', "/"));
    config.connected = true;
    config.disclosed_public_drafts = true;
    store.save_config(&config)?;
    Ok((store, config))
}

/// 建立 bare 远端与工作副本（一次性）。
fn seed(root: &Path, remote: &Path, workspace: &Path) -> Result<()> {
    let io =
        |e: std::io::Error| WriterError::new(ErrorCode::IoFailed, format!("创建测试仓库失败：{e}"));

    std::fs::create_dir_all(remote).map_err(io)?;
    crate::git::git(remote, &["init", "--bare", "--initial-branch=main"])?;

    let seed_dir = root.join("seed");
    std::fs::create_dir_all(seed_dir.join("src/content/blog")).map_err(io)?;
    std::fs::create_dir_all(seed_dir.join("public/blog")).map_err(io)?;
    std::fs::write(seed_dir.join("package.json"), "{\n  \"name\": \"crash-fixture\"\n}\n")
        .map_err(io)?;
    std::fs::write(seed_dir.join("SITE.md"), "崩溃测试站点骨架\n").map_err(io)?;
    std::fs::write(seed_dir.join("src/content/blog/.gitkeep"), "").map_err(io)?;

    crate::git::git(&seed_dir, &["init", "--initial-branch=main"])?;
    local_identity(&seed_dir)?;
    crate::git::git(&seed_dir, &["add", "-A"])?;
    crate::git::git(&seed_dir, &["commit", "-m", "崩溃测试站点骨架"])?;
    crate::git::git(
        &seed_dir,
        &[
            "remote",
            "add",
            "origin",
            &format!("file://{}", remote.to_string_lossy().replace('\\', "/")),
        ],
    )?;
    crate::git::git(&seed_dir, &["push", "origin", "main"])?;

    crate::git::git(
        root,
        &[
            "clone",
            "--branch",
            "main",
            "--single-branch",
            &format!("file://{}", remote.to_string_lossy().replace('\\', "/")),
            &workspace.to_string_lossy(),
        ],
    )?;
    local_identity(workspace)?;
    Ok(())
}

/// 在测试仓库内写一次性提交身份（不改全局 Git 配置）。
fn local_identity(repo: &Path) -> Result<()> {
    crate::git::git(repo, &["config", "user.name", "观澜志崩溃测试"])?;
    crate::git::git(repo, &["config", "user.email", "crash-test@example.invalid"])?;
    crate::git::git(repo, &["config", "commit.gpgsign", "false"])?;
    Ok(())
}

/// 读取远端分支头（分支不存在时返回 `None`）。
fn branch_head(state: &core::AppState, config: &AppConfig, branch: &str) -> Option<String> {
    let workspace = Workspace::open(PathBuf::from(&config.workspace_dir)).ok()?;
    let engine = crate::sync::SyncEngine::new(&workspace, &state.store, &config.repo_url);
    engine.fetch(branch).ok().flatten()
}

/// `--crash-session`：建立环境、写一篇已保存的文章，再留下**未落盘**的编辑，然后挂起。
///
/// 挂起是刻意的：外部测试脚本需要对它执行强制结束，才能复现「崩溃」。
/// 这里不注册任何退出处理，因此被强杀时不会有任何优雅清理。
pub fn run_crash_session(data_dir: &Path) -> Result<()> {
    let (store, config) = open_env(data_dir)?;
    let state = core::AppState::with_store(store);
    let workspace = Workspace::open(PathBuf::from(&config.workspace_dir))?;

    // 1. 新文章并**保存**（这一步会落盘）。
    let saved_meta = ArticleMeta {
        title: SAVED_TITLE.to_string(),
        description: "异常关闭恢复测试用的样稿。".to_string(),
        pub_date: "2026-01-01".to_string(),
        updated_date: None,
        tags: vec!["测试".to_string()],
        draft: true,
    };
    match core::create_article(
        &state,
        CRASH_ARTICLE_ID.to_string(),
        saved_meta.clone(),
        SAVED_BODY.to_string(),
    ) {
        Ok(_) => {}
        // 重复运行同一目录时文章已存在，继续即可。
        Err(err) if err.code == ErrorCode::ArticleExists => {}
        Err(err) => return Err(err),
    }

    // 2. 模拟「用户继续编辑但还没落盘」：只写恢复副本，不写文章文件。
    let unsaved_meta = ArticleMeta { title: UNSAVED_TITLE.to_string(), ..saved_meta };
    core::snapshot_recovery(
        &state,
        CRASH_ARTICLE_ID.to_string(),
        unsaved_meta,
        UNSAVED_BODY.to_string(),
    )?;

    // 3. 明确把状态打到 stdout，测试脚本据此判断「可以杀了」。
    println!("CRASH_SESSION_READY");
    println!("data_dir={}", data_dir.display());
    println!("workspace={}", workspace.root().display());
    println!("article={CRASH_ARTICLE_ID}");
    use std::io::Write;
    std::io::stdout().flush().ok();

    // 4. 挂起等待被强杀。不注册清理，模拟真实的进程被结束。
    loop {
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
}

/// `--inspect-recovery`：只读检查，供测试脚本在被强杀之后调用。
pub fn inspect_recovery(data_dir: &Path) -> Result<RecoveryInspection> {
    let (store, config) = open_env(data_dir)?;
    let state = core::AppState::with_store(store);
    let workspace = Workspace::open(PathBuf::from(&config.workspace_dir))?;

    let pending = core::pending_recovery(&state)?;
    let pending_article_ids: Vec<String> =
        pending.iter().map(|draft| draft.article_id.clone()).collect();
    let pending_contains_unsaved_body =
        pending.iter().any(|draft| draft.markdown.contains(UNSAVED_BODY.trim()));

    // 磁盘上的文章（可能存在解析失败的情况，这里按原文判断）。
    let disk_raw = std::fs::read_to_string(
        workspace.root().join("src/content/blog").join(format!("{CRASH_ARTICLE_ID}.md")),
    )
    .unwrap_or_default();
    let disk_lacks_unsaved_body = !disk_raw.contains(UNSAVED_BODY.trim());
    let disk_title = crate::article_io::parse_markdown(&disk_raw)
        .ok()
        .and_then(|parsed| crate::article_io::parse_front_matter_map(&parsed.front_matter).ok())
        .and_then(|map| crate::article_io::meta_from_map(&map).ok())
        .map(|meta| meta.title);

    Ok(RecoveryInspection {
        pending_article_ids,
        pending_contains_unsaved_body,
        disk_lacks_unsaved_body,
        disk_title,
        writing_head: branch_head(&state, &config, crate::sync::WRITING_BRANCH),
        main_head: branch_head(&state, &config, crate::sync::MAIN_BRANCH),
        toolchain: crate::preview::check_toolchain(),
    })
}

/// `--restore-recovery`：执行恢复并报告结果。
pub fn run_restore_recovery(data_dir: &Path, article_id: &str) -> Result<RestoreResult> {
    let (store, config) = open_env(data_dir)?;
    let state = core::AppState::with_store(store);

    let restored = core::restore_recovery(&state, article_id.to_string())?;
    let pending_after: Vec<String> = core::pending_recovery(&state)?
        .iter()
        .map(|draft| draft.article_id.clone())
        .collect();

    Ok(RestoreResult {
        article_id: article_id.to_string(),
        title: restored.meta.title,
        body: restored.body,
        pending_after,
        writing_head: branch_head(&state, &config, crate::sync::WRITING_BRANCH),
        main_head: branch_head(&state, &config, crate::sync::MAIN_BRANCH),
    })
}

/// 定位崩溃测试用的默认数据目录（测试脚本会显式传入，这里仅作兜底）。
pub fn default_data_dir() -> PathBuf {
    crate::git::temp_path("crash-recovery")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 进程内版本：建立环境、留下未落盘编辑，然后（在另一个 AppState 上）检查与恢复。
    ///
    /// 真正的「外部强杀」由 `scripts/test/crash-recovery.mjs` 驱动真实进程完成；
    /// 这里验证的是同一条代码路径在进程内的行为。
    #[test]
    fn recovery_survives_a_fresh_store_handle() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().join("data");

        // 建立环境并留下未落盘的编辑（不进入挂起循环）。
        {
            let (store, config) = open_env(&data_dir).unwrap();
            let state = core::AppState::with_store(store);
            let meta = ArticleMeta {
                title: SAVED_TITLE.to_string(),
                description: "摘要".to_string(),
                pub_date: "2026-01-01".to_string(),
                updated_date: None,
                tags: vec![],
                draft: true,
            };
            core::create_article(&state, CRASH_ARTICLE_ID.to_string(), meta.clone(), SAVED_BODY.to_string())
                .unwrap();
            core::snapshot_recovery(
                &state,
                CRASH_ARTICLE_ID.to_string(),
                ArticleMeta { title: UNSAVED_TITLE.to_string(), ..meta },
                UNSAVED_BODY.to_string(),
            )
            .unwrap();
            let _ = config;
        }

        // 模拟「重新打开软件」：全新的 store 句柄读同一目录。
        let inspection = inspect_recovery(&data_dir).unwrap();
        assert_eq!(inspection.pending_article_ids, vec![CRASH_ARTICLE_ID.to_string()]);
        assert!(inspection.pending_contains_unsaved_body, "恢复副本应含未保存正文");
        assert!(inspection.disk_lacks_unsaved_body, "磁盘不应含未保存正文");
        assert_eq!(inspection.disk_title.as_deref(), Some(SAVED_TITLE));
        assert!(inspection.writing_head.is_none(), "编辑不该产生 writing 提交");
        assert!(inspection.writing_head.is_none() && inspection.main_head.is_some());

        // 恢复。
        let restored = run_restore_recovery(&data_dir, CRASH_ARTICLE_ID).unwrap();
        assert_eq!(restored.title, UNSAVED_TITLE);
        assert!(restored.body.contains(UNSAVED_BODY.trim()));
        assert!(restored.pending_after.is_empty(), "恢复后不应再有待处理副本");
    }

    /// 环境可重复使用：第二次调用不应重复初始化或失败。
    #[test]
    fn open_env_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().join("data");
        let (_, first) = open_env(&data_dir).unwrap();
        let (_, second) = open_env(&data_dir).unwrap();
        assert_eq!(first.workspace_dir, second.workspace_dir);
    }
}
