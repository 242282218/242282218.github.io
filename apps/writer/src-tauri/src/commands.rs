//! 任务队列与命令参数校验。
//!
//! 队列保证同一篇文章的同步／发布／删除**串行执行**，不会互相并发；
//! 不同文章可以继续各自处理，不因某一篇的冲突而全局冻结。

use crate::model::{ErrorCode, Result, WriterError};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};

/// 任务类型，用于互斥键构造与日志。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TaskKind {
    SaveLocal,
    Sync,
    Publish,
    Withdraw,
    Delete,
    Restore,
    RenameUrl,
    SitePreview,
}

impl TaskKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SaveLocal => "save-local",
            Self::Sync => "sync",
            Self::Publish => "publish",
            Self::Withdraw => "withdraw",
            Self::Delete => "delete",
            Self::Restore => "restore",
            Self::RenameUrl => "rename-url",
            Self::SitePreview => "site-preview",
        }
    }
}

/// 队列中的一个待执行项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueItem {
    pub id: u64,
    pub article_id: String,
    pub kind: TaskKind,
}

/// 串行任务队列。
///
/// 互斥粒度是「文章 + 远端写操作」：同一篇文章的远端写操作必须串行，
/// 避免同篇同步与发布并发；纯本地任务不占用远端锁。
pub struct TaskQueue {
    inner: Arc<Mutex<QueueState>>,
}

/// 队列内部状态。
#[derive(Debug)]
struct QueueState {
    next_id: u64,
    /// 当前被占用的互斥键（远端写操作）。
    busy_keys: HashSet<String>,
    /// 等待中的任务。
    pending: VecDeque<QueueItem>,
    /// 已完成的最近任务（用于界面显示与诊断）。
    recent: VecDeque<QueueItem>,
}

/// 一次已获取的独占执行许可。
#[derive(Debug)]
pub struct QueuePermit {
    queue: Arc<Mutex<QueueState>>,
    key: String,
    item: QueueItem,
}

impl QueuePermit {
    pub fn item(&self) -> &QueueItem {
        &self.item
    }

    /// 释放许可并记录完成。
    pub fn finish(self) {
        let mut state = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        state.busy_keys.remove(&self.key);
        state.recent.push_back(self.item);
        while state.recent.len() > 50 {
            state.recent.pop_front();
        }
    }
}

impl Default for TaskQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl TaskQueue {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(QueueState {
                next_id: 1,
                busy_keys: HashSet::new(),
                pending: VecDeque::new(),
                recent: VecDeque::new(),
            })),
        }
    }

    /// 互斥键：远端写操作按文章互斥；本地任务使用独立通道。
    fn key_for(kind: TaskKind, article_id: &str) -> String {
        match kind {
            TaskKind::SaveLocal => format!("local:{article_id}"),
            TaskKind::SitePreview => "preview".to_string(),
            _ => format!("remote:{article_id}"),
        }
    }

    /// 尝试获取独占许可；已被占用时返回 `None`，调用方应提示「该文章正在处理」。
    pub fn try_acquire(&self, kind: TaskKind, article_id: &str) -> Option<QueuePermit> {
        let key = Self::key_for(kind, article_id);
        let mut state = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if state.busy_keys.contains(&key) {
            let id = state.next_id;
            state.next_id += 1;
            state.pending.push_back(QueueItem {
                id,
                article_id: article_id.to_string(),
                kind,
            });
            return None;
        }
        state.busy_keys.insert(key.clone());
        let id = state.next_id;
        state.next_id += 1;
        Some(QueuePermit {
            queue: Arc::clone(&self.inner),
            key,
            item: QueueItem { id, article_id: article_id.to_string(), kind },
        })
    }

    /// 获取许可，被占用时报出可操作错误而不是阻塞界面。
    pub fn acquire(&self, kind: TaskKind, article_id: &str) -> Result<QueuePermit> {
        self.try_acquire(kind, article_id).ok_or_else(|| {
            WriterError::new(
                ErrorCode::InvalidArgument,
                "这篇文章上已有正在进行的远端操作，请等待它结束",
            )
            .with_detail(format!("{} @ {article_id}", kind.as_str()))
        })
    }

    /// 当前等待队列长度。
    pub fn pending_len(&self) -> usize {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).pending.len()
    }

    /// 某篇文章是否有远端操作在进行。
    pub fn is_busy(&self, article_id: &str) -> bool {
        let state = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        state.busy_keys.contains(&format!("remote:{article_id}"))
    }
}

/// 命令层可复用的「执行 + 释放」包装。
pub fn run_exclusive<T>(
    queue: &TaskQueue,
    kind: TaskKind,
    article_id: &str,
    action: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let permit = queue.acquire(kind, article_id)?;
    let result = action();
    permit.finish();
    result
}

/// 校验文章 ID 参数（允许既有中文 ID）。
pub fn validate_article_id(article_id: &str) -> Result<()> {
    crate::paths::validate_existing_article_id(article_id)
}

/// 校验新建文章 ID 参数。
pub fn validate_new_article_id(article_id: &str) -> Result<()> {
    crate::paths::validate_new_article_id(article_id)
}

/// 校验元数据参数，给出靠近字段的错误。
pub fn validate_meta(meta: &crate::model::ArticleMeta) -> Result<()> {
    use crate::model::ErrorCode as Code;

    // 日期错误要带上字段名，界面才能在对应输入框附近提示。
    let with_field = |result: Result<()>, field: &str| -> Result<()> {
        result.map_err(|err| WriterError { detail: Some(field.to_string()), ..err })
    };

    if meta.title.trim().is_empty() {
        return Err(WriterError::new(Code::MetaFieldInvalid, "标题不能为空").with_detail("title"));
    }
    if meta.description.trim().is_empty() {
        return Err(
            WriterError::new(Code::MetaFieldInvalid, "摘要不能为空").with_detail("description")
        );
    }
    with_field(crate::paths::validate_calendar_date(&meta.pub_date), "pubDate")?;
    if let Some(updated) = &meta.updated_date {
        if !updated.trim().is_empty() {
            with_field(crate::paths::validate_calendar_date(updated), "updatedDate")?;
        }
    }
    for tag in &meta.tags {
        if tag.trim().is_empty() {
            return Err(
                WriterError::new(Code::MetaFieldInvalid, "标签不能为空").with_detail("tags")
            );
        }
    }
    Ok(())
}

/// 统计各状态的文章数，供界面概览。
pub fn summarize_counts(
    statuses: &[(crate::model::RemoteSync, crate::model::SiteState)],
) -> HashMap<&'static str, usize> {
    let mut out = HashMap::new();
    for (sync, site) in statuses {
        *out.entry(match sync {
            crate::model::RemoteSync::Unverified => "待核对",
            crate::model::RemoteSync::LocalOnly => "仅本地",
            crate::model::RemoteSync::Saving => "同步中",
            crate::model::RemoteSync::Saved => "远程已存",
            crate::model::RemoteSync::Conflict => "冲突",
            crate::model::RemoteSync::Failed => "同步失败",
        })
        .or_insert(0) += 1;
        *out.entry(match site {
            crate::model::SiteState::Unverified => "待核对",
            crate::model::SiteState::NeverPublished => "从未发布",
            crate::model::SiteState::LiveOldVersion => "网站仍是旧版",
            crate::model::SiteState::PublicationSubmitted => "已提交发布",
            crate::model::SiteState::Deploying => "部署中",
            crate::model::SiteState::LiveCurrentVersion => "网站已上线",
            crate::model::SiteState::DeployFailed => "部署失败",
            crate::model::SiteState::Withdrawn => "已从网站撤下",
        })
        .or_insert(0) += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ArticleMeta, RemoteSync, SiteState};

    fn meta() -> ArticleMeta {
        ArticleMeta {
            title: "标题".to_string(),
            description: "摘要".to_string(),
            pub_date: "2026-09-27".to_string(),
            updated_date: None,
            tags: vec!["标签".to_string()],
            draft: true,
        }
    }

    #[test]
    fn remote_operations_on_same_article_are_mutually_exclusive() {
        let queue = TaskQueue::new();
        let first = queue.acquire(TaskKind::Sync, "a").unwrap();
        // 同篇文章的第二个远端操作被拒绝。
        assert!(queue.acquire(TaskKind::Publish, "a").is_err());
        assert!(queue.is_busy("a"));
        assert_eq!(queue.pending_len(), 1);

        // 不同文章可以继续。
        let other = queue.acquire(TaskKind::Sync, "b").unwrap();

        first.finish();
        assert!(!queue.is_busy("a"));
        // 释放后可以再次获取。
        assert!(queue.acquire(TaskKind::Publish, "a").is_ok());
        other.finish();
    }

    #[test]
    fn local_save_does_not_block_remote_operations() {
        let queue = TaskQueue::new();
        let remote = queue.acquire(TaskKind::Sync, "a").unwrap();
        // 本地保存走独立通道，不被远端操作阻塞（输入时不触发远端 Git）。
        let local = queue.acquire(TaskKind::SaveLocal, "a").unwrap();
        local.finish();
        remote.finish();
    }

    #[test]
    fn run_exclusive_releases_on_success_and_failure() {
        let queue = TaskQueue::new();
        let ok = run_exclusive(&queue, TaskKind::Sync, "a", || Ok(1));
        assert_eq!(ok.unwrap(), 1);
        assert!(!queue.is_busy("a"));

        let err: Result<()> =
            run_exclusive(&queue, TaskKind::Sync, "a", || Err(WriterError::new(ErrorCode::GitFailed, "x")));
        assert!(err.is_err());
        // 失败也必须释放，避免文章被永久锁死。
        assert!(!queue.is_busy("a"));
    }

    #[test]
    fn concurrent_acquire_attempts_are_reported_not_blocked() {
        let queue = TaskQueue::new();
        let _held = queue.acquire(TaskKind::Delete, "a").unwrap();
        let err = queue.acquire(TaskKind::Delete, "a").unwrap_err();
        assert!(err.message.contains("正在进行"));
    }

    #[test]
    fn validates_article_ids_and_meta() {
        assert!(validate_article_id("read-code").is_ok());
        assert!(validate_article_id("观澜/记录").is_ok());
        assert!(validate_article_id("../escape").is_err());
        assert!(validate_new_article_id("NewPost").is_err());
        assert!(validate_new_article_id("new-post").is_ok());

        assert!(validate_meta(&meta()).is_ok());

        let mut empty_title = meta();
        empty_title.title = "  ".to_string();
        let err = validate_meta(&empty_title).unwrap_err();
        assert_eq!(err.detail.as_deref(), Some("title"));

        let mut bad_date = meta();
        bad_date.pub_date = "2026-02-30".to_string();
        assert!(validate_meta(&bad_date).is_err());

        let mut empty_tag = meta();
        empty_tag.tags = vec![" ".to_string()];
        assert!(validate_meta(&empty_tag).is_err());

        let mut blank_updated = meta();
        blank_updated.updated_date = Some("  ".to_string());
        assert!(validate_meta(&blank_updated).is_ok(), "空白 updatedDate 视为未设置");
    }

    #[test]
    fn writer_error_serializes_with_code_for_frontend() {
        let err = WriterError::new(ErrorCode::Offline, "网络不可用");
        let json = serde_json::to_string(&err).unwrap();
        assert!(json.contains("\"offline\""), "{json}");
        assert!(json.contains("网络不可用"));
        // 前端按错误码分支处理，不依赖文案。
        let back: WriterError = serde_json::from_str(&json).unwrap();
        assert_eq!(back.code, ErrorCode::Offline);
    }

    #[test]
    fn counts_are_grouped_by_explicit_state_labels() {
        let counts = summarize_counts(&[
            (RemoteSync::LocalOnly, SiteState::NeverPublished),
            (RemoteSync::Saved, SiteState::LiveOldVersion),
            (RemoteSync::Saved, SiteState::LiveCurrentVersion),
        ]);
        assert_eq!(counts.get("仅本地"), Some(&1));
        assert_eq!(counts.get("远程已存"), Some(&2));
        assert_eq!(counts.get("网站仍是旧版"), Some(&1));
    }
}
