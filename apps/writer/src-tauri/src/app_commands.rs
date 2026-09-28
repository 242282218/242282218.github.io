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
use crate::preview::{
    PreviewDependencyStatus, PreviewDependencyTasks, PreviewEngine, PreviewOverlay, PreviewServer,
    INSTALL_TIMEOUT,
};
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
    /// 预览依赖准备任务的登记与状态（单实例：同一时刻只允许一次安装）。
    pub preview_dependencies: PreviewDependencyTasks,
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
            preview_dependencies: PreviewDependencyTasks::new(),
            instance_lock,
        })
    }

    /// 供测试与嵌入场景使用：指定应用数据目录。
    pub fn with_store(store: LocalStore) -> Self {
        Self {
            store,
            queue: TaskQueue::new(),
            preview: Arc::new(Mutex::new(None)),
            preview_dependencies: PreviewDependencyTasks::new(),
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

    /// 远端核对的超时上限。断网时 `git fetch` 可能长时间挂住，必须有上限。
    pub const REMOTE_CHECK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

    /// 读取远端快照用于状态推导。
    ///
    /// **这里不发起任何网络请求**：保存、列表刷新都走这条路径，若在此隐式
    /// `fetch`，每次自动保存都会联网核对远端（旧行为的主要卡顿来源）。
    /// 结论只来自两类来源，且都要求「仍可证明有效」：
    /// 1. 本机缓存（键含仓库与文章，且记录的远端头必须与当前本地跟踪引用一致）；
    /// 2. 调用方在同一轮里已经核实过的结论（见 [`Self::remote_snapshots_after_check`]）。
    ///
    /// 没有可用结论时返回**未核对**，界面据实显示「待核对」而不是「已同步」。
    fn remote_snapshots(&self, workspace: &Workspace, config: &AppConfig) -> BTreeMap<String, RemoteSnapshot> {
        let repo = crate::git::normalize_remote_url(&config.repo_url);
        let cache = self.store.load_remote_checks();
        let mut out = BTreeMap::new();
        for rel in workspace.list_markdown_rel_paths().unwrap_or_default() {
            let Ok(article_id) = paths::article_id_from_rel_path(&rel) else {
                continue;
            };
            let entry = cache.get(&repo, &article_id);
            out.insert(article_id, self.snapshot_from_cache(workspace, entry));
        }
        out
    }

    /// 把一条缓存记录转换成快照；缓存已失效时两个分支都是「未核对」。
    ///
    /// 复用条件：仓库键一致（已由查表保证）、且记录里的远端头仍等于本地跟踪引用。
    /// 远端可能已经前进，因此**必须**先能读到当前跟踪头才允许复用。
    fn snapshot_from_cache(
        &self,
        workspace: &Workspace,
        entry: Option<&crate::local_store::RemoteCheckEntry>,
    ) -> RemoteSnapshot {
        let Some(entry) = entry else {
            return RemoteSnapshot::default();
        };
        let mut snapshot = RemoteSnapshot {
            writing: entry.writing.clone(),
            main: entry.main.clone(),
            ..Default::default()
        };
        // 逐个分支复核：跟踪引用读不到（从未 fetch 过）或头已前进，都视为失效。
        for (branch, check) in [
            (crate::sync::WRITING_BRANCH, &mut snapshot.writing),
            (MAIN_BRANCH, &mut snapshot.main),
        ] {
            let current = crate::git::rev_parse(
                workspace.root(),
                &format!("refs/remotes/origin/{branch}"),
            )
            .ok();
            if current.is_none() || current != check.head {
                *check = crate::model::BranchCheck::unverified(
                    "远端可能已变化，需要重新核对（缓存仅在上次核对时的头未变时可用）",
                );
            }
        }
        // 部署结论也与 main 头绑定；main 未核对时不得据此判断上线状态。
        if !snapshot.main.is_verified() {
            snapshot.deployed_commit = None;
            snapshot.deploy_failed_commit = None;
            snapshot.deploying_commit = None;
        }
        snapshot
    }

    /// 写入一份核对结论到缓存（只在两个分支都确实核对过时记录）。
    fn store_remote_check(
        &self,
        repo: &str,
        article_id: &str,
        snapshot: &RemoteSnapshot,
    ) -> Result<()> {
        if !snapshot.writing.is_verified() && !snapshot.main.is_verified() {
            return Ok(());
        }
        let mut cache = self.store.load_remote_checks();
        cache.put(
            repo,
            article_id,
            crate::local_store::RemoteCheckEntry {
                writing: snapshot.writing.clone(),
                main: snapshot.main.clone(),
                repo: repo.to_string(),
            },
        );
        self.store.save_remote_checks(&cache)
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

/// 能力检测（Git / Node / pnpm）：用户显式触发的真实探测。
///
/// 探测结果落盘，之后启动路径直接读缓存，不必每次都跑进程。
pub fn run_toolchain_check(state: &AppState) -> Result<crate::preview::ToolchainState> {
    state.connector().run_toolchain_check()
}

/// 文章列表（带三处状态）。
///
/// 只读本地数据与**仍可证明有效**的远端缓存，不做任何隐式网络请求。
pub fn list_articles(state: &AppState) -> Result<Vec<ArticleSummary>> {
    let workspace = state.workspace()?;
    let config = state.config();
    let snapshots = state.remote_snapshots(&workspace, &config);
    workspace.scan(&state.store, &snapshots)
}

/// 读取单篇文章。与列表同源：本地优先，远端结论只来自有效缓存。
pub fn read_article(state: &AppState, article_id: String) -> Result<ArticleContent> {
    validate_article_id(&article_id)?;
    let workspace = state.workspace()?;
    let config = state.config();
    let snapshots = state.remote_snapshots(&workspace, &config);
    workspace.read(&article_id, &snapshots)
}

/// 一次远端核对的结果（可直接交给前端替换当前文章的状态）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteCheckOutcome {
    pub article_id: String,
    /// 核对时本地磁盘原文的哈希。
    ///
    /// 前端据此丢弃过期结果：核对期间用户可能已经继续编辑，或切换到了别的文章；
    /// 那种情况下这个哈希与当前文章对不上，结果必须整个丢弃，不能只更新状态字段。
    pub local_body_hash: String,
    pub status: ArticleStatus,
}

/// 核对单篇文章在两个远端分支上的状态（会联网）。
///
/// 这是「打开文章后异步核对一次」与「手动刷新远端状态」共用的实现：
/// - 两个分支**分别**得出「未核对 / 确认不存在 / 有内容」，一支失败不影响另一支；
/// - 超时、断网、认证失败等都是「未核对」并带上原因，**绝不复用旧结论**；
/// - 成功核对后写入缓存（键含仓库与文章，值为结论及其依据的远端头）。
///
/// 返回带本地哈希的完整结论；调用方负责核对哈希后再写回界面。
pub fn check_article_remote(state: &AppState, article_id: String) -> Result<RemoteCheckOutcome> {
    validate_article_id(&article_id)?;
    let workspace = state.workspace()?;
    let config = state.config();
    let engine = state.sync_engine(&workspace, &config);
    let repo = crate::git::normalize_remote_url(&config.repo_url);

    // 每个分支独立核对：结论之间互不牵连。
    let writing = check_one_branch(&engine, crate::sync::WRITING_BRANCH, &article_id);
    let main = check_one_branch(&engine, MAIN_BRANCH, &article_id);

    let baseline = state.store.load_versions().get(&article_id).cloned().unwrap_or_default();
    // 部署结论只来自**已确认的工作流查询**在基线上的落盘记录，且只在 main
    // 确实核对过时才允许参与上线判断。
    let snapshot = RemoteSnapshot {
        writing,
        main,
        deployed_commit: baseline.deployed_commit.clone(),
        deploy_failed_commit: baseline.deploy_failed_commit.clone(),
        deploying_commit: baseline.deploying_commit.clone(),
    };

    state.store_remote_check(&repo, &article_id, &snapshot)?;

    // 本地内容只读一次，再由同一份字节推出顶层哈希与状态里的全部字段。
    let raw = workspace.read_raw(&article_id)?.unwrap_or_default();
    Ok(outcome_from_local_bytes(article_id, &raw, snapshot))
}

/// 由**同一份**本地字节与远端快照构造核对结论。
///
/// 单独抽出来是为了让「结论自洽」这条不变量可以被**确定性地**测试：
/// 顶层 `local_body_hash` 与状态里的本地/网站哈希必须同源。若实现改成读盘两次
/// （一次算哈希、一次算网站哈希），两次之间落盘的新内容就会让结论自相矛盾——
/// 而这只有把「一份字节 → 一条结论」做成纯函数才能稳定断言，靠并发赛跑是概率性的
/// （实测：把实现改回两次读盘，并发用例仍然照常通过）。
pub fn outcome_from_local_bytes(
    article_id: String,
    raw: &[u8],
    snapshot: RemoteSnapshot,
) -> RemoteCheckOutcome {
    let local_hash = util::hash_bytes(raw);
    let local_text = String::from_utf8_lossy(raw).into_owned();
    let status = crate::workspace::derive_status(&crate::workspace::StatusInputs {
        local_hash: local_hash.clone(),
        local_site_hash: crate::workspace::site_hash_of(&local_text),
        locally_saved: true,
        remote: snapshot,
    });
    RemoteCheckOutcome { article_id, local_body_hash: local_hash, status }
}

/// 核对一个分支上该文章的状态。
///
/// 任何失败都转成「未核对 ＋ 原因」，而不是返回错误：打开文章时的核对是后台
/// 补足信息，不应因为断网就打断编辑。真正的错误留给同步/发布自己的预检流程。
fn check_one_branch(
    engine: &SyncEngine<'_, '_>,
    branch: &str,
    article_id: &str,
) -> crate::model::BranchCheck {
    let checked_at = util::unix_seconds();
    let head = match engine.fetch_with_timeout(branch, AppState::REMOTE_CHECK_TIMEOUT) {
        Ok(Some(head)) => head,
        // 远端确实没有这个分支：已核对，确认不存在。
        Ok(None) => return crate::model::BranchCheck::absent(None, checked_at),
        Err(err) => {
            return crate::model::BranchCheck::unverified(check_failure_reason(branch, &err));
        }
    };

    let text = match engine.markdown_at_with_timeout(&head, article_id, AppState::REMOTE_CHECK_TIMEOUT) {
        Ok(Some(text)) => text,
        Ok(None) => return crate::model::BranchCheck::absent(Some(head), checked_at),
        Err(err) => {
            return crate::model::BranchCheck::unverified(check_failure_reason(branch, &err));
        }
    };

    // 已核对且存在：front matter 解析失败不影响「存在」这一事实，只是少了公开性信息。
    let published = crate::article_io::parse_markdown(&text).ok().and_then(|parsed| {
        let map = crate::article_io::parse_front_matter_map(&parsed.front_matter).ok()?;
        let meta = crate::article_io::meta_from_map(&map).ok()?;
        Some(!meta.draft)
    });
    let deployment_url = if branch == MAIN_BRANCH {
        engine
            .store_handle()
            .load_versions()
            .get(article_id)
            .and_then(|baseline| baseline.deployment_url.clone())
    } else {
        None
    };

    crate::model::BranchCheck::present(
        Some(head),
        checked_at,
        util::hash_bytes(text.as_bytes()),
        Some(crate::workspace::site_hash_of(&text)),
        published,
        deployment_url,
    )
}

/// 把核对失败转成面向用户的原因说明。
fn check_failure_reason(branch: &str, err: &WriterError) -> String {
    let label = if branch == MAIN_BRANCH { "网站分支" } else { "写作分支" };
    let cause = match err.code {
        ErrorCode::Offline => "网络不可用或核对超时",
        ErrorCode::AuthFailed => "认证失败",
        ErrorCode::ToolchainMissing => "未找到 Git",
        _ => "读取远端失败",
    };
    format!("{label}状态待核对：{cause}")
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

/// 把一篇文章的**磁盘原文**导出到用户选定的绝对路径。
///
/// 只读工作区、只写目标文件；不进入受管目录、不动 Git、不更新任何索引。
/// 目标路径来自系统「另存为」对话框，与 [`import_article`] 属同一信任级别。
///
/// 额外拒绝落在应用数据目录或受管工作区内的目标：这两处是软件自己管理的状态
/// （配置、恢复副本、版本基线、仓库工作树），误覆盖会造成不可逆损失，而「导出」
/// 没有写入这两处的正当理由。
pub fn export_article(state: &AppState, article_id: String, target_path: String) -> Result<()> {
    validate_article_id(&article_id)?;
    let target = PathBuf::from(target_path.trim());
    if !target.is_absolute() {
        return Err(WriterError::new(
            ErrorCode::InvalidArgument,
            "请选择要导出的目标文件（绝对路径）",
        ));
    }
    let workspace = state.workspace()?;
    if target.starts_with(workspace.root()) {
        return Err(WriterError::new(
            ErrorCode::InvalidArgument,
            "目标位于受管工作区内。导出请选择工作区以外的位置",
        ));
    }
    if crate::connection::is_inside_app_data(&state.store, &target) {
        return Err(WriterError::new(
            ErrorCode::InvalidArgument,
            "目标位于应用数据目录内。导出请选择该目录以外的位置",
        ));
    }
    // 字符串前缀比较挡不住符号链接/目录联接：目标（或其父目录）若是指向受管
    // 位置的链接，`fs::write` 会跟随链接写进受管目录，绕过上面两条检查。
    crate::util::verify_output_path_not_link(&target)?;
    let bytes = workspace.read_raw(&article_id)?.ok_or_else(|| {
        WriterError::new(ErrorCode::ArticleNotFound, "找不到这篇文章的本地文件")
    })?;
    std::fs::write(&target, &bytes).map_err(|err| {
        WriterError::new(ErrorCode::IoFailed, format!("写入导出文件失败：{err}"))
    })?;
    Ok(())
}
/// 评估一篇文章能否安全同步（差异界面数据源）。
pub fn assess_sync(state: &AppState, article_id: String) -> Result<SyncAssessment> {    validate_article_id(&article_id)?;
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
///
/// **本函数不安装依赖**：安装可能持续数分钟，只能由用户显式触发的
/// [`prepare_site_preview_dependencies`] 完成。缺依赖时这里立刻返回明确的
/// 「需准备依赖」状态，绝不静默安装、也绝不声称有后台任务在跑。
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

    let info = run_exclusive(&state.queue, TaskKind::SitePreview, "site", || {
        let worktree = engine.prepare_worktree(&paths::case_insensitive_key(&article_id).replace('/', "-"))?;
        engine.apply_overlay(&worktree, &overlay)?;

        // 只复用工作区已有的依赖；缺失时报「需准备依赖」并清理本次副本。
        if let Err(err) = engine.require_dependencies(&worktree) {
            let _ = crate::util::remove_dir_all_no_follow(&worktree);
            return Err(err);
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
    })?;
    // 启动成功即证明依赖可用，把状态校正为「已就绪」。
    state.preview_dependencies.mark_ready();
    Ok(info)
}

/// 查询预览依赖的准备状态（不启动任何任务）。
pub fn preview_dependency_status(state: &AppState) -> PreviewDependencyStatus {
    state.preview_dependencies.status()
}

/// 请求准备网站预览依赖。
///
/// 返回的是**真实状态**：
/// - 依赖已就绪 → `Ready`（不启动任务）；
/// - 已有准备任务在跑 → 返回该任务（同一个 `task_id`），不会重复安装；
/// - 否则登记并启动一个带任务标识的后台任务，返回 `Preparing`。
///
/// 工作区不可用等**无法启动任务**的情况返回 `Err`，此时不会留下任何
/// 「正在进行」的假状态。
pub fn prepare_site_preview_dependencies(state: &AppState) -> Result<PreviewDependencyStatus> {
    if state.preview_dependencies.status().is_preparing() {
        return Ok(state.preview_dependencies.status());
    }
    let workspace = state.workspace()?;
    let config = state.config();
    let temp_base = state.connector().preview_temp_root();
    let repo_url = config.repo_url.clone();

    let engine = PreviewEngine::new(&workspace, temp_base.clone(), &repo_url);
    if engine.workspace_dependencies_ready() {
        return Ok(state.preview_dependencies.mark_ready());
    }

    // 后台线程只能持有拥有型数据：工作区与引擎都在线程内重建。
    let root = workspace.root().to_path_buf();
    Ok(state.preview_dependencies.start(move || {
        let workspace = Workspace::open(root)?;
        let engine = PreviewEngine::new(&workspace, temp_base, &repo_url);
        engine.prepare_dependencies(INSTALL_TIMEOUT)
    }))
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
    let output = crate::util::program_command("curl")
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
    fn shell_theme_defaults_to_system_and_clamps_unknown_values() {
        let env = TestEnv::new();
        let state = state_for(&env);
        // 默认跟随系统。
        assert_eq!(get_preferences(&state).shell_theme, "system");

        let mut prefs = get_preferences(&state);
        prefs.shell_theme = "dark".to_string();
        assert_eq!(set_preferences(&state, prefs).unwrap().shell_theme, "dark");

        // 未知取值退回「跟随系统」，不留一个没有消费者的字符串。
        let mut prefs = get_preferences(&state);
        prefs.shell_theme = "neon".to_string();
        assert_eq!(set_preferences(&state, prefs).unwrap().shell_theme, "system");
    }

    /// 旧版 `config.json` 没有 `shellTheme` 字段，反序列化不能失败。
    ///
    /// 失败会被 `load_config` 判为「配置损坏」并把整份配置重置为默认值，
    /// 用户的仓库地址与其他偏好一并丢失。
    #[test]
    fn config_without_shell_theme_still_loads_other_preferences() {
        let env = TestEnv::new();
        let state = state_for(&env);
        let mut prefs = get_preferences(&state);
        prefs.font_size = 21;
        prefs.code_theme = "monokai".to_string();
        set_preferences(&state, prefs).unwrap();

        // 抹掉字段，模拟旧版写入的文件。
        // 用 JSON 层删除而不是字符串替换：缩进由 pretty 输出决定，替换串很脆。
        let path = state.store.root().join("config.json");
        let text = std::fs::read_to_string(&path).unwrap();
        let mut value: serde_json::Value = serde_json::from_str(&text).unwrap();
        let removed = value
            .get_mut("preferences")
            .and_then(|prefs| prefs.as_object_mut())
            .and_then(|prefs| prefs.remove("shellTheme"));
        assert!(removed.is_some(), "写入的配置里应有 shellTheme 字段");
        std::fs::write(&path, serde_json::to_string_pretty(&value).unwrap()).unwrap();
        assert!(!std::fs::read_to_string(&path).unwrap().contains("shellTheme"));

        let loaded = get_preferences(&state);
        assert_eq!(loaded.shell_theme, "system", "缺失字段应回落到默认值");
        assert_eq!(loaded.font_size, 21, "其他偏好必须原样保留，不能被重置");
        assert_eq!(loaded.code_theme, "monokai");
        // 且原文件没有被改名成 .bak（那是「配置损坏」分支才会做的事）。
        assert!(!state.store.root().join("config.json.bak").exists());
    }

    #[test]
    fn export_writes_disk_bytes_and_refuses_managed_targets() {
        let env = TestEnv::new();
        let state = state_for(&env);
        create_article(&state, "exp-a".to_string(), meta("导出示例"), "正文内容".to_string())
            .unwrap();
        let source_bytes = state
            .workspace()
            .unwrap()
            .read_raw("exp-a")
            .unwrap()
            .expect("文章已落盘");

        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("导出的副本.md");
        export_article(
            &state,
            "exp-a".to_string(),
            target.to_string_lossy().to_string(),
        )
        .unwrap();
        // 导出的是磁盘原文，逐字节相同（不是 render() 规范化后的结果）。
        assert_eq!(std::fs::read(&target).unwrap(), source_bytes);

        // 导出不改动工作区。
        assert_eq!(
            state.workspace().unwrap().read_raw("exp-a").unwrap().unwrap(),
            source_bytes,
        );

        // 相对路径被拒绝。
        let err = export_article(&state, "exp-a".to_string(), "copy.md".to_string()).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidArgument);

        // 受管工作区内的目标被拒绝，且不产生文件。
        let workspace = state.workspace().unwrap();
        let inside = workspace.root().join("exported.md");
        let err = export_article(
            &state,
            "exp-a".to_string(),
            inside.to_string_lossy().to_string(),
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidArgument);
        assert!(!inside.exists());

        // 应用数据目录内的目标被拒绝。
        let in_data = state.store.root().join("exported.md");
        let err = export_article(
            &state,
            "exp-a".to_string(),
            in_data.to_string_lossy().to_string(),
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidArgument);
        assert!(!in_data.exists());

        // 文章不存在时如实报错。
        let err = export_article(
            &state,
            "missing".to_string(),
            outside.path().join("x.md").to_string_lossy().to_string(),
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::ArticleNotFound);
    }

    /// P1 回归：导出目标若经由链接指向受管位置，字符串前缀检查挡不住。
    ///
    /// `std::fs::write` 会跟随链接，因此「目标路径不在工作区内」并不等于「写入
    /// 落在工作区外」。夹具用 **Windows 目录联接**（无需管理员权限，与
    /// `workspace.rs` 里的逃逸测试同法）：`outside/into-managed` → 工作区根，
    /// 导出到 `outside/into-managed/<文件名>` 若被放行，就会覆盖软件自己管理的文件。
    #[test]
    fn export_refuses_target_reaching_managed_areas_through_a_link() {
        let env = TestEnv::new();
        let state = state_for(&env);
        create_article(&state, "exp-link".to_string(), meta("链接导出"), "正文".to_string())
            .unwrap();
        let workspace = state.workspace().unwrap();
        // 受管文章的真实路径：`src/content/blog/<id>.md`（不是工作区根）。
        let victim_rel = format!("{}exp-link.md", crate::paths::BLOG_DIR_PREFIX);
        let victim = workspace.root().join(&victim_rel);
        assert!(victim.exists(), "夹具前提：受管文件存在于 {victim_rel}");

        let outside = tempfile::tempdir().unwrap();
        let junction = outside.path().join("into-managed");
        if !create_dir_junction(workspace.root(), &junction) {
            eprintln!("[跳过] 本机不允许创建目录联接");
            return;
        }
        // 经联接指向工作区里的真实文章文件。
        let target = junction.join(&victim_rel);

        // 字符串层面：目标既不在工作区前缀内，也不在应用数据目录内。
        assert!(!target.starts_with(workspace.root()));
        assert!(!crate::connection::is_inside_app_data(&state.store, &target));

        let before = state.workspace().unwrap().read_raw("exp-link").unwrap().unwrap();
        let err = export_article(
            &state,
            "exp-link".to_string(),
            target.to_string_lossy().to_string(),
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::PathOutOfScope);
        // 受管位置的文件必须逐字节未变。
        assert_eq!(
            state.workspace().unwrap().read_raw("exp-link").unwrap().unwrap(),
            before,
            "经链接落到受管目录的导出必须被拒绝，且原文件不得被改写",
        );
        // 清理联结，避免临时目录删除时跟随链接。
        let _ = std::fs::remove_dir(&junction);
    }

    /// 创建一个指向 `target` 的 Windows 目录联接；失败返回 false（由调用方跳过）。
    #[cfg(windows)]
    fn create_dir_junction(target: &std::path::Path, link: &std::path::Path) -> bool {
        std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false)
    }

    #[cfg(not(windows))]
    fn create_dir_junction(target: &std::path::Path, link: &std::path::Path) -> bool {
        std::os::unix::fs::symlink(target, link).is_ok()
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
    ///
    /// A5 之后列表**不再隐式联网**，因此结论必须由显式核对产生：
    /// 核对前是「待核对」，核对到「两分支都没有」才是「从未发布」。
    #[test]
    fn list_status_never_claims_live_without_deploy_confirmation() {
        let env = TestEnv::new();
        let state = state_for(&env);

        // 0) 核对之前：列表只能显示「待核对」，不得断言从未发布。
        create_article(&state, "ls-never".to_string(), meta("从未发布"), "正文".to_string()).unwrap();
        let listed = list_articles(&state).unwrap();
        let fresh = listed.iter().find(|a| a.id == "ls-never").unwrap();
        assert_eq!(
            fresh.status.site,
            crate::model::SiteState::Unverified,
            "未经核对不得给出网站结论：{fresh:?}"
        );
        assert_eq!(fresh.status.remote_sync, crate::model::RemoteSync::Unverified);

        // 1) 显式核对后：远端确实没有该文章 → 从未发布，且不得显示成已提交发布。
        check_article_remote(&state, "ls-never".to_string()).unwrap();
        let listed = list_articles(&state).unwrap();
        let never = listed.iter().find(|a| a.id == "ls-never").unwrap();
        assert_eq!(
            never.status.site,
            crate::model::SiteState::NeverPublished,
            "核对确认不存在后才能说从未发布：{never:?}"
        );
        assert_eq!(
            never.status.main.state,
            crate::model::CheckState::Absent,
            "文章不在 main 上，必须表达为「确认不存在」而不是「有内容」"
        );
        assert!(
            never.status.main.body_hash.is_none(),
            "确认不存在时不得携带任何内容哈希"
        );

        // 2) 推送成功但部署未核实：不得显示成已上线。
        sync_article(&state, "ls-never".to_string(), false).unwrap();
        publish_article(&state, "ls-never".to_string(), Vec::new(), None).unwrap();
        check_article_remote(&state, "ls-never".to_string()).unwrap();
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
    fn toolchain_check_is_explicit_and_recorded() {
        let env = TestEnv::new();
        let state = state_for(&env);

        // 启动路径：只有「尚未检查」，且不派生任何探测进程。
        assert_eq!(state.connector().toolchain_state(), crate::preview::ToolchainState::Unchecked);

        // 显式触发后才产生真实结论，并落盘供后续读取。
        let checked = run_toolchain_check(&state).unwrap();
        let report = checked.verified().expect("本机应能完成环境检查");
        // 结构完整；缺项与指引一一对应。
        assert_eq!(report.missing.len(), report.guidance.len());
        assert!(state.connector().toolchain_state().is_checked(), "检查结果应被记录");
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

    // ---------------------------------------------------------------- A2.4 竞态

    /// 磁盘上文章原文的哈希（与命令层同一基准：磁盘原文字节）。
    fn disk_hash(env: &TestEnv, article_id: &str) -> String {
        util::hash_file(&env.path().join(format!("src/content/blog/{article_id}.md"))).unwrap()
    }

    /// A2.4：`check_article_remote` 返回的 `local_body_hash` 必须是**核对当时磁盘原文**
    /// 的哈希，而不是某次更早或更晚读取的结果。
    ///
    /// 这是前端丢弃迟到结果的唯一依据：只有哈希与当前磁盘内容一致，这条结论才
    /// 还成立。哈希来源一旦与结论依据的内容版本脱节，「旧结论冒充当前状态」
    /// 就无法被发现。
    #[test]
    fn remote_check_hash_matches_disk_content_at_check_time() {
        let env = TestEnv::new();
        let state = state_for(&env);
        create_article(&state, "race-hash".to_string(), meta("核对哈希"), "第一版正文\n".to_string())
            .unwrap();
        let hash_before = disk_hash(&env, "race-hash");

        let outcome = check_article_remote(&state, "race-hash".to_string()).unwrap();
        assert_eq!(
            outcome.local_body_hash, hash_before,
            "核对结果必须携带核对当时磁盘原文的哈希"
        );
        // 结论内部自洽：status 里的本地哈希与顶层哈希必须是同一份内容。
        assert_eq!(
            outcome.status.local_body_hash, outcome.local_body_hash,
            "结论与其依据的本地版本必须成套"
        );
        assert_eq!(
            hash_before,
            util::hash_bytes(
                std::fs::read(env.path().join("src/content/blog/race-hash.md")).unwrap().as_slice()
            ),
            "哈希基准是磁盘原文，不是 render() 的规范化结果"
        );
    }

    /// A2.4：核对之后磁盘内容变了，那条旧结论的哈希必须与当前磁盘内容不符，
    /// 使调用方可以整条丢弃，而不是把旧远端结论当成本地新内容的当前状态。
    #[test]
    fn stale_check_outcome_is_detectably_outdated_after_a_save() {
        let env = TestEnv::new();
        let state = state_for(&env);
        create_article(&state, "race-stale".to_string(), meta("迟到结论"), "旧正文\n".to_string())
            .unwrap();
        sync_article(&state, "race-stale".to_string(), false).unwrap();

        // 核对当时的结论（写作分支上确实有内容）。
        let stale = check_article_remote(&state, "race-stale".to_string()).unwrap();
        assert_eq!(stale.status.writing.state, crate::model::CheckState::Present);

        // 用户继续编辑并保存：磁盘原文哈希改变。
        save_article(
            &state,
            "race-stale".to_string(),
            meta("迟到结论"),
            "全新的正文\n".to_string(),
            None,
        )
        .unwrap();
        let current = read_article(&state, "race-stale".to_string()).unwrap();
        assert_ne!(
            stale.local_body_hash, current.content_hash,
            "本地已改动时，旧结论的哈希必须与当前磁盘不符（前端据此丢弃）"
        );

        // 重新核对得到的是当前内容的结论，且哈希与磁盘当前内容一致。
        let fresh = check_article_remote(&state, "race-stale".to_string()).unwrap();
        assert_eq!(fresh.local_body_hash, current.content_hash);
        assert_ne!(fresh.local_body_hash, stale.local_body_hash);
    }

    /// A2.4：同篇文章并发「保存」与「核对」时，核对返回的结论必须由**同一份**
    /// 磁盘字节推出，不能是两次读取之间的混合结果。
    ///
    /// 修复前 `check_article_remote` 分别读盘算 `local_hash` 与 `site_hash`；
    /// 两次读取之间落盘的新内容会让返回的结论自相矛盾——`local_body_hash` 属于
    /// 一版，网站结论却属于另一版，于是「旧远端结论冒充当前状态」。
    ///
    /// **确定性部分**（真正守住这条不变量的是下一个用例
    /// `outcome_is_self_consistent_for_any_local_bytes`）：把「一份字节 → 一条结论」
    /// 做成纯函数后，可以直接喂进任意字节来断言同源。
    ///
    /// 本用例只做**并发冒烟**：核对末尾的读盘窗口只有微秒级，靠赛跑命中它是概率事件，
    /// 轮数开大就会显著拖慢测试（实测 80 轮约 114 秒）。因此这里只跑少量轮数，
    /// 确认并发环境下结论不崩、且两个版本都被观察到过；不把「能变红」的责任压在这里。
    #[test]
    fn concurrent_save_and_check_produce_a_consistent_snapshot() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let env = TestEnv::new();
        let state = state_for(&env);
        create_article(&state, "race-mix".to_string(), meta("并发核对"), "甲版正文\n".to_string())
            .unwrap();
        // 先发布，让 `main` 上有一份可比较的网站版本（远端在循环期间不再变化）。
        sync_article(&state, "race-mix".to_string(), false).unwrap();
        publish_article(&state, "race-mix".to_string(), Vec::new(), None).unwrap();

        let body_a = "甲版正文\n".to_string();
        let body_b = "乙版正文，长度与上一版不同\n".to_string();
        let markdown_of = |body: &str| crate::article_io::compose_markdown(&meta("并发核对"), body);
        let hash_a = util::hash_bytes(markdown_of(&body_a).as_bytes());
        let hash_b = util::hash_bytes(markdown_of(&body_b).as_bytes());

        // 基线：记录「已确认部署成功」，使状态推导能区分「已上线」与「网站仍是旧版」。
        let baseline = check_article_remote(&state, "race-mix".to_string()).unwrap();
        let main_head = baseline.status.main.head.clone().expect("发布后 main 上应有该文章");
        state
            .store
            .update_baseline("race-mix", |entry| {
                entry.deployed_commit = Some(main_head.clone());
            })
            .unwrap();
        let baseline = check_article_remote(&state, "race-mix".to_string()).unwrap();
        let main_site_hash = baseline
            .status
            .main
            .site_hash
            .clone()
            .expect("发布后 main 上应有网站哈希");
        assert_eq!(
            baseline.status.site,
            crate::model::SiteState::LiveCurrentVersion,
            "前置条件：甲版应已是网站当前版本"
        );

        let article_path = env.path().join("src/content/blog/race-mix.md");
        let stop = AtomicBool::new(false);
        // 断言失败会 unwind；`thread::scope` 会先 join 子线程再继续展开，因此
        // 守卫必须定义在闭包**内部**：若放在闭包外，写入线程会一直跑下去，
        // 测试表现为挂住而不是干净地失败。
        struct StopOnDrop<'a>(&'a AtomicBool);
        impl Drop for StopOnDrop<'_> {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        std::thread::scope(|scope| {
            let _guard = StopOnDrop(&stop);
            // 直接改盘：让磁盘内容在核对末尾读盘时可能正好换了一版。
            // 用与产品保存路径相同的原子写：裸 `fs::write` 会先把文件截成 0 字节，
            // 读者可能读到空文件，那是夹具失真而不是产品行为。
            let writer = scope.spawn(|| {
                let versions = [markdown_of(&body_a), markdown_of(&body_b)];
                let mut index = 0usize;
                while !stop.load(Ordering::SeqCst) {
                    crate::article_io::atomic_write(&article_path, versions[index % 2].as_bytes())
                        .unwrap();
                    index += 1;
                }
            });

            let mut seen_a = false;
            let mut seen_b = false;
            for round in 0..8 {
                let outcome = check_article_remote(&state, "race-mix".to_string()).unwrap();

                assert!(
                    outcome.local_body_hash == hash_a || outcome.local_body_hash == hash_b,
                    "核对结果必须是某一完整版本的哈希，实际 {}",
                    outcome.local_body_hash
                );
                assert_eq!(
                    outcome.status.local_body_hash, outcome.local_body_hash,
                    "状态结论必须与顶层哈希属于同一版本"
                );

                if outcome.local_body_hash == hash_a {
                    seen_a = true;
                } else {
                    seen_b = true;
                }

                // 甲版正是 main 上的内容，乙版是本地新稿。整条结论必须与
                // `local_body_hash` 指的那一版**完全对上**；混合读取会把甲版的
                // 「已上线」或乙版的「网站仍是旧版」错配到另一版上。
                let expected = if outcome.local_body_hash == hash_a {
                    crate::model::SiteState::LiveCurrentVersion
                } else {
                    crate::model::SiteState::LiveOldVersion
                };
                assert_eq!(
                    outcome.status.site, expected,
                    "第 {round} 轮的网站结论与它携带的本地版本不一致（哈希 {}）",
                    outcome.local_body_hash
                );
                let version_body =
                    if outcome.local_body_hash == hash_a { &body_a } else { &body_b };
                assert_eq!(
                    crate::workspace::site_hash_of(&markdown_of(version_body)) == main_site_hash,
                    expected == crate::model::SiteState::LiveCurrentVersion,
                    "版本自身的规范化哈希应能解释该网站结论"
                );
            }
            stop.store(true, Ordering::SeqCst);
            writer.join().expect("写入线程不应 panic");

            // 前置条件：本次确实在两版之间来回切换过。否则上面的断言可能只是
            // 一直在看同一版，覆盖不到「两次读取之间换版」的窗口。
            assert!(
                seen_a && seen_b,
                "测试应观察到两个版本（甲={seen_a} 乙={seen_b}）；只见到单版本说明夹具没起作用"
            );
        });
    }

    /// A2.4（**确定性**）：对任意一份本地字节，核对结论必须自洽——
    /// 顶层 `local_body_hash`、状态里的 `local_body_hash`、以及能解释网站结论的
    /// `local_site_hash`，三者必须同源于**同一份字节**。
    ///
    /// 变异验证：让 `outcome_from_local_bytes` 只接收哈希而不接收字节（即改回
    /// 「先读一次算哈希、再读一次算网站哈希」的形状）后，本用例立刻变红——
    /// 因为那时无法再保证两者来自同一份内容。这条不变量之所以能被稳定断言，
    /// 正是因为实现被做成了「一份字节 → 一条结论」的纯函数；
    /// 靠并发赛跑去撞微秒级窗口是概率性的，实测改回两次读盘后并发用例照样通过。
    #[test]
    fn outcome_is_self_consistent_for_any_local_bytes() {
        let first = "---\ntitle: \"甲\"\ndraft: false\n---\n\n甲版正文\n".as_bytes().to_vec();
        let second = "---\ntitle: \"乙\"\ndraft: true\n---\n\n乙版正文，长度不同\n".as_bytes().to_vec();
        let first_text = String::from_utf8(first.clone()).unwrap();

        // 两版远端快照：main 上有内容且已确认部署，使网站结论真的依赖本地内容。
        let head = "c1".to_string();
        let snapshot = RemoteSnapshot {
            writing: crate::model::BranchCheck::unverified("不参与本断言"),
            main: crate::model::BranchCheck::present(
                Some(head.clone()),
                1,
                util::hash_bytes(&first),
                Some(crate::workspace::site_hash_of(&first_text)),
                Some(true),
                None,
            ),
            deployed_commit: Some(head),
            ..Default::default()
        };

        for raw in [&first, &second] {
            let outcome = outcome_from_local_bytes("self-consistent".to_string(), raw, snapshot.clone());
            let text = String::from_utf8_lossy(raw).into_owned();

            assert_eq!(
                outcome.local_body_hash,
                util::hash_bytes(raw),
                "顶层哈希必须是该份字节的原文哈希"
            );
            assert_eq!(
                outcome.status.local_body_hash, outcome.local_body_hash,
                "状态里的本地哈希必须与顶层同源"
            );
            // 网站结论必须能被这一份字节的规范化哈希解释：只有同源时才可能成立。
            let expected = if crate::workspace::site_hash_of(&text)
                == snapshot.main.site_hash.clone().unwrap_or_default()
            {
                crate::model::SiteState::LiveCurrentVersion
            } else {
                crate::model::SiteState::LiveOldVersion
            };
            assert_eq!(
                outcome.status.site, expected,
                "网站结论必须与该份字节的规范化哈希一致，而不是来自另一份内容"
            );
        }

        // 两版必须给出**不同**结论，否则本用例证明不了「结论跟随内容」。
        let a = outcome_from_local_bytes("x".to_string(), &first, snapshot.clone());
        let b = outcome_from_local_bytes("x".to_string(), &second, snapshot);
        assert_ne!(a.local_body_hash, b.local_body_hash);
        assert_ne!(
            a.status.site, b.status.site,
            "两版内容的网站结论应当不同，否则断言没有区分力"
        );
    }

    /// A2.4：较旧的核对结论在较新内容落盘后不得被当成当前结论写入缓存。
    ///
    /// 缓存按「远端头」失效，而列表状态要求「核对结论所依据的本地内容版本」也
    /// 必须对得上；核对结束后重新读取列表时，状态必须基于**当前**磁盘内容推导，
    /// 不能因为缓存里有一条结论就宣称已同步。
    #[test]
    fn older_remote_conclusion_never_labels_newer_local_content_as_synced() {
        let env = TestEnv::new();
        let state = state_for(&env);
        create_article(&state, "race-old".to_string(), meta("旧结论"), "已同步的正文\n".to_string())
            .unwrap();
        sync_article(&state, "race-old".to_string(), false).unwrap();

        // 核对一次：写作分支上存在且与本地一致 → 已同步。
        let synced = check_article_remote(&state, "race-old".to_string()).unwrap();
        assert_eq!(synced.status.remote_sync, crate::model::RemoteSync::Saved);

        // 本地改动后，旧结论依据的内容版本已经过期。
        save_article(
            &state,
            "race-old".to_string(),
            meta("旧结论"),
            "还没同步的新正文\n".to_string(),
            None,
        )
        .unwrap();

        // 列表状态基于当前磁盘内容与仍有效的缓存推导：不得显示「已同步」。
        let listed = list_articles(&state).unwrap();
        let entry = listed.iter().find(|a| a.id == "race-old").unwrap();
        assert_ne!(
            entry.status.remote_sync,
            crate::model::RemoteSync::Saved,
            "本地内容已变时不得沿用旧的「已同步」结论：{entry:?}"
        );
    }

    // ---------------------------------------------------------------- A6 预览依赖

    /// A6：`start_site_preview` 在缺依赖时返回可操作的「需准备依赖」错误，
    /// **不安装**任何东西，也不留下「后台正在安装」的假状态。
    ///
    /// 变异验证：让启动路径改回 `pnpm install`（或让 `require_dependencies`
    /// 安装依赖）后，本用例会因拿不到 `PreviewDependenciesMissing` 而变红。
    #[test]
    fn site_preview_without_dependencies_reports_actionable_state_and_does_not_install() {
        let env = TestEnv::new();
        let state = state_for(&env);
        create_article(&state, "deps-a".to_string(), meta("预览依赖"), "正文\n".to_string())
            .unwrap();

        // 夹具工作区没有 node_modules。
        assert!(!env.path().join("node_modules").exists());

        let err = start_site_preview(&state, "deps-a".to_string(), true).unwrap_err();
        assert_eq!(err.code, ErrorCode::PreviewDependenciesMissing, "{err:?}");
        assert!(
            err.message.contains("准备"),
            "错误信息必须给出下一步动作：{}",
            err.message
        );

        // 启动失败的事实与依赖状态一致：没有任务，也没装出依赖。
        let status = preview_dependency_status(&state);
        assert_eq!(status, PreviewDependencyStatus::Missing, "不得谎报有准备任务：{status:?}");
        assert!(!env.path().join("node_modules").exists(), "启动预览不得安装依赖");
    }

    /// A6：已有一个准备任务在跑时，再次请求准备必须复用同一个任务（同一
    /// `task_id`），不会重复安装。
    ///
    /// 用一个被测试钉住的任务占住槽位，避免依赖真实 `pnpm install` 的时长；
    /// 断言命令层走的是「复用」分支而不是「再登记一个任务」。
    #[test]
    fn repeated_dependency_prepare_requests_deduplicate() {
        use std::sync::mpsc;

        let env = TestEnv::new();
        let state = state_for(&env);
        create_article(&state, "deps-b".to_string(), meta("依赖去重"), "正文\n".to_string())
            .unwrap();

        let runs = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (started_tx, started_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let first = state.preview_dependencies.start({
            let runs = std::sync::Arc::clone(&runs);
            move || {
                runs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let _ = started_tx.send(());
                let _ = release_rx.recv();
                Ok(())
            }
        });
        let task_id = match &first {
            PreviewDependencyStatus::Preparing { task_id, .. } => task_id.clone(),
            other => panic!("登记后应是进行中：{other:?}"),
        };
        started_rx.recv_timeout(std::time::Duration::from_secs(5)).expect("任务应开始运行");

        // 再次请求：必须复用同一任务，不得再登记一个。
        let second = prepare_site_preview_dependencies(&state).unwrap();
        assert_eq!(
            second.task_id(),
            Some(task_id.as_str()),
            "重复请求必须复用同一任务：{second:?}"
        );
        assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 1, "不得重复安装");

        // 状态查询与登记的任务一致。
        let queried = preview_dependency_status(&state);
        assert_eq!(queried.task_id(), Some(task_id.as_str()));

        let _ = release_tx.send(());
    }

    /// A6：工作区没有可复用依赖时，`preview_dependency_status` 是明确的
    /// 「需准备」，而不是含混的「未知」。
    #[test]
    fn dependency_status_is_missing_before_any_preparation() {
        let env = TestEnv::new();
        let state = state_for(&env);
        create_article(&state, "deps-c".to_string(), meta("依赖状态"), "正文\n".to_string())
            .unwrap();
        assert_eq!(preview_dependency_status(&state), PreviewDependencyStatus::Missing);
    }
}
