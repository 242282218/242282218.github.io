//! Tauri 命令层：应用状态与全部对外命令。
//!
//! 这是 webview 能触达的**唯一**业务入口。所有参数都被强类型化并校验；
//! 命令内部只调用受管模块，不开放任意 shell、任意文件系统读写或任意 Git 命令。

use crate::commands::{
    run_exclusive, validate_article_id, validate_meta, validate_new_article_id, TaskKind, TaskQueue,
};
use crate::connection::Connector;
use crate::images;
use crate::local_store::{AppConfig, LocalStore, RecoveryDraft, TrashEntry, WritingPreferences};
use crate::model::{
    ArticleContent, ArticleMeta, ArticleSummary, ArticleStatus, ErrorCode, Result, WriterError,
};
use crate::paths;
use crate::preview::{PreviewEngine, PreviewOverlay, PreviewServer, ToolchainReport};
use crate::publish::{
    PublishEngine, PublishOutcome, PublishPrecheck, WorkflowConclusion,
};
use crate::sync::{SyncAssessment, SyncEngine, SyncOutcome, MAIN_BRANCH};
use crate::trash::{DeleteAssessment, DeleteOutcome, TrashEngine, WithdrawOutcome};
use crate::workspace::{RemoteSnapshot, Workspace};
use crate::util;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// 全局应用状态。
pub struct AppState {
    pub store: LocalStore,
    pub queue: TaskQueue,
    /// 当前活动的预览服务（单实例：新的预览会替换旧的）。
    pub preview: Arc<Mutex<Option<PreviewServer>>>,
    /// 工作区排他锁，防止第二个软件实例同时写入。
    pub instance_lock: Option<crate::local_store::InstanceLock>,
}

impl AppState {
    pub fn new() -> Result<Self> {
        let store = LocalStore::open_default()?;
        let instance_lock = match store.acquire_instance_lock() {
            Ok(lock) => Some(lock),
            Err(err) => return Err(err),
        };
        Ok(Self {
            store,
            queue: TaskQueue::new(),
            preview: Arc::new(Mutex::new(None)),
            instance_lock,
        })
    }

    /// 供测试与嵌入场景使用：指定应用数据目录。
    pub fn with_store(store: LocalStore) -> Self {
        Self {
            store,
            queue: TaskQueue::new(),
            preview: Arc::new(Mutex::new(None)),
            instance_lock: None,
        }
    }

    pub fn connector(&self) -> Connector {
        Connector::new(self.store.clone_handle())
    }

    fn config(&self) -> AppConfig {
        self.store.load_config()
    }

    fn workspace(&self) -> Result<Workspace> {
        let config = self.config();
        if config.workspace_dir.is_empty() {
            return Err(WriterError::new(
                ErrorCode::InvalidArgument,
                "尚未完成首次连接，请先选择一个工作目录",
            ));
        }
        Workspace::open(PathBuf::from(&config.workspace_dir))
    }

    fn sync_engine<'w>(&self, workspace: &'w Workspace, config: &AppConfig) -> SyncEngine<'w, '_> {
        SyncEngine::new(workspace, &self.store, &config.repo_url)
    }

    fn publish_engine<'w>(&self, workspace: &'w Workspace, config: &AppConfig) -> PublishEngine<'w, '_> {
        PublishEngine::new(workspace, &self.store, &config.repo_url)
    }

    fn trash_engine<'w>(&self, workspace: &'w Workspace, config: &AppConfig) -> TrashEngine<'w, '_> {
        TrashEngine::new(workspace, &self.store, &config.repo_url)
    }

    /// 读取远端快照用于状态推导（离线时返回空快照，状态退化为本地视角）。
    fn remote_snapshots(&self, workspace: &Workspace, config: &AppConfig) -> BTreeMap<String, RemoteSnapshot> {
        let engine = self.sync_engine(workspace, config);
        let mut out = BTreeMap::new();
        let writing_head = engine.fetch(crate::sync::WRITING_BRANCH).ok().flatten();
        let main_head = engine.fetch(MAIN_BRANCH).ok().flatten();
        if writing_head.is_none() && main_head.is_none() {
            return out;
        }

        let ids: Vec<String> = workspace
            .list_markdown_rel_paths()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|rel| paths::article_id_from_rel_path(&rel).ok())
            .collect();

        for article_id in ids {
            let baseline = self.store.load_versions().get(&article_id).cloned().unwrap_or_default();
            // 该文章在各分支上的原文；`None` 表示该分支上没有这篇文章。
            let writing_text = writing_head
                .as_deref()
                .and_then(|rev| engine.markdown_at(rev, &article_id).ok().flatten());
            let main_text = main_head
                .as_deref()
                .and_then(|rev| engine.markdown_at(rev, &article_id).ok().flatten());

            let writing_hash = writing_text.as_deref().map(|t| util::hash_bytes(t.as_bytes()));
            let main_hash = main_text.as_deref().map(|t| util::hash_bytes(t.as_bytes()));
            let main_site_hash = main_text.as_deref().map(crate::workspace::site_hash_of);
            let main_published = main_text.as_deref().and_then(|t| {
                let parsed = crate::article_io::parse_markdown(t).ok()?;
                let map = crate::article_io::parse_front_matter_map(&parsed.front_matter).ok()?;
                let meta = crate::article_io::meta_from_map(&map).ok()?;
                Some(!meta.draft)
            });

            // `main_commit` 表示「该文章当前 main 版本所在的提交」；文章不在
            // `main` 上时必须为 `None`，否则状态推导会把从未发布的文章
            // 显示成「已提交发布」。
            let main_commit = if main_text.is_some() { main_head.clone() } else { None };
            let deployment_url =
                if main_text.is_some() { baseline.deployment_url.clone() } else { None };

            out.insert(
                article_id.clone(),
                RemoteSnapshot {
                    writing_hash,
                    main_hash,
                    main_site_hash,
                    main_commit,
                    main_published,
                    // 部署结论只来自**已确认的工作流查询**在基线上的落盘记录，
                    // 不再由「推送成功」推导。
                    deployed_commit: baseline.deployed_commit.clone(),
                    deploy_failed_commit: baseline.deploy_failed_commit.clone(),
                    deploying_commit: baseline.deploying_commit.clone(),
                    deployment_url,
                },
            );
        }
        out
    }
}

/// 应用数据目录的克隆句柄。
impl LocalStore {
    /// 以同一路径复制一个句柄（`LocalStore` 只持有路径，可安全复制）。
    pub fn clone_handle(&self) -> LocalStore {
        LocalStore::open_at(self.root().to_path_buf()).expect("应用数据目录已存在")
    }
}

// ---------------------------------------------------------------------------
// 命令实现（供 Tauri 包装层与测试直接调用）
// ---------------------------------------------------------------------------

/// 首次连接状态。
pub fn connection_status(state: &AppState) -> Result<crate::connection::ConnectionStatus> {
    Ok(state.connector().status())
}

/// 确认公开性说明。
pub fn acknowledge_disclosure(state: &AppState) -> Result<crate::connection::ConnectionStatus> {
    state.connector().acknowledge_disclosure()?;
    connection_status(state)
}

/// 建立独立工作目录。
pub fn connect(
    state: &AppState,
    workspace_dir: Option<String>,
) -> Result<crate::connection::ConnectionStatus> {
    let dir = match workspace_dir {
        None => None,
        Some(raw) if raw.trim().is_empty() => None,
        Some(raw) => Some(crate::connection::normalize_workspace_dir(&raw)?),
    };
    state.connector().connect(dir)
}

/// 能力检测（Git / Node / pnpm）。
pub fn toolchain_report(state: &AppState) -> ToolchainReport {
    let _ = state;
    crate::preview::check_toolchain()
}

/// 文章列表（带三处状态）。
pub fn list_articles(state: &AppState) -> Result<Vec<ArticleSummary>> {
    let workspace = state.workspace()?;
    let config = state.config();
    let snapshots = state.remote_snapshots(&workspace, &config);
    workspace.scan(&state.store, &snapshots)
}

/// 读取单篇文章。
pub fn read_article(state: &AppState, article_id: String) -> Result<ArticleContent> {
    validate_article_id(&article_id)?;
    let workspace = state.workspace()?;
    let config = state.config();
    let snapshots = state.remote_snapshots(&workspace, &config);
    workspace.read(&article_id, &snapshots)
}

/// 新建文章：默认 `draft: true`，在第一次发布时才切换主站文件。
pub fn create_article(
    state: &AppState,
    article_id: String,
    meta: ArticleMeta,
    body: String,
) -> Result<ArticleContent> {
    validate_new_article_id(&article_id)?;
    let mut meta = meta;
    // 新建文章默认不公开。
    meta.draft = true;
    validate_meta(&meta)?;
    let workspace = state.workspace()?;
    let created = workspace.create(&article_id, &meta, &body)?;
    crate::workspace::touch_edited(&state.store, &article_id)?;
    // 记录本地编辑起始点。
    state.sync_engine(&workspace, &state.config()).record_edit_base(&article_id)?;
    Ok(created)
}

/// 保存文章（本地阶段）。
///
/// `updated_date_action`：`None` 不改动 `updatedDate`（自动保存的默认行为）；
/// `Some(Set)` 写入用户确认的日期；`Some(Remove)` 删除该字段。
pub fn save_article(
    state: &AppState,
    article_id: String,
    meta: ArticleMeta,
    body: String,
    updated_date_action: Option<crate::model::UpdatedDateAction>,
) -> Result<ArticleContent> {
    use crate::model::UpdatedDateAction;
    validate_article_id(&article_id)?;
    let mut meta = meta;
    // 空字符串视为未设置。
    if meta.updated_date.as_deref().map(str::trim).unwrap_or("").is_empty() {
        meta.updated_date = None;
    }
    validate_meta(&meta)?;
    let workspace = state.workspace()?;
    let write_action: Option<Option<&str>> = match &updated_date_action {
        None => None,
        Some(UpdatedDateAction::Set { value }) => Some(Some(value.as_str())),
        Some(UpdatedDateAction::Remove) => Some(None),
    };

    let saved = run_exclusive(&state.queue, TaskKind::SaveLocal, &article_id, || {
        // 保存前先写一份崩溃恢复副本，成功后再清除，保证异常退出可恢复。
        let existing = workspace.read_raw(&article_id)?.and_then(|bytes| String::from_utf8(bytes).ok());
        if let Some(text) = &existing {
            let _ = state.store.write_recovery(&RecoveryDraft {
                article_id: article_id.clone(),
                markdown_rel_path: format!("{}{}.md", paths::BLOG_DIR_PREFIX, article_id),
                markdown: text.clone(),
                saved_at_unix: util::unix_seconds(),
            });
        }
        let result = workspace.save(&article_id, &meta, &body, write_action)?;
        state.store.clear_recovery(&article_id);
        Ok(result)
    })?;
    crate::workspace::touch_edited(&state.store, &article_id)?;
    Ok(saved)
}

/// 把用户选择的图片文件插入文章：校验后归档到 `public/blog/<article-id>/`。
///
/// 返回可直接写进 Markdown 的站点根路径与文件信息；调用方负责把引用插入正文。
/// 只复制该文件，不改动源文件，也不触碰其它目录。
pub fn import_article_image(
    state: &AppState,
    article_id: String,
    source_path: String,
) -> Result<ImportedImage> {
    validate_article_id(&article_id)?;
    let source = PathBuf::from(source_path.trim());
    if !source.is_absolute() {
        return Err(WriterError::new(
            ErrorCode::InvalidArgument,
            "请选择要插入的图片文件（绝对路径）",
        ));
    }

    // 读取并校验：只接受经文件头确认的 PNG/JPEG/WebP/GIF，且不超过大小上限。
    let bytes = std::fs::read(&source).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            WriterError::new(ErrorCode::ArticleNotFound, "要插入的图片文件不存在")
        } else {
            WriterError::new(ErrorCode::IoFailed, format!("读取图片失败：{e}"))
        }
    })?;

    let basename_source = source
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("figure")
        .to_string();
    import_article_image_data(state, article_id, &basename_source, &bytes)
}

/// 把已在内存中的图片字节插入文章（用于剪贴板粘贴、浏览器拖入等没有磁盘路径的来源）。
///
/// 与 [`import_article_image`] 走完全相同的校验与归档路径，因此校验强度一致：
/// 只接受经文件头确认的 PNG/JPEG/WebP/GIF，超过大小上限直接拒绝。
pub fn import_article_image_data(
    state: &AppState,
    article_id: String,
    basename_source: &str,
    bytes: &[u8],
) -> Result<ImportedImage> {
    validate_article_id(&article_id)?;
    let workspace = state.workspace()?;

    let kind = util::detect_image(bytes)?;
    let (rel_path, url) =
        images::import_image_bytes(workspace.root(), &article_id, basename_source, bytes)?;
    let file_name = std::path::Path::new(&rel_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("figure")
        .to_string();

    Ok(ImportedImage {
        article_id,
        rel_path,
        url,
        file_name,
        size: bytes.len() as u64,
        content_hash: util::hash_bytes(bytes),
        mime: kind.mime().to_string(),
    })
}

/// 列出某篇文章已归档的图片。
pub fn list_article_images(state: &AppState, article_id: String) -> Result<Vec<ImportedImage>> {
    validate_article_id(&article_id)?;
    let workspace = state.workspace()?;
    let mut out = Vec::new();
    for image in images::article_images(workspace.root(), &article_id)? {
        let url = images::url_path_for(&image.rel_path)?;
        let file_name = std::path::Path::new(&image.rel_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("figure")
            .to_string();
        out.push(ImportedImage {
            article_id: article_id.clone(),
            rel_path: image.rel_path,
            url,
            file_name,
            size: image.size,
            content_hash: image.content_hash,
            mime: String::new(),
        });
    }
    Ok(out)
}

/// 已被归档到文章目录的图片。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportedImage {
    pub article_id: String,
    /// 仓库内相对路径，如 `public/blog/read-code/figure-01-abcd1234.png`。
    pub rel_path: String,
    /// 站点根路径，写入 Markdown 的形式，如 `/blog/read-code/figure-01-abcd1234.png`。
    pub url: String,
    pub file_name: String,
    pub size: u64,
    pub content_hash: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub mime: String,
}

/// 显式导入一篇已有的本地 Markdown 文件。
pub fn import_article(
    state: &AppState,
    source_path: String,
    article_id: String,
) -> Result<ArticleContent> {
    validate_new_article_id(&article_id)?;
    let source = PathBuf::from(source_path.trim());
    if !source.is_absolute() {
        return Err(WriterError::new(
            ErrorCode::InvalidArgument,
            "请选择要导入的 Markdown 文件（绝对路径）",
        ));
    }
    let workspace = state.workspace()?;
    let imported = workspace.import_markdown_file(&source, &article_id)?;
    crate::workspace::touch_edited(&state.store, &article_id)?;
    Ok(imported)
}

/// 评估一篇文章能否安全同步（差异界面数据源）。
pub fn assess_sync(state: &AppState, article_id: String) -> Result<SyncAssessment> {
    validate_article_id(&article_id)?;
    let workspace = state.workspace()?;
    state.sync_engine(&workspace, &state.config()).assess(&article_id)
}

/// 同步到写作分支。`adopt_local` 为 true 表示用户已在差异界面选择「采用本地」。
pub fn sync_article(
    state: &AppState,
    article_id: String,
    adopt_local: bool,
) -> Result<SyncOutcome> {
    validate_article_id(&article_id)?;
    let workspace = state.workspace()?;
    let config = state.config();
    let engine = state.sync_engine(&workspace, &config);
    let title = workspace
        .read(&article_id, &BTreeMap::new())
        .map(|c| c.meta.title)
        .unwrap_or_else(|_| article_id.clone());
    let message = format!("远程保存：{title}");
    run_exclusive(&state.queue, TaskKind::Sync, &article_id, || {
        engine.push_article(&article_id, &message, adopt_local)
    })
}

/// 放弃本地修改，采用远端版本（先把本地改动放入恢复副本）。
pub fn adopt_remote(state: &AppState, article_id: String) -> Result<ArticleContent> {
    validate_article_id(&article_id)?;
    let workspace = state.workspace()?;
    let config = state.config();
    let engine = state.sync_engine(&workspace, &config);

    // 先把未保存的本地修改放入恢复副本。
    if let Some(bytes) = workspace.read_raw(&article_id)? {
        if let Ok(text) = String::from_utf8(bytes) {
            state.store.write_recovery(&RecoveryDraft {
                article_id: article_id.clone(),
                markdown_rel_path: format!("{}{}.md", paths::BLOG_DIR_PREFIX, article_id),
                markdown: text,
                saved_at_unix: util::unix_seconds(),
            })?;
        }
    }

    let head = engine.fetch(crate::sync::WRITING_BRANCH)?.ok_or_else(|| {
        WriterError::new(ErrorCode::RemoteChanged, "写作分支不存在，无法采用远端版本")
    })?;
    let remote = engine.markdown_at(&head, &article_id)?.ok_or_else(|| {
        WriterError::new(ErrorCode::RemoteChanged, "远端没有这篇文章，无法采用远端版本")
    })?;
    workspace.write_raw(&article_id, remote.as_bytes())?;

    // 采用远端后，本地与远端一致，更新基线。
    engine.record_sync_success(&article_id, &head)?;
    // 注意：这里**不**清除刚写入的恢复副本。采用远端等于丢弃用户本地的修改，
    // 那份副本是用户唯一能取回自己改动的入口，必须保留到用户显式丢弃。
    let snapshots = state.remote_snapshots(&workspace, &config);
    workspace.read(&article_id, &snapshots)
}

/// 手动合并后保存：把用户编辑的结果写盘并清除冲突。
pub fn resolve_conflict_manually(
    state: &AppState,
    article_id: String,
    meta: ArticleMeta,
    body: String,
) -> Result<ArticleContent> {
    save_article(state, article_id, meta, body, None)
}

/// 发布预检。
pub fn publish_precheck(state: &AppState, article_id: String) -> Result<PublishPrecheck> {
    validate_article_id(&article_id)?;
    let workspace = state.workspace()?;
    state.publish_engine(&workspace, &state.config()).precheck(&article_id)
}

/// 执行按篇发布。
pub fn publish_article(
    state: &AppState,
    article_id: String,
    removed_image_paths: Vec<String>,
    remove_old_markdown_path: Option<String>,
) -> Result<PublishOutcome> {
    validate_article_id(&article_id)?;
    let workspace = state.workspace()?;
    let config = state.config();
    let engine = state.publish_engine(&workspace, &config);
    let title = workspace
        .read(&article_id, &BTreeMap::new())
        .map(|c| c.meta.title)
        .unwrap_or_else(|_| article_id.clone());
    let message = crate::publish::publish_commit_message(&title);
    run_exclusive(&state.queue, TaskKind::Publish, &article_id, || {
        engine.publish(
            &article_id,
            &message,
            &removed_image_paths,
            remove_old_markdown_path.as_deref(),
        )
    })
}

/// 撤下。
pub fn withdraw_article(state: &AppState, article_id: String) -> Result<WithdrawOutcome> {
    validate_article_id(&article_id)?;
    let workspace = state.workspace()?;
    let config = state.config();
    let engine = state.trash_engine(&workspace, &config);
    run_exclusive(&state.queue, TaskKind::Withdraw, &article_id, || engine.withdraw(&article_id))
}

/// 删除影响评估。
pub fn assess_delete(state: &AppState, article_id: String) -> Result<DeleteAssessment> {
    validate_article_id(&article_id)?;
    let workspace = state.workspace()?;
    state.trash_engine(&workspace, &state.config()).assess_delete(&article_id)
}

/// 删除文件（两个分支 + 本地回收副本）。
pub fn delete_article(state: &AppState, article_id: String) -> Result<DeleteOutcome> {
    validate_article_id(&article_id)?;
    let workspace = state.workspace()?;
    let config = state.config();
    let engine = state.trash_engine(&workspace, &config);
    run_exclusive(&state.queue, TaskKind::Delete, &article_id, || engine.delete(&article_id))
}

/// 重试未完成的多分支删除。
pub fn retry_delete(state: &AppState, op_id: String) -> Result<DeleteOutcome> {
    let workspace = state.workspace()?;
    let engine = state.trash_engine(&workspace, &state.config());
    engine.retry_delete(&op_id)
}

/// 从回收区恢复为未发布草稿。
pub fn restore_article(state: &AppState, op_id: String) -> Result<ArticleContent> {
    let workspace = state.workspace()?;
    let config = state.config();
    let engine = state.trash_engine(&workspace, &config);
    run_exclusive(&state.queue, TaskKind::Restore, &op_id, || engine.restore(&op_id))
}

/// 回收区列表。
pub fn list_trash(state: &AppState) -> Result<Vec<TrashEntry>> {
    let workspace = state.workspace()?;
    Ok(state.trash_engine(&workspace, &state.config()).list())
}

/// 清理回收记录。
pub fn purge_trash(state: &AppState, op_id: String) -> Result<()> {
    let workspace = state.workspace()?;
    state.trash_engine(&workspace, &state.config()).purge(&op_id)
}

/// 待处理的崩溃恢复副本。
///
/// 只返回**与磁盘内容不同**的副本：内容已经落盘的副本不再打扰用户，
/// 避免「已恢复但提示仍在」这类误导。
pub fn pending_recovery(state: &AppState) -> Result<Vec<RecoveryDraft>> {
    let drafts = state.store.list_recovery();
    let workspace = state.workspace().ok();
    Ok(drafts
        .into_iter()
        .filter(|draft| match &workspace {
            Some(workspace) => match workspace.read_raw(&draft.article_id) {
                Ok(Some(bytes)) => bytes != draft.markdown.as_bytes(),
                // 文件不存在（例如文章已删除）时保留提示，让用户能取回内容。
                _ => true,
            },
            None => true,
        })
        .collect())
}

/// 丢弃一份崩溃恢复副本。
pub fn discard_recovery(state: &AppState, article_id: String) -> Result<()> {
    validate_article_id(&article_id)?;
    state.store.clear_recovery(&article_id);
    Ok(())
}

/// 记录一份崩溃恢复副本。
///
/// 前端在编辑过程中轻量防抖调用：把**当前内存中**的元数据与正文写入本机恢复区。
/// 这样即使进程被强制结束（来不及执行优雅关闭时的 flush），下一次启动也能提示
/// 并恢复这部分内容，而不是只保留上次落盘的结果。
pub fn snapshot_recovery(
    state: &AppState,
    article_id: String,
    meta: ArticleMeta,
    body: String,
) -> Result<()> {
    validate_article_id(&article_id)?;
    // 这里不校验必填字段：编辑中途标题可能还是空的，草稿快照仍应保存下来。
    let markdown = crate::article_io::compose_markdown(&meta, &body);
    state.store.write_recovery(&RecoveryDraft {
        article_id: article_id.clone(),
        markdown_rel_path: format!("{}{}.md", paths::BLOG_DIR_PREFIX, article_id),
        markdown,
        saved_at_unix: util::unix_seconds(),
    })
}

/// 读取某篇文章的恢复副本（用于界面展示「未保存内容」的详情）。
pub fn read_recovery(state: &AppState, article_id: String) -> Result<Option<RecoveryDraft>> {
    validate_article_id(&article_id)?;
    Ok(state.store.read_recovery(&article_id))
}

/// 用恢复副本覆盖文章文件（用户显式确认后调用）。
///
/// 这是**用户主动发起的恢复动作**：把恢复副本的内容写回文章文件。
///
/// 两个刻意的选择：
/// - 覆盖前先确认恢复内容能被解析，解析不了就拒绝写入，宁可保持现状也不覆盖成坏内容；
/// - **保留**恢复副本本身。它是崩溃时用户唯一的内容来源，只有在后续一次成功保存
///   或用户显式丢弃时才清除；这样恢复错了还能再取回。
///
/// 它只改本地文件，不触碰 `writing` 或 `main`。
pub fn restore_recovery(state: &AppState, article_id: String) -> Result<ArticleContent> {
    validate_article_id(&article_id)?;
    let draft = state.store.read_recovery(&article_id).ok_or_else(|| {
        WriterError::new(ErrorCode::ArticleNotFound, "这篇文章没有可恢复的未保存内容")
    })?;

    // 恢复副本必须先能解析，否则不覆盖磁盘。
    let parsed = crate::article_io::parse_markdown(&draft.markdown)?;
    let map = crate::article_io::parse_front_matter_map(&parsed.front_matter)?;
    crate::article_io::meta_from_map(&map)?;

    let workspace = state.workspace()?;
    workspace.write_raw(&article_id, draft.markdown.as_bytes())?;

    state.sync_engine(&workspace, &state.config()).record_edit_base(&article_id)?;
    crate::workspace::touch_edited(&state.store, &article_id)?;

    let config = state.config();
    let snapshots = state.remote_snapshots(&workspace, &config);
    workspace.read(&article_id, &snapshots)
}

/// 校验 URL 改名的影响（旧/新 URL、外链风险、冲突）。
pub fn assess_rename_url(
    state: &AppState,
    old_id: String,
    new_id: String,
) -> Result<RenameAssessment> {
    validate_article_id(&old_id)?;
    validate_new_article_id(&new_id)?;
    let workspace = state.workspace()?;

    if let Some(existing) = workspace.check_id_available(&new_id)? {
        return Err(WriterError::new(
            ErrorCode::ArticleExists,
            format!("目标 URL 标识已被占用：{existing}"),
        )
        .with_detail(existing));
    }

    let config = state.config();
    let publish = state.publish_engine(&workspace, &config);
    let main_published = publish.main_published_state(&old_id)?.unwrap_or(false);

    // 扫描站内对旧 URL 的引用。
    let old_url = format!("/blog/{old_id}/");
    let old_url_exact = format!("/blog/{old_id}/");
    let texts = workspace.markdown_texts()?;
    let referencing: Vec<String> = texts
        .iter()
        .filter(|(_, text)| text.contains(&old_url) || text.contains(&old_url_exact))
        .map(|(id, _)| id.clone())
        .collect();

    Ok(RenameAssessment {
        old_id: old_id.clone(),
        new_id: new_id.clone(),
        old_url: format!("/blog/{old_id}/"),
        new_url: format!("/blog/{new_id}/"),
        published: main_published,
        referencing_articles: referencing,
        requires_republish: main_published,
    })
}

/// 执行 URL 改名。
pub fn rename_article_url(
    state: &AppState,
    old_id: String,
    new_id: String,
    confirm_published: bool,
) -> Result<ArticleContent> {
    let assessment = assess_rename_url(state, old_id.clone(), new_id.clone())?;
    if assessment.requires_republish && !confirm_published {
        return Err(WriterError::new(
            ErrorCode::InvalidArgument,
            "该文章已发布，改 URL 会让旧链接失效，需要再次确认",
        ));
    }
    let workspace = state.workspace()?;
    let config = state.config();
    let engine = state.sync_engine(&workspace, &config);

    run_exclusive(&state.queue, TaskKind::RenameUrl, &old_id, || {
        workspace.rename_article_id(&old_id, &new_id)?;
        // 迁移本地基线记录。
        let baseline = state.store.load_versions().get(&old_id).cloned().unwrap_or_default();
        state.store.update_baseline(&new_id, |b| *b = baseline.clone())?;
        engine.record_edit_base(&new_id)?;
        Ok(())
    })?;

    workspace.read(&new_id, &BTreeMap::new())
}

/// 作品外观偏好读取。
pub fn get_preferences(state: &AppState) -> WritingPreferences {
    state.config().preferences
}

/// 写入写作外观偏好（只影响本机，不污染文章）。
pub fn set_preferences(state: &AppState, preferences: WritingPreferences) -> Result<WritingPreferences> {
    let mut config = state.config();
    config.preferences = preferences;
    config.preferences.clamp();
    state.store.save_config(&config)?;
    Ok(state.config().preferences)
}

/// 启动网站预览。
///
/// 在隔离临时副本中覆盖当前文章内容，只绑定 `127.0.0.1`。
pub fn start_site_preview(
    state: &AppState,
    article_id: String,
    simulate_public: bool,
) -> Result<PreviewSessionInfo> {
    validate_article_id(&article_id)?;
    let workspace = state.workspace()?;
    let config = state.config();
    let engine = PreviewEngine::new(&workspace, state.connector().preview_temp_root(), &config.repo_url);

    let article = workspace.read(&article_id, &BTreeMap::new())?;
    // 覆盖内容 = 本地当前内容 + 全部专属图片。
    let mut image_bytes = BTreeMap::new();
    for image in images::article_images(workspace.root(), &article_id)? {
        if let Ok(bytes) = std::fs::read(workspace.root().join(&image.rel_path)) {
            image_bytes.insert(image.rel_path.clone(), bytes);
        }
    }
    let markdown = format!(
        "---\n{}\n---\n{}",
        article.raw_front_matter, article.body
    );
    let overlay = PreviewOverlay {
        article_id: article_id.clone(),
        markdown,
        images: image_bytes,
        simulate_public,
    };
    let _ = config;

    run_exclusive(&state.queue, TaskKind::SitePreview, "site", || {
        let worktree = engine.prepare_worktree(&paths::case_insensitive_key(&article_id).replace('/', "-"))?;
        engine.apply_overlay(&worktree, &overlay)?;

        // 依赖：优先复用工作区已安装的那份（目录联接），避免重复下载。
        let reused = engine.ensure_dependencies(&worktree)?;
        if reused {
            let _ = reused;
        }

        let server = engine.start(&worktree)?;
        let url = server.url().to_string();
        let temp_root = server.temp_root().to_string_lossy().to_string();
        // 替换旧的预览服务（先停旧的再存新的）。
        let mut slot = state.preview.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(old) = slot.take() {
            old.shutdown();
        }
        *slot = Some(server);
        Ok(PreviewSessionInfo {
            url,
            banner: crate::preview::PREVIEW_BANNER.to_string(),
            temp_root,
            offline_font_notice: Some(crate::preview::OFFLINE_FONT_NOTICE.to_string()),
            simulate_public,
        })
    })
}

/// 关闭网站预览并清理临时目录。
pub fn stop_site_preview(state: &AppState) -> Result<()> {
    let mut slot = state.preview.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(server) = slot.take() {
        server.shutdown();
    }
    Ok(())
}

/// 查询发布对应的工作流与部署结论。
///
/// 只用**无需凭据的只读 API**；无法查询时返回 `None`，界面显示「部署状态待确认」，
/// 不凭推送成功猜测上线。
pub fn deployment_status(state: &AppState, article_id: String) -> Result<DeploymentStatus> {
    validate_article_id(&article_id)?;
    let workspace = state.workspace()?;
    let config = state.config();
    let engine = state.publish_engine(&workspace, &config);

    let baseline = state.store.load_versions().get(&article_id).cloned().unwrap_or_default();
    let Some(commit) = baseline.main_commit.clone() else {
        return Ok(DeploymentStatus {
            commit: None,
            state: "never-published".to_string(),
            run_url: None,
            checked: false,
            notice: Some("这篇文章还没有发布到网站".to_string()),
        });
    };

    let run_url = format!(
        "https://github.com/{}/actions",
        config.repo_label
    );

    // 尝试只读查询工作流运行；失败时明确「待确认」。
    match fetch_workflow_conclusion(&config.repo_label) {
        Ok(Some(conclusion)) => {
            // 只有结论明确归属到**本次发布提交**时才落盘，避免把「别的文章
            // 那次运行」误当成这篇文章的部署结果。落盘后列表状态与这里一致。
            let attributed_to_publish = conclusion
                .head_sha()
                .map(|sha| sha == commit)
                .unwrap_or(true);
            if attributed_to_publish {
                let _ = engine.record_deploy_conclusion(&article_id, &conclusion, Some(&run_url));
            }
            let deploy_state = engine.deploy_state(&article_id, Some(conclusion))?;
            let label = match deploy_state {
                crate::publish::DeployState::Submitted => "publication-submitted",
                crate::publish::DeployState::Deploying => "deploying",
                crate::publish::DeployState::Live => "live-current-version",
                crate::publish::DeployState::Failed => "deploy-failed",
            };
            Ok(DeploymentStatus {
                commit: Some(commit),
                state: label.to_string(),
                run_url: Some(run_url),
                checked: true,
                notice: None,
            })
        }
        Ok(None) | Err(_) => Ok(DeploymentStatus {
            commit: Some(commit),
            state: "publication-submitted".to_string(),
            run_url: Some(run_url),
            checked: false,
            notice: Some("部署状态待确认：无法读取工作流结果，请打开运行记录查看".to_string()),
        }),
    }
}

/// 查询公开仓库最近一次 Pages 部署工作流结论（无凭据只读）。
fn fetch_workflow_conclusion(repo_label: &str) -> Result<Option<WorkflowConclusion>> {
    // 使用 GitHub 公开 REST API；未认证请求有速率限制，因此失败时必须降级而不是报错。
    let url = format!(
        "https://api.github.com/repos/{repo_label}/actions/workflows/deploy.yml/runs?per_page=1"
    );
    let output = std::process::Command::new("curl")
        .args(["-sS", "-m", "15", "-H", "Accept: application/vnd.github+json", &url])
        .output();

    let Ok(output) = output else {
        return Ok(None);
    };
    if !output.status.success() {
        return Ok(None);
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Ok(None);
    };
    Ok(WorkflowConclusion::from_workflow_run(&json))
}

/// 预览会话信息。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewSessionInfo {
    pub url: String,
    pub banner: String,
    pub temp_root: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offline_font_notice: Option<String>,
    pub simulate_public: bool,
}

/// 部署状态查询结果。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentStatus {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_url: Option<String>,
    /// 是否成功读取了工作流结果。
    pub checked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notice: Option<String>,
}

/// URL 改名影响评估。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenameAssessment {
    pub old_id: String,
    pub new_id: String,
    pub old_url: String,
    pub new_url: String,
    pub published: bool,
    /// 站内引用了旧 URL 的文章 ID 列表。
    pub referencing_articles: Vec<String>,
    /// 已发布文章改 URL 需重新发布。
    pub requires_republish: bool,
}

/// 文章状态汇总（供界面顶部显示）。
pub fn status_overview(state: &AppState) -> Result<Vec<ArticleStatus>> {
    Ok(list_articles(state)?.into_iter().map(|s| s.status).collect())
}

impl Default for AppState {
    fn default() -> Self {
        Self::with_store(LocalStore::open_default().expect("应用数据目录不可用"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{sample_markdown, sample_markdown_with_body, tiny_png, TestEnv};

    /// 构造一个指向隔离测试远端的命令层状态。
    fn state_for(env: &TestEnv) -> AppState {
        let mut config = env.store.load_config();
        config.workspace_dir = env.path().to_string_lossy().to_string();
        config.connected = true;
        config.repo_url = format!("file://{}", env.remote.to_string_lossy().replace('\\', "/"));
        config.repo_label = "fixture/site".to_string();
        config.disclosed_public_drafts = true;
        env.store.save_config(&config).unwrap();
        AppState::with_store(env.store.clone_handle())
    }

    fn meta(title: &str) -> ArticleMeta {
        ArticleMeta {
            title: title.to_string(),
            description: format!("{title} 摘要"),
            pub_date: "2026-09-23".to_string(),
            updated_date: None,
            tags: vec!["测试样稿".to_string()],
            draft: true,
        }
    }

    #[test]
    fn create_read_save_through_command_layer() {
        let env = TestEnv::new();
        let state = state_for(&env);

        let created = create_article(&state, "cmd-a".to_string(), meta("命令层"), "正文\n".to_string())
            .unwrap();
        assert!(created.meta.draft, "新建必须默认 draft: true");
        assert_eq!(created.body, "正文\n");

        let listed = list_articles(&state).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "cmd-a");

        let saved = save_article(
            &state,
            "cmd-a".to_string(),
            meta("命令层改"),
            "改过的正文\n".to_string(),
            None,
        )
        .unwrap();
        assert_eq!(saved.meta.title, "命令层改");
        assert_eq!(saved.body, "改过的正文\n");
    }

    #[test]
    fn create_rejects_invalid_and_duplicate_ids() {
        let env = TestEnv::new();
        let state = state_for(&env);
        assert!(create_article(&state, "Bad Id".to_string(), meta("x"), String::new()).is_err());
        create_article(&state, "dup".to_string(), meta("x"), String::new()).unwrap();
        let err = create_article(&state, "dup".to_string(), meta("y"), String::new()).unwrap_err();
        assert_eq!(err.code, ErrorCode::ArticleExists);
    }

    #[test]
    fn save_validates_meta_near_field() {
        let env = TestEnv::new();
        let state = state_for(&env);
        create_article(&state, "val-a".to_string(), meta("有效"), String::new()).unwrap();

        let mut bad = meta("无效");
        bad.description = "   ".to_string();
        let err = save_article(&state, "val-a".to_string(), bad, String::new(), None).unwrap_err();
        assert_eq!(err.detail.as_deref(), Some("description"));

        let mut bad_date = meta("无效日期");
        bad_date.pub_date = "2026-13-40".to_string();
        let err = save_article(&state, "val-a".to_string(), bad_date, String::new(), None).unwrap_err();
        assert_eq!(err.detail.as_deref(), Some("pubDate"));
    }

    #[test]
    fn updated_date_actions_are_distinguishable_over_json() {
        use crate::model::UpdatedDateAction;

        let env = TestEnv::new();
        let state = state_for(&env);
        create_article(&state, "date-a".to_string(), meta("日期"), "正文".to_string()).unwrap();

        // 省略 → 不改动该字段。
        save_article(&state, "date-a".to_string(), meta("日期"), "正文".to_string(), None).unwrap();
        assert!(read_article(&state, "date-a".to_string()).unwrap().meta.updated_date.is_none());

        // Set 与 Remove 在 JSON 里必须能区分（这是不用 Option<Option<_>> 的原因）。
        let set_json = serde_json::to_string(&Some(UpdatedDateAction::Set {
            value: "2026-03-04".to_string(),
        }))
        .unwrap();
        let none_json = serde_json::to_string(&Option::<UpdatedDateAction>::None).unwrap();
        let remove_json = serde_json::to_string(&Some(UpdatedDateAction::Remove)).unwrap();
        assert_ne!(set_json, remove_json);
        assert_ne!(none_json, remove_json);
        assert!(set_json.contains("\"set\""));
        assert!(remove_json.contains("\"remove\""));

        // 写入日期。
        let set_action: UpdatedDateAction =
            serde_json::from_str::<Option<UpdatedDateAction>>(&set_json).unwrap().unwrap();
        save_article(
            &state,
            "date-a".to_string(),
            meta("日期"),
            "正文".to_string(),
            Some(set_action),
        )
        .unwrap();
        assert_eq!(
            read_article(&state, "date-a".to_string()).unwrap().meta.updated_date.as_deref(),
            Some("2026-03-04")
        );

        // 之后再保存（省略）不得刷新它。
        save_article(&state, "date-a".to_string(), meta("日期"), "改过".to_string(), None).unwrap();
        assert_eq!(
            read_article(&state, "date-a".to_string()).unwrap().meta.updated_date.as_deref(),
            Some("2026-03-04"),
            "自动保存不应刷新 updatedDate"
        );

        // 显式删除。
        save_article(
            &state,
            "date-a".to_string(),
            meta("日期"),
            "改过".to_string(),
            Some(UpdatedDateAction::Remove),
        )
        .unwrap();
        assert!(read_article(&state, "date-a".to_string()).unwrap().meta.updated_date.is_none());
    }

    #[test]
    fn save_writes_recovery_then_clears_it() {
        let env = TestEnv::new();
        let state = state_for(&env);
        create_article(&state, "rec-a".to_string(), meta("恢复"), "原始".to_string()).unwrap();
        save_article(&state, "rec-a".to_string(), meta("恢复"), "更新".to_string(), None).unwrap();
        // 成功保存后不留恢复副本。
        assert!(pending_recovery(&state).unwrap().is_empty());
    }

    #[test]
    fn sync_then_publish_through_commands() {
        let env = TestEnv::new();
        let state = state_for(&env);
        create_article(&state, "flow-a".to_string(), meta("流程"), "正文 A\n".to_string()).unwrap();

        let assessment = assess_sync(&state, "flow-a".to_string()).unwrap();
        assert_eq!(assessment.decision, crate::sync::SyncDecision::Ready);

        let synced = sync_article(&state, "flow-a".to_string(), false).unwrap();
        assert!(synced.pushed_commit.is_some());
        assert!(synced.created_branch, "首次同步应创建写作分支");

        // 发布预检 + 发布。
        let precheck = publish_precheck(&state, "flow-a".to_string()).unwrap();
        assert_eq!(precheck.title, "流程");
        assert_eq!(precheck.target_branch, "main");

        let outcome = publish_article(&state, "flow-a".to_string(), Vec::new(), None).unwrap();
        assert!(env
            .remote_file_direct("main", "src/content/blog/flow-a.md")
            .unwrap()
            .contains("draft: false"));
        assert_eq!(outcome.changed_paths, vec!["src/content/blog/flow-a.md"]);
    }

    #[test]
    fn sync_queues_and_rejects_concurrent_remote_ops() {
        let env = TestEnv::new();
        let state = state_for(&env);
        create_article(&state, "queue-a".to_string(), meta("队列"), "正文".to_string()).unwrap();

        // 人为占用该文章的远端许可，模拟已有操作在进行。
        let held = state.queue.acquire(TaskKind::Sync, "queue-a").unwrap();
        let err = sync_article(&state, "queue-a".to_string(), false).unwrap_err();
        assert!(err.message.contains("正在进行"));
        held.finish();
        // 释放后可以正常同步。
        assert!(sync_article(&state, "queue-a".to_string(), false).is_ok());
    }

    #[test]
    fn withdraw_delete_and_restore_through_commands() {
        let env = TestEnv::new();
        let state = state_for(&env);
        create_article(&state, "wd-cmd".to_string(), meta("撤下删除"), "正文".to_string()).unwrap();
        sync_article(&state, "wd-cmd".to_string(), false).unwrap();
        publish_article(&state, "wd-cmd".to_string(), Vec::new(), None).unwrap();

        // 撤下。
        let withdrawn = withdraw_article(&state, "wd-cmd".to_string()).unwrap();
        assert!(withdrawn.main_done);
        assert!(env
            .remote_file_direct("main", "src/content/blog/wd-cmd.md")
            .unwrap()
            .contains("draft: true"));

        // 删除评估与删除。
        let assessment = assess_delete(&state, "wd-cmd".to_string()).unwrap();
        assert_eq!(assessment.title, "撤下删除");
        let deleted = delete_article(&state, "wd-cmd".to_string()).unwrap();
        assert!(deleted.writing_done && deleted.main_done);

        // 回收区与恢复。
        let trash = list_trash(&state).unwrap();
        assert_eq!(trash.len(), 1);
        let restored = restore_article(&state, deleted.op_id.clone()).unwrap();
        assert!(restored.meta.draft, "恢复后必须是未发布草稿");
        purge_trash(&state, deleted.op_id).unwrap();
        assert!(list_trash(&state).unwrap().is_empty());
    }

    #[test]
    fn rename_url_assessment_reports_impact() {
        let env = TestEnv::new();
        let state = state_for(&env);
        create_article(&state, "old-url".to_string(), meta("改名"), "正文".to_string()).unwrap();
        // 另一篇引用旧 URL。
        create_article(
            &state,
            "referrer".to_string(),
            meta("引用者"),
            "参考 /blog/old-url/ 这篇".to_string(),
        )
        .unwrap();

        let assessment = assess_rename_url(&state, "old-url".to_string(), "new-url".to_string()).unwrap();
        assert_eq!(assessment.old_url, "/blog/old-url/");
        assert_eq!(assessment.new_url, "/blog/new-url/");
        assert!(!assessment.published);
        assert!(assessment.referencing_articles.contains(&"referrer".to_string()));

        // 未发布文章改名不需要再发布确认。
        let renamed = rename_article_url(&state, "old-url".to_string(), "new-url".to_string(), false)
            .unwrap();
        assert_eq!(renamed.id, "new-url");
        assert!(env.path().join("src/content/blog/new-url.md").exists());
        assert!(!env.path().join("src/content/blog/old-url.md").exists());
    }

    #[test]
    fn rename_url_of_published_article_requires_confirmation() {
        let env = TestEnv::new();
        let state = state_for(&env);
        create_article(&state, "pub-url".to_string(), meta("已发布改名"), "正文".to_string()).unwrap();
        sync_article(&state, "pub-url".to_string(), false).unwrap();
        publish_article(&state, "pub-url".to_string(), Vec::new(), None).unwrap();

        let assessment = assess_rename_url(&state, "pub-url".to_string(), "pub-url-2".to_string()).unwrap();
        assert!(assessment.published);
        assert!(assessment.requires_republish);

        let err =
            rename_article_url(&state, "pub-url".to_string(), "pub-url-2".to_string(), false).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidArgument);
        // 未确认时不得移动文件。
        assert!(env.path().join("src/content/blog/pub-url.md").exists());

        // 确认后可以改名。
        rename_article_url(&state, "pub-url".to_string(), "pub-url-2".to_string(), true).unwrap();
        assert!(env.path().join("src/content/blog/pub-url-2.md").exists());
    }

    #[test]
    fn rename_refuses_occupied_target() {
        let env = TestEnv::new();
        let state = state_for(&env);
        create_article(&state, "id-a".to_string(), meta("A"), String::new()).unwrap();
        create_article(&state, "id-b".to_string(), meta("B"), String::new()).unwrap();
        let err = assess_rename_url(&state, "id-a".to_string(), "id-b".to_string()).unwrap_err();
        assert_eq!(err.code, ErrorCode::ArticleExists);
    }

    #[test]
    fn import_command_copies_single_file() {
        let env = TestEnv::new();
        let state = state_for(&env);
        let outside = tempfile::tempdir().unwrap();
        let src = outside.path().join("example-one.md");
        std::fs::write(&src, sample_markdown("导入示例", false)).unwrap();

        let imported = import_article(
            &state,
            src.to_string_lossy().to_string(),
            "example-one".to_string(),
        )
        .unwrap();
        assert_eq!(imported.meta.title, "导入示例");
        assert!(list_articles(&state).unwrap().iter().any(|a| a.id == "example-one"));

        // 相对路径被拒绝。
        let err = import_article(&state, "relative.md".to_string(), "x".to_string()).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidArgument);
    }

    #[test]
    fn preferences_are_clamped_and_persisted() {
        let env = TestEnv::new();
        let state = state_for(&env);
        let mut prefs = get_preferences(&state);
        prefs.font_size = 500;
        prefs.editor_mode = "unsupported".to_string();
        let saved = set_preferences(&state, prefs).unwrap();
        assert!(saved.font_size <= 28);
        assert_eq!(saved.editor_mode, "sv");
        assert_eq!(get_preferences(&state).font_size, saved.font_size);
    }

    #[test]
    fn status_overview_matches_article_count() {
        let env = TestEnv::new();
        let state = state_for(&env);
        create_article(&state, "ov-a".to_string(), meta("概览"), String::new()).unwrap();
        create_article(&state, "ov-b".to_string(), meta("概览2"), String::new()).unwrap();
        assert_eq!(status_overview(&state).unwrap().len(), 2);
    }

    #[test]
    fn deployment_status_is_honest_when_unverifiable() {
        let env = TestEnv::new();
        let state = state_for(&env);
        create_article(&state, "dep-a".to_string(), meta("部署"), "正文".to_string()).unwrap();
        sync_article(&state, "dep-a".to_string(), false).unwrap();
        publish_article(&state, "dep-a".to_string(), Vec::new(), None).unwrap();

        let status = deployment_status(&state, "dep-a".to_string()).unwrap();
        assert!(status.commit.is_some());
        assert!(status.run_url.is_some(), "必须给出可核对的运行记录链接");
        // 不能因为推送成功就宣称已上线。
        assert_ne!(status.state, "live-current-version");
        assert!(matches!(status.state.as_str(), "publication-submitted" | "deploying" | "deploy-failed"));

        // 从未发布的文章。
        create_article(&state, "never-a".to_string(), meta("未发布"), String::new()).unwrap();
        let never = deployment_status(&state, "never-a".to_string()).unwrap();
        assert_eq!(never.state, "never-published");
        assert!(never.commit.is_none());
    }

    /// P0-2 回归（列表状态）：列表状态必须与 `deployment_status` 口径一致。
    ///
    /// 旧缺陷：`remote_snapshots` 把仓库级 `main` 头填进每篇文章的
    /// `main_commit`，于是「从未发布的文章」在列表里显示成「已提交发布」，
    /// 「刚推送、部署未核实」显示成「网站已上线」。
    #[test]
    fn list_status_never_claims_live_without_deploy_confirmation() {
        let env = TestEnv::new();
        let state = state_for(&env);

        // 1) 从未发布：列表不得显示成已提交发布。
        create_article(&state, "ls-never".to_string(), meta("从未发布"), "正文".to_string()).unwrap();
        let listed = list_articles(&state).unwrap();
        let never = listed.iter().find(|a| a.id == "ls-never").unwrap();
        assert_eq!(
            never.status.site,
            crate::model::SiteState::NeverPublished,
            "从未发布的文章不得显示成已提交发布：{never:?}"
        );
        assert!(never.status.main_commit.is_none());

        // 2) 推送成功但部署未核实：不得显示成已上线。
        sync_article(&state, "ls-never".to_string(), false).unwrap();
        publish_article(&state, "ls-never".to_string(), Vec::new(), None).unwrap();
        let listed = list_articles(&state).unwrap();
        let pushed = listed.iter().find(|a| a.id == "ls-never").unwrap();
        assert_ne!(
            pushed.status.site,
            crate::model::SiteState::LiveCurrentVersion,
            "推送成功但部署未核实时不得显示已上线：{pushed:?}"
        );
        assert_eq!(pushed.status.site, crate::model::SiteState::PublicationSubmitted);
        // 与详情接口口径一致。
        let detail = deployment_status(&state, "ls-never".to_string()).unwrap();
        assert_ne!(detail.state, "live-current-version");
    }

    #[test]
    fn sync_offline_reports_actionable_error_and_keeps_local() {
        let env = TestEnv::new();
        let state = state_for(&env);
        create_article(&state, "off-a".to_string(), meta("离线"), "本地正文\n".to_string()).unwrap();

        // 让 origin 不可达。
        let bogus = env.dir.path().join("missing.git");
        env.git(&["remote", "set-url", "origin", &format!("file://{}", bogus.to_string_lossy().replace('\\', "/"))]);
        let mut config = env.store.load_config();
        config.repo_url = format!("file://{}", bogus.to_string_lossy().replace('\\', "/"));
        env.store.save_config(&config).unwrap();

        let result = assess_sync(&state, "off-a".to_string());
        assert!(result.is_err(), "远端不可达时评估应失败");
        // 本地内容仍然是可读的。
        let content = read_article(&state, "off-a".to_string()).unwrap();
        assert_eq!(content.body, "本地正文\n");
    }

    #[test]
    fn conflict_assessment_exposes_both_sides_via_commands() {
        let env = TestEnv::new();
        let state = state_for(&env);
        create_article(&state, "conf-a".to_string(), meta("冲突"), "本地初稿\n".to_string()).unwrap();
        sync_article(&state, "conf-a".to_string(), false).unwrap();

        // 他处设备改同一篇。
        let other = env.other_device("other-cmd");
        other.fetch("writing");
        other.checkout_new_branch("writing", "origin/writing");
        other.write_and_commit(
            "src/content/blog/conf-a.md",
            &sample_markdown_with_body("冲突", true, "远端改过"),
            "远端修改",
        );
        other.push("writing");

        // 本地也改。
        save_article(
            &state,
            "conf-a".to_string(),
            meta("冲突"),
            "本地改过\n".to_string(),
            None,
        )
        .unwrap();

        let assessment = assess_sync(&state, "conf-a".to_string()).unwrap();
        assert_eq!(assessment.decision, crate::sync::SyncDecision::RemoteChanged);
        assert!(assessment.needs_user_decision());
        assert!(assessment.writing_markdown.as_deref().unwrap().contains("远端改过"));

        // 未确认时同步被拒绝。
        assert!(sync_article(&state, "conf-a".to_string(), false).is_err());

        // 采用远端：本地内容被替换，且旧本地内容进入恢复副本。
        let adopted = adopt_remote(&state, "conf-a".to_string()).unwrap();
        assert!(adopted.body.contains("远端改过"));
        let recovery = pending_recovery(&state).unwrap();
        assert!(
            recovery.iter().any(|r| r.markdown.contains("本地改过")),
            "采用远端前必须保底保存本地修改"
        );
    }

    #[test]
    fn manual_merge_saves_and_is_syncable() {
        let env = TestEnv::new();
        let state = state_for(&env);
        create_article(&state, "merge-a".to_string(), meta("手工合并"), "初稿".to_string()).unwrap();
        sync_article(&state, "merge-a".to_string(), false).unwrap();

        let merged = resolve_conflict_manually(
            &state,
            "merge-a".to_string(),
            meta("手工合并"),
            "合并后的正文\n".to_string(),
        )
        .unwrap();
        assert_eq!(merged.body, "合并后的正文\n");
        // 合并结果可以继续同步。
        let outcome = sync_article(&state, "merge-a".to_string(), false).unwrap();
        assert!(outcome.pushed_commit.is_some());
    }

    #[test]
    fn discard_recovery_removes_draft() {
        let env = TestEnv::new();
        let state = state_for(&env);
        state
            .store
            .write_recovery(&RecoveryDraft {
                article_id: "r-a".to_string(),
                markdown_rel_path: "src/content/blog/r-a.md".to_string(),
                markdown: "---\ntitle: \"x\"\n---\n".to_string(),
                saved_at_unix: 1,
            })
            .unwrap();
        assert_eq!(pending_recovery(&state).unwrap().len(), 1);
        discard_recovery(&state, "r-a".to_string()).unwrap();
        assert!(pending_recovery(&state).unwrap().is_empty());
    }

    #[test]
    fn toolchain_report_is_available_through_commands() {
        let env = TestEnv::new();
        let state = state_for(&env);
        let report = toolchain_report(&state);
        // 结构完整；缺项与指引一一对应。
        assert_eq!(report.missing.len(), report.guidance.len());
    }

    #[test]
    fn connection_status_reflects_config() {
        let env = TestEnv::new();
        let state = state_for(&env);
        let status = connection_status(&state).unwrap();
        assert!(status.connected, "配置指向存在的仓库时应为已连接");
        assert!(status.workspace_ready);
        assert!(status.public_disclosure.contains("可见"));
    }

    #[test]
    fn missing_workspace_config_is_reported() {
        let env = TestEnv::new();
        let mut config = env.store.load_config();
        config.workspace_dir = String::new();
        config.connected = false;
        env.store.save_config(&config).unwrap();
        let state = AppState::with_store(env.store.clone_handle());

        let err = list_articles(&state).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidArgument);
        assert!(err.message.contains("首次连接"));
    }

    #[test]
    fn images_import_through_workspace_are_listed() {
        let env = TestEnv::new();
        let state = state_for(&env);
        create_article(&state, "img-cmd".to_string(), meta("带图"), String::new()).unwrap();
        images::import_image_bytes(env.path(), "img-cmd", "图", &tiny_png()).unwrap();
        let listed = list_articles(&state).unwrap();
        assert_eq!(listed[0].image_count, 1);
    }

    #[test]
    fn image_insert_validates_and_archives_to_article_dir() {
        let env = TestEnv::new();
        let state = state_for(&env);
        create_article(&state, "ins-a".to_string(), meta("插图"), String::new()).unwrap();

        // 源文件在文章目录之外，只复制它自己。
        let outside = tempfile::tempdir().unwrap();
        let source = outside.path().join("示意图 01.png");
        std::fs::write(&source, tiny_png()).unwrap();

        let inserted = import_article_image(&state, "ins-a".to_string(), source.to_string_lossy().to_string())
            .unwrap();
        assert!(inserted.rel_path.starts_with("public/blog/ins-a/"), "{}", inserted.rel_path);
        assert!(inserted.url.starts_with("/blog/ins-a/"), "{}", inserted.url);
        assert!(inserted.file_name.ends_with(".png"));
        assert_eq!(inserted.mime, "image/png");
        assert!(env.path().join(&inserted.rel_path).exists(), "图片应已归档");
        // 源文件未被改动或删除。
        assert!(source.exists());
        assert_eq!(std::fs::read(&source).unwrap(), tiny_png());

        // 列表能取回该图。
        let listed = list_article_images(&state, "ins-a".to_string()).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].url, inserted.url);
    }

    #[test]
    fn image_insert_rejects_unsupported_and_relative_paths() {
        let env = TestEnv::new();
        let state = state_for(&env);
        create_article(&state, "ins-b".to_string(), meta("插图校验"), String::new()).unwrap();

        // 伪装成 PNG 的文本被拒绝，且不落盘。
        let outside = tempfile::tempdir().unwrap();
        let fake = outside.path().join("fake.png");
        std::fs::write(&fake, b"<html>not an image</html>").unwrap();
        let err = import_article_image(&state, "ins-b".to_string(), fake.to_string_lossy().to_string())
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::ImageUnsupported);
        assert!(list_article_images(&state, "ins-b".to_string()).unwrap().is_empty());

        // 相对路径被拒绝。
        let err = import_article_image(&state, "ins-b".to_string(), "relative.png".to_string()).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidArgument);

        // 不存在的文件给出明确错误。
        let missing = outside.path().join("missing.png");
        let err = import_article_image(&state, "ins-b".to_string(), missing.to_string_lossy().to_string())
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::ArticleNotFound);
    }

    #[test]
    fn document_with_manual_image_reference_survives_round_trip() {
        let env = TestEnv::new();
        let state = state_for(&env);
        create_article(&state, "ins-c".to_string(), meta("含图正文"), "开头\n".to_string()).unwrap();

        let outside = tempfile::tempdir().unwrap();
        let source = outside.path().join("figure.png");
        std::fs::write(&source, tiny_png()).unwrap();
        let inserted = import_article_image(&state, "ins-c".to_string(), source.to_string_lossy().to_string())
            .unwrap();

        // 模拟前端把引用插入正文后保存。
        let body = format!("开头\n\n![figure]({})\n", inserted.url);
        let saved = save_article(
            &state,
            "ins-c".to_string(),
            meta("含图正文"),
            body.clone(),
            None,
        )
        .unwrap();
        assert_eq!(saved.body, body);
        assert!(saved.meta.title.contains("含图正文"));

        // 保存后的正文仍能在磁盘上逐字节核对，且 front matter 未被破坏。
        let on_disk =
            std::fs::read_to_string(env.path().join("src/content/blog/ins-c.md")).unwrap();
        assert!(on_disk.contains(&format!("![figure]({})", inserted.url)));
        assert!(on_disk.starts_with("---\n"));
        assert!(on_disk.contains("draft: true"));
    }

    #[test]
    fn image_bytes_import_goes_through_the_same_validation() {
        let env = TestEnv::new();
        let state = state_for(&env);
        create_article(&state, "paste-a".to_string(), meta("粘贴"), String::new()).unwrap();

        // 剪贴板/拖入来源：只有内存字节与原始文件名，没有磁盘路径。
        let inserted = import_article_image_data(&state, "paste-a".to_string(), "截图 01.png", &tiny_png())
            .unwrap();
        assert!(inserted.rel_path.starts_with("public/blog/paste-a/"), "{}", inserted.rel_path);
        assert!(inserted.url.starts_with("/blog/paste-a/"));
        assert_eq!(inserted.mime, "image/png");
        assert!(env.path().join(&inserted.rel_path).exists());

        // 与文件路径导入走同一套校验：伪装扩展名被拒绝。
        let err = import_article_image_data(&state, "paste-a".to_string(), "fake.png", b"<html>x</html>")
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::ImageUnsupported);

        // 超过大小上限同样被拒绝。
        let too_big = vec![0u8; (crate::util::MAX_IMAGE_BYTES + 1) as usize];
        let err = import_article_image_data(&state, "paste-a".to_string(), "big.png", &too_big)
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::ImageTooLarge);

        // 非法文章标识被拒绝。
        let err = import_article_image_data(&state, "../escape".to_string(), "x.png", &tiny_png())
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::ArticleIdInvalid);
    }

    #[test]
    fn pasted_image_content_is_archived_byte_for_byte() {
        let env = TestEnv::new();
        let state = state_for(&env);
        create_article(&state, "paste-b".to_string(), meta("粘贴字节"), String::new()).unwrap();

        let bytes = tiny_png();
        let inserted =
            import_article_image_data(&state, "paste-b".to_string(), "clip.png", &bytes).unwrap();

        let on_disk = std::fs::read(env.path().join(&inserted.rel_path)).unwrap();
        assert_eq!(on_disk, bytes, "归档内容必须与粘贴的字节完全一致");
        assert_eq!(inserted.content_hash, crate::util::hash_bytes(&bytes));

        // 相同内容重复粘贴会得到相同文件名（内容哈希去重），不产生第二份文件。
        let again =
            import_article_image_data(&state, "paste-b".to_string(), "clip.png", &bytes).unwrap();
        assert_eq!(again.rel_path, inserted.rel_path);
    }

    #[test]
    fn pasted_image_ref_is_insertable_and_syncable() {
        let env = TestEnv::new();
        let state = state_for(&env);
        create_article(&state, "paste-c".to_string(), meta("粘贴后同步"), "开头\n".to_string())
            .unwrap();

        let inserted =
            import_article_image_data(&state, "paste-c".to_string(), "图 2.png", &tiny_png()).unwrap();
        let body = format!("开头\n\n![图 2]({})\n", inserted.url);
        save_article(&state, "paste-c".to_string(), meta("粘贴后同步"), body.clone(), None)
            .unwrap();

        // 粘贴进来的图片随文章一起同步（图片先落盘才允许远端操作）。
        sync_article(&state, "paste-c".to_string(), false).unwrap();
        let synced = env
            .remote_file_direct("writing", "src/content/blog/paste-c.md")
            .expect("文章应已同步");
        assert!(synced.contains(&inserted.url));
        let images = env.remote_ls("writing", "public/blog/paste-c");
        assert_eq!(images.len(), 1, "粘贴的图片应随文章一起入库：{images:?}");
        // main 不受影响。
        assert!(env.remote_ls("main", "public/blog/paste-c").is_empty());
    }

    #[test]
    fn a1_recovery_snapshot_survives_simulated_crash_without_remote_commits() {
        // A1：编辑后异常关闭并重开 —— 本地正文与元数据可恢复；两个远端分支都没有新提交。
        let env = TestEnv::new();
        let state = state_for(&env);
        create_article(&state, "a1-a".to_string(), meta("A1 草稿"), "初始正文\n".to_string()).unwrap();

        // 记录「崩溃前」两个远端分支的头。
        let writing_before = env.remote_head("writing");
        let main_before = env.remote_head("main");

        // 模拟编辑过程中的恢复快照（内容尚未落盘）。
        let edited = meta("A1 改过的标题");
        snapshot_recovery(
            &state,
            "a1-a".to_string(),
            edited.clone(),
            "崩溃前未保存的正文\n".to_string(),
        )
        .unwrap();

        // 此时磁盘上仍是旧内容，恢复副本与磁盘不同，因此应被提示。
        let pending = pending_recovery(&state).unwrap();
        assert_eq!(pending.len(), 1, "应提示一份未保存的恢复副本");
        assert_eq!(pending[0].article_id, "a1-a");
        assert!(pending[0].markdown.contains("崩溃前未保存的正文"));

        // 磁盘内容确实还没变（模拟「来不及落盘就被强杀」）。
        let on_disk = std::fs::read_to_string(env.path().join("src/content/blog/a1-a.md")).unwrap();
        assert!(!on_disk.contains("崩溃前未保存的正文"), "崩溃前不应已落盘");

        // 模拟「强杀后重新打开软件」：重新构造 AppState（同一数据目录）。
        let reopened = AppState::with_store(env.store.clone_handle());
        let pending_after_restart = pending_recovery(&reopened).unwrap();
        assert_eq!(pending_after_restart.len(), 1, "重开后仍应提示恢复副本");
        assert_eq!(pending_after_restart[0].article_id, "a1-a");

        // 恢复：正文与元数据都回来了。
        let restored = restore_recovery(&reopened, "a1-a".to_string()).unwrap();
        assert_eq!(restored.meta.title, "A1 改过的标题", "元数据应恢复");
        assert!(restored.body.contains("崩溃前未保存的正文"), "正文应恢复");
        assert_eq!(restored.meta.description, edited.description);

        // 恢复后磁盘内容与恢复副本一致，提示消失。
        assert!(
            pending_recovery(&reopened).unwrap().is_empty(),
            "恢复后不应再提示未保存内容"
        );

        // 关键：两个远端分支都没有任何新提交（编辑与恢复都不触发远端 Git）。
        assert_eq!(env.remote_head("writing"), writing_before, "writing 不应有新提交");
        assert_eq!(env.remote_head("main"), main_before, "main 不应有新提交");
    }

    #[test]
    fn a1_discarding_recovery_keeps_disk_content_untouched() {
        // 丢弃恢复副本不应改动磁盘文章，也不产生远端提交。
        let env = TestEnv::new();
        let state = state_for(&env);
        create_article(&state, "a1-b".to_string(), meta("A1 保留"), "已保存正文\n".to_string())
            .unwrap();
        let before = std::fs::read_to_string(env.path().join("src/content/blog/a1-b.md")).unwrap();
        let writing_before = env.remote_head("writing");

        snapshot_recovery(
            &state,
            "a1-b".to_string(),
            meta("未保存的标题"),
            "未保存的正文\n".to_string(),
        )
        .unwrap();
        assert_eq!(pending_recovery(&state).unwrap().len(), 1);

        discard_recovery(&state, "a1-b".to_string()).unwrap();

        assert!(pending_recovery(&state).unwrap().is_empty(), "丢弃后不应再提示");
        let after = std::fs::read_to_string(env.path().join("src/content/blog/a1-b.md")).unwrap();
        assert_eq!(after, before, "丢弃副本不得改动磁盘文章");
        assert_eq!(env.remote_head("writing"), writing_before);
    }

    #[test]
    fn recovery_is_not_reported_once_content_is_saved() {
        // 内容已落盘时不应再提示「未保存内容」，避免误导。
        let env = TestEnv::new();
        let state = state_for(&env);
        create_article(&state, "a1-c".to_string(), meta("A1 一致"), "正文\n".to_string()).unwrap();

        // 快照内容与磁盘一致。
        let current = read_article(&state, "a1-c".to_string()).unwrap();
        snapshot_recovery(
            &state,
            "a1-c".to_string(),
            current.meta.clone(),
            current.body.clone(),
        )
        .unwrap();

        assert!(
            pending_recovery(&state).unwrap().is_empty(),
            "内容与磁盘一致时不应提示未保存内容"
        );
    }

    #[test]
    fn saving_clears_the_pending_recovery_copy() {
        // 正常保存成功后，恢复副本应被清除（由 save_article 内部完成）。
        let env = TestEnv::new();
        let state = state_for(&env);
        create_article(&state, "a1-d".to_string(), meta("A1 保存"), "初始\n".to_string()).unwrap();

        snapshot_recovery(
            &state,
            "a1-d".to_string(),
            meta("未保存"),
            "未保存的正文\n".to_string(),
        )
        .unwrap();
        assert_eq!(pending_recovery(&state).unwrap().len(), 1);

        save_article(
            &state,
            "a1-d".to_string(),
            meta("已保存"),
            "已保存的正文\n".to_string(),
            None,
        )
        .unwrap();

        assert!(
            pending_recovery(&state).unwrap().is_empty(),
            "保存成功后不应残留恢复副本"
        );
    }

    #[test]
    fn restore_recovery_refuses_unparsable_content_and_keeps_disk() {
        // 恢复副本若无法解析，必须拒绝覆盖磁盘，且保留原文件。
        let env = TestEnv::new();
        let state = state_for(&env);
        create_article(&state, "a1-e".to_string(), meta("A1 坏副本"), "完好正文\n".to_string())
            .unwrap();
        let before = std::fs::read_to_string(env.path().join("src/content/blog/a1-e.md")).unwrap();

        // 直接写入一份损坏的恢复副本（模拟极端情况）。
        state
            .store
            .write_recovery(&RecoveryDraft {
                article_id: "a1-e".to_string(),
                markdown_rel_path: "src/content/blog/a1-e.md".to_string(),
                markdown: "没有 front matter 的内容".to_string(),
                saved_at_unix: 1,
            })
            .unwrap();

        let err = restore_recovery(&state, "a1-e".to_string()).unwrap_err();
        assert_eq!(err.code, ErrorCode::FrontMatterMissing);
        let after = std::fs::read_to_string(env.path().join("src/content/blog/a1-e.md")).unwrap();
        assert_eq!(after, before, "解析失败时不得覆盖磁盘内容");
    }

    #[test]
    fn image_ref_counts_are_used_by_delete_protection() {
        // 图片被两篇文章引用时，删除其中一篇不应清理该图（配合 trash 测试）。
        let env = TestEnv::new();
        let state = state_for(&env);
        create_article(&state, "owner-x".to_string(), meta("拥有者"), String::new()).unwrap();
        create_article(&state, "ref-x".to_string(), meta("引用者"), String::new()).unwrap();

        let outside = tempfile::tempdir().unwrap();
        let source = outside.path().join("shared.png");
        std::fs::write(&source, tiny_png()).unwrap();
        let inserted =
            import_article_image(&state, "owner-x".to_string(), source.to_string_lossy().to_string())
                .unwrap();

        // 两篇都引用该图。
        for (id, title) in [("owner-x", "拥有者"), ("ref-x", "引用者")] {
            save_article(
                &state,
                id.to_string(),
                meta(title),
                format!("![shared]({})\n", inserted.url),
                None,
            )
            .unwrap();
        }

        let assessment = assess_delete(&state, "owner-x".to_string()).unwrap();
        assert!(
            assessment.protected_images.contains(&inserted.rel_path),
            "被另一篇引用的图片必须列入保护：{assessment:?}"
        );
        assert!(assessment.exclusive_images.is_empty());
    }
}
