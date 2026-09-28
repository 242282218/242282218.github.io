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
    /// 远端状态**尚未核对**（或核对失败）。不得据此推断「已同步」或「尚未同步」。
    Unverified,
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
    /// 网站状态**尚未核对**（或核对失败）。不得据此推断「从未发布」或「已上线」。
    Unverified,
    NeverPublished,
    LiveOldVersion,
    PublicationSubmitted,
    Deploying,
    LiveCurrentVersion,
    DeployFailed,
    Withdrawn,
}

/// 单个远端分支上一次核对的结论类别。
///
/// `None`（缺值）无法区分「没查过」「查了但失败」「确认不存在」——三者对用户
/// 意味着完全不同的下一步操作，因此必须分开表达，未知一律不得等同于肯定结论。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CheckState {
    /// 尚未核对，或核对失败（超时、断网、认证受阻、读取异常）。
    #[default]
    Unverified,
    /// 已核对：该分支或该文章确实不存在。
    Absent,
    /// 已核对：该分支上存在该文章。
    Present,
}

/// 单个远端分支上该文章的核对结论。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchCheck {
    pub state: CheckState,
    /// 未核对时的原因（面向用户），`state == unverified` 时展示。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// 核对时所依据的远端头。已核对时为 `Some`（分支不存在则为 `None`）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    /// 核对时间（Unix 秒）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked_at_unix: Option<u64>,
    /// 该文件的原始内容哈希（`present` 时才有值，含 `draft` 字段）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_hash: Option<String>,
    /// 该文件按「网站发布版」规范化后的哈希（`present` 时才有值）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub site_hash: Option<String>,
    /// 该分支上是否公开（`draft: false`）。只有 `main` 上有意义。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub published: Option<bool>,
    /// 该分支上该文章记录的上线地址。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deployment_url: Option<String>,
}

impl BranchCheck {
    /// 未核对（可带原因）。
    pub fn unverified(reason: impl Into<String>) -> Self {
        Self { state: CheckState::Unverified, reason: Some(reason.into()), ..Default::default() }
    }

    /// 已核对但该分支上不存在该文章。
    pub fn absent(head: Option<String>, checked_at_unix: u64) -> Self {
        Self {
            state: CheckState::Absent,
            head,
            checked_at_unix: Some(checked_at_unix),
            ..Default::default()
        }
    }

    /// 已核对且存在。
    pub fn present(
        head: Option<String>,
        checked_at_unix: u64,
        body_hash: String,
        site_hash: Option<String>,
        published: Option<bool>,
        deployment_url: Option<String>,
    ) -> Self {
        Self {
            state: CheckState::Present,
            head,
            checked_at_unix: Some(checked_at_unix),
            body_hash: Some(body_hash),
            site_hash,
            published,
            deployment_url,
            ..Default::default()
        }
    }

    pub fn is_verified(&self) -> bool {
        self.state != CheckState::Unverified
    }

    pub fn is_present(&self) -> bool {
        self.state == CheckState::Present
    }
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
    /// `writing` 分支的核对结论（含未核对态）。
    pub writing: BranchCheck,
    /// `main` 分支的核对结论（含未核对态）。
    pub main: BranchCheck,
    /// 远端核对时间（两个分支中最近一次已核对的时间）。未核对时为 `None`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_checked_at_unix: Option<u64>,
}

impl ArticleStatus {
    /// 本地内容尚未纳入任何状态推导时的占位值。
    ///
    /// 远端两分支都是「未核对」：新建文章从未查过远端，不能声称「尚未同步」。
    pub fn local_only(local_body_hash: impl Into<String>) -> Self {
        Self {
            locally_saved: true,
            remote_sync: RemoteSync::Unverified,
            site: SiteState::Unverified,
            local_body_hash: local_body_hash.into(),
            writing: BranchCheck::default(),
            main: BranchCheck::default(),
            remote_checked_at_unix: None,
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
    /// 网站预览所需的依赖尚未准备（不会在启动预览时自行安装）。
    PreviewDependenciesMissing,
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
            Self::PreviewDependenciesMissing => "preview-dependencies-missing",
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
