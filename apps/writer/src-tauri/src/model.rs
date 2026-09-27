//! 前后端共享的文章数据模型与错误类型。
//!
//! 协议字段与 `src/types/article.ts` 一一对应；字段类型显式，不用单个
//! `published: boolean` 概括本地、远程与网站三类状态。

use serde::{Deserialize, Serialize};
use std::fmt;

/// 对 `updatedDate` 字段的一次显式操作。
///
/// 不用 `Option<Option<String>>`：在 JSON 里 `Some(None)` 与 `None` 都会序列化成
/// `null`，语义会丢失。这个枚举把「不改动 / 写入 / 删除」三种意图分开表达。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "action")]
pub enum UpdatedDateAction {
    /// 写入用户确认的日期。
    Set { value: String },
    /// 从 front matter 中移除该字段。
    Remove,
}

/// 文章在站点中的 URL 标识：`src/content/blog/` 下的相对路径，不含扩展名。
///
/// 创建后不随标题变化；改 URL 是独立的显式操作。校验见 [`crate::paths`]。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ArticleId(String);

impl ArticleId {
    /// 由已校验的字符串构造。调用方须先用 [`crate::paths::validate_article_id`]。
    pub fn new_unchecked(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// 文章 Markdown 相对仓库根的路径，如 `src/content/blog/read-code.md`。
    pub fn markdown_rel_path(&self) -> String {
        format!("{}{}.md", crate::paths::BLOG_DIR_PREFIX, self.0)
    }

    /// 文章专属图片相对仓库根的目录，如 `public/blog/read-code`。
    pub fn image_rel_dir(&self) -> String {
        format!("{}{}", crate::paths::IMAGE_DIR_PREFIX, self.0)
    }
}

impl fmt::Display for ArticleId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// 文章的站点元数据，与 `src/content.config.ts` 的 schema 对齐。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArticleMeta {
    pub title: String,
    pub description: String,
    /// `YYYY-MM-DD`，作者希望在博客呈现的日期。
    pub pub_date: String,
    /// 可选；仅在用户确认更新已发布文章元数据时改写。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_date: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub draft: bool,
}

/// 远程写作分支的同步状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RemoteSync {
    LocalOnly,
    Saving,
    Saved,
    Conflict,
    Failed,
}

/// 主站版本与 Pages 部署状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SiteState {
    NeverPublished,
    LiveOldVersion,
    PublicationSubmitted,
    Deploying,
    LiveCurrentVersion,
    DeployFailed,
    Withdrawn,
}

/// 文章在其三处位置（本地 / writing / main）的版本对照。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArticleStatus {
    pub locally_saved: bool,
    pub remote_sync: RemoteSync,
    pub site: SiteState,
    /// 文章文件的完整内容哈希，而非仅正文。
    pub local_body_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub writing_body_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub main_body_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub main_commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deployment_url: Option<String>,
}

impl ArticleStatus {
    /// 本地内容尚未纳入任何状态推导时的占位值。
    pub fn local_only(local_body_hash: impl Into<String>) -> Self {
        Self {
            locally_saved: true,
            remote_sync: RemoteSync::LocalOnly,
            site: SiteState::NeverPublished,
            local_body_hash: local_body_hash.into(),
            writing_body_hash: None,
            main_body_hash: None,
            main_commit: None,
            deployment_url: None,
        }
    }
}

/// 文章来源，用于区分独立工作目录中的文件与显式导入的示例。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ArticleSource {
    Workspace,
    Imported,
}

/// 列表项：仅展示 ID、标题、摘要、标签、日期、图片数及来源。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArticleSummary {
    pub id: String,
    pub title: String,
    pub description: String,
    pub tags: Vec<String>,
    pub pub_date: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_date: Option<String>,
    pub draft: bool,
    pub image_count: usize,
    pub source: ArticleSource,
    /// 本机记录的最近编辑时间（Unix 秒）。用于「按最近编辑」排序；
    /// 与网站按 `pubDate` 的排序可能不同。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_edited_unix: Option<u64>,
    pub status: ArticleStatus,
    /// 读取或解析异常。存在时条目仍要展示出来，并给出原文入口，
    /// 不能静默跳过。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub load_error: Option<WriterError>,
}

/// 编辑页所需完整内容：raw front matter、字段、正文与版本 ID。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArticleContent {
    pub id: String,
    pub meta: ArticleMeta,
    /// front matter 的原文（不含 `---` 围栏），供源码级核对。
    pub raw_front_matter: String,
    pub body: String,
    /// 当前磁盘文件完整内容的哈希。
    pub content_hash: String,
    pub status: ArticleStatus,
}

/// 图片文件的引用情况，用于删除前的独占性判定。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageRef {
    pub rel_path: String,
    pub size: u64,
    pub content_hash: String,
    /// 引用该图片的文章 ID 列表（含跨文章引用）。
    pub referenced_by: Vec<String>,
}

/// 业务错误码。前端据此给出操作性提示，不暴露内部路径细节。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ErrorCode {
    /// 文件不以 front matter 围栏开头。
    FrontMatterMissing,
    /// front matter 缺少结束围栏。
    FrontMatterUnterminated,
    /// YAML 语法错误，已定位但不允许自动修复后覆盖。
    FrontMatterInvalid,
    /// 必需的站点字段缺失或类型不符。
    MetaFieldInvalid,
    /// 文章 ID 非法（保留名、大小写冲突、越界等）。
    ArticleIdInvalid,
    /// 文章已存在。
    ArticleExists,
    /// 文章不存在。
    ArticleNotFound,
    /// 路径越出受管目录。
    PathOutOfScope,
    /// 图片格式或大小不符。
    ImageUnsupported,
    /// 图片过大。
    ImageTooLarge,
    /// Git 操作失败。
    GitFailed,
    /// 远端版本已变化，需要冲突处理。
    RemoteChanged,
    /// 推送被拒绝（非快进）。
    PushRejected,
    /// 认证失败。
    AuthFailed,
    /// 网络不可用。
    Offline,
    /// 网站依赖（Node/pnpm/Git）缺失。
    ToolchainMissing,
    /// 本地预览不可用。
    PreviewFailed,
    /// 构建或部署失败。
    BuildFailed,
    /// 磁盘错误。
    IoFailed,
    /// 参数校验失败。
    InvalidArgument,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FrontMatterMissing => "front-matter-missing",
            Self::FrontMatterUnterminated => "front-matter-unterminated",
            Self::FrontMatterInvalid => "front-matter-invalid",
            Self::MetaFieldInvalid => "meta-field-invalid",
            Self::ArticleIdInvalid => "article-id-invalid",
            Self::ArticleExists => "article-exists",
            Self::ArticleNotFound => "article-not-found",
            Self::PathOutOfScope => "path-out-of-scope",
            Self::ImageUnsupported => "image-unsupported",
            Self::ImageTooLarge => "image-too-large",
            Self::GitFailed => "git-failed",
            Self::RemoteChanged => "remote-changed",
            Self::PushRejected => "push-rejected",
            Self::AuthFailed => "auth-failed",
            Self::Offline => "offline",
            Self::ToolchainMissing => "toolchain-missing",
            Self::PreviewFailed => "preview-failed",
            Self::BuildFailed => "build-failed",
            Self::IoFailed => "io-failed",
            Self::InvalidArgument => "invalid-argument",
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 结构化业务错误，序列化后交给前端展示。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriterError {
    pub code: ErrorCode,
    /// 面向用户的中文说明。
    pub message: String,
    /// 可选的定位信息（行号、字段名、相对路径），不含凭据。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl WriterError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self { code, message: message.into(), detail: None }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

impl fmt::Display for WriterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)
    }
}

impl std::error::Error for WriterError {}

/// 统一 Result 别名。
pub type Result<T> = std::result::Result<T, WriterError>;
