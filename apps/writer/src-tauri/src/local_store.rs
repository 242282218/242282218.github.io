//! 应用数据目录中的本地持久数据：设置、写作偏好、最近删除、崩溃恢复、版本索引。
//!
//! 这些内容**只**保存在 Windows 每用户应用数据目录，不进入公开仓库；
//! 结构带明确的 `schemaVersion`，升级时只迁移本目录中的已知结构。

use crate::model::{ErrorCode, Result, WriterError};
use crate::util;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// 当前数据结构版本。仅当结构变化时递增。
pub const SCHEMA_VERSION: u32 = 1;

/// 应用数据目录名。
const APP_DIR_NAME: &str = "guanlanzhi-writer";

/// 覆盖应用数据根目录的环境变量名。
///
/// 仅用于自动化测试与便携部署，让软件使用一个新的数据目录，
/// 从而可以重复验证「首次启动」路径而不污染真实用户数据。
pub const DATA_DIR_ENV: &str = "GUANLANZHI_WRITER_DATA_DIR";

/// 默认目标仓库（公开仓库，仅用于克隆与推送）。
pub const DEFAULT_REPO_LABEL: &str = "guanlangzg/guanlangzg.github.io";
/// 默认远端 URL。
pub const DEFAULT_REPO_URL: &str = "https://github.com/guanlangzg/guanlangzg.github.io.git";

/// 写作外观偏好。只影响本机阅读与编辑体验，绝不写入文章或网站 CSS。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WritingPreferences {
    /// 右侧即时排版的字号（px）。
    pub font_size: u16,
    /// 行高百分比。
    pub line_height: u16,
    /// 代码主题标识（由前端映射到实际主题）。
    pub code_theme: String,
    /// 预览面板宽度（px）。
    pub preview_width: u16,
    /// 编辑器模式：`sv`（源码分屏）或 `ir`（即时渲染）。
    pub editor_mode: String,
    /// 自动保存停顿防抖（毫秒）。
    pub auto_save_debounce_ms: u16,
    /// 外壳配色：`system`（默认，跟随系统）/ `light` / `dark`。
    ///
    /// 只作用于软件外壳；站点预览始终浅色（网站只有 light 主题）。
    /// `serde(default)` 不可省略：旧版 `config.json` 没有这个字段，缺失时
    /// 反序列化失败会让整份配置被判为损坏并重置，用户的其他偏好一并丢失。
    #[serde(default = "default_shell_theme")]
    pub shell_theme: String,
}

/// 外壳配色的默认值：跟随系统。
fn default_shell_theme() -> String {
    "system".to_string()
}

impl Default for WritingPreferences {
    fn default() -> Self {
        Self {
            font_size: 16,
            line_height: 175,
            code_theme: "github".to_string(),
            preview_width: 460,
            editor_mode: "sv".to_string(),
            auto_save_debounce_ms: 800,
            shell_theme: default_shell_theme(),
        }
    }
}

impl WritingPreferences {
    /// 收敛到合理范围，避免异常的持久化值破坏界面。
    pub fn clamp(&mut self) {
        self.font_size = self.font_size.clamp(12, 28);
        self.line_height = self.line_height.clamp(120, 260);
        self.preview_width = self.preview_width.clamp(280, 900);
        self.auto_save_debounce_ms = self.auto_save_debounce_ms.clamp(300, 3000);
        if self.editor_mode != "sv" && self.editor_mode != "ir" {
            self.editor_mode = "sv".to_string();
        }
        if self.code_theme.trim().is_empty() {
            self.code_theme = "github".to_string();
        }
        // 未知取值退回「跟随系统」，而不是留一个没有消费者的字符串。
        if !matches!(self.shell_theme.as_str(), "system" | "light" | "dark") {
            self.shell_theme = default_shell_theme();
        }
    }
}

/// 应用设置。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppConfig {
    pub schema_version: u32,
    /// 用户确认过的目标仓库标识（只读显示）。
    pub repo_label: String,
    /// 实际使用的远端 URL。
    pub repo_url: String,
    /// 独立工作目录的绝对路径。
    pub workspace_dir: String,
    /// 首次连接是否已完成。
    pub connected: bool,
    /// 是否已向用户说明「公开仓库会公开远程草稿」。
    pub disclosed_public_drafts: bool,
    #[serde(default)]
    pub preferences: WritingPreferences,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            repo_label: DEFAULT_REPO_LABEL.to_string(),
            repo_url: DEFAULT_REPO_URL.to_string(),
            workspace_dir: String::new(),
            connected: false,
            disclosed_public_drafts: false,
            preferences: WritingPreferences::default(),
        }
    }
}

impl AppConfig {
    fn clamp(&mut self) {
        self.preferences.clamp();
        if self.schema_version == 0 {
            self.schema_version = SCHEMA_VERSION;
        }
        if self.repo_url.trim().is_empty() {
            self.repo_url = DEFAULT_REPO_URL.to_string();
        }
        if self.repo_label.trim().is_empty() {
            self.repo_label = DEFAULT_REPO_LABEL.to_string();
        }
    }
}

/// 每篇文章的版本基线（本机索引，可重建；不作为远端真实状态的替代）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArticleBaseline {
    /// 本机编辑会话开始时记录的本地内容哈希。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edit_base_hash: Option<String>,
    /// 最近一次成功同步到 `writing` 的提交与内容哈希。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub writing_commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub writing_hash: Option<String>,
    /// 最近一次成功发布到 `main` 的提交与内容哈希。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main_commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main_hash: Option<String>,
    /// 该次发布对应的站点上线确认结果。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deployment_url: Option<String>,
    /// 已确认**部署成功**的 `main` 提交（来自 Pages 工作流查询）。
    ///
    /// 只有它等于当前 `main` 头时才允许显示「网站已上线」；仅推送成功不写此字段。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deployed_commit: Option<String>,
    /// 已确认**部署失败**的 `main` 提交。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deploy_failed_commit: Option<String>,
    /// 查询到**部署进行中**的 `main` 提交。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deploying_commit: Option<String>,
    /// 本地记录的最近编辑时间（Unix 秒），用于「按最近编辑」排序。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_edited_unix: Option<u64>,
    /// 该文章最近一次已确认在 `main` 上是否公开。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main_published: Option<bool>,
    /// 本地编辑起始点的图片哈希（相对路径 → 内容哈希），用于检测图片冲突。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub edit_base_image_hashes: BTreeMap<String, String>,
    /// 最近一次成功同步到 `writing` 时的图片哈希。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub writing_image_hashes: BTreeMap<String, String>,
    /// `main` 上该文章的图片哈希（发布基线）。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub main_image_hashes: BTreeMap<String, String>,
}

/// 全部文章的版本基线索引。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionIndex {
    pub schema_version: u32,
    #[serde(default)]
    pub articles: BTreeMap<String, ArticleBaseline>,
}

/// 远端核对缓存中的一条记录。
///
/// 缓存**只**用于省掉重复的远端读取，不能替代事实：只有先成功取得当前远端头，
/// 且头与 `head` 一致时才允许复用。因此每条记录都必须带上它成立时的头提交。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteCheckEntry {
    /// 两个分支各自的核对结论。`head` 为该条记录成立时的远端头。
    pub writing: crate::model::BranchCheck,
    pub main: crate::model::BranchCheck,
    /// 仓库标识（远端 URL 归一化后的值），换仓库后缓存立即失效。
    pub repo: String,
}

/// 远端核对缓存：以「仓库 ＋ 分支 ＋ 文章路径」为键。
///
/// 键里不含头提交：头是**复用条件**（`RemoteCheckEntry` 内记录），
/// 而不是键的一部分——同一篇文章在不同头下的结论需要互相覆盖，
/// 而不是无限堆积。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteCheckCache {
    pub schema_version: u32,
    /// 键为 `repo::article_id`，值为该文章两分支的最近一次结论。
    #[serde(default)]
    pub entries: BTreeMap<String, RemoteCheckEntry>,
}

impl RemoteCheckCache {
    /// 缓存键：仓库 ＋ 文章。分支在值内部区分。
    pub fn key(repo: &str, article_id: &str) -> String {
        format!("{repo}::{article_id}")
    }

    fn load_or_default(path: &Path) -> Self {
        let empty = || Self { schema_version: SCHEMA_VERSION, entries: BTreeMap::new() };
        let Ok(bytes) = fs::read(path) else {
            return empty();
        };
        match serde_json::from_slice::<Self>(&bytes) {
            Ok(mut cache) => {
                cache.schema_version = SCHEMA_VERSION;
                cache
            }
            // 缓存是可重建的派生数据：损坏时直接丢弃，不留档也不报错。
            Err(_) => empty(),
        }
    }

    pub fn get(&self, repo: &str, article_id: &str) -> Option<&RemoteCheckEntry> {
        self.entries.get(&Self::key(repo, article_id))
    }

    pub fn put(&mut self, repo: &str, article_id: &str, entry: RemoteCheckEntry) {
        self.entries.insert(Self::key(repo, article_id), entry);
    }
}

impl VersionIndex {
    fn load_or_default(path: &Path) -> Self {
        let empty = || Self { schema_version: SCHEMA_VERSION, articles: BTreeMap::new() };
        // 先读字节：**只有确实读到内容却无法解析**时才留档。
        // 读取本身失败（文件不存在、被占用、权限不足）无法区分「损坏」还是
        // 「暂时读不到」，此时重命名会把好文件改名，因此不做留档。
        let Ok(bytes) = fs::read(path) else {
            return empty();
        };
        let Ok(text) = String::from_utf8(bytes) else {
            preserve_corrupt(path);
            return empty();
        };
        match serde_json::from_str::<Self>(&text) {
            Ok(mut index) => {
                index.schema_version = SCHEMA_VERSION;
                index
            }
            Err(_) => {
                // 解析失败不静默清空：保留一份 `.bak` 供手工恢复，再返回空索引。
                preserve_corrupt(path);
                empty()
            }
        }
    }

    pub fn get(&self, article_id: &str) -> Option<&ArticleBaseline> {
        self.articles.get(article_id)
    }
}

/// 把损坏的索引文件另存为 `<原文件>.bak`，避免下一次保存静默覆盖它。
///
/// 只在「读到了内容但无法解析」时调用；读取失败不在此列（无法区分损坏与
/// 暂时不可读，重命名反而可能破坏好文件）。留档失败时也只能继续，因为核心
/// 目标是让程序仍然可用。
fn preserve_corrupt(path: &Path) {
    let mut bak = path.as_os_str().to_owned();
    bak.push(".bak");
    let _ = fs::rename(path, std::path::PathBuf::from(bak));
}

/// 最近删除 / 撤下的条目类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TrashKind {
    /// 从两个分支删除文件，但保留本地副本。
    Deleted,
    /// 仅标记为非公开，正文与图片仍在。
    Withdrawn,
}

/// 回收区中保留的图片。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrashImage {
    pub rel_path: String,
    /// 回收副本在本条目目录中的文件名。
    pub backup_name: String,
    pub size: u64,
    pub content_hash: String,
}

/// 一条回收记录。含操作 ID 与两个分支的完成状态，用于部分失败后的幂等重试。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrashEntry {
    pub op_id: String,
    pub kind: TrashKind,
    pub article_id: String,
    pub title: String,
    /// 本地日期。
    pub deleted_at: String,
    pub markdown_rel_path: String,
    /// 是否保留了 Markdown 正文副本。
    pub has_markdown: bool,
    /// 正文副本在本条目目录中的**确切文件名**。
    ///
    /// 恢复时必须按这个名字读取，不能在该目录里「随便找一个 .md」：
    /// `sanitize_image_basename` 会压平并截断文件名（`观澜/记录` → `观澜`），
    /// 依赖猜测有恢复到错误内容的风险。旧条目缺该字段时按文章 ID 现算一次。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub markdown_backup_name: Option<String>,
    /// 该文章目录下的**全部**图片副本（含被其他文章引用的），用于恢复。
    #[serde(default)]
    pub images: Vec<TrashImage>,
    /// 删除时确认**无其他文章引用**的独占图片；只有这些才允许从远端分支删除。
    ///
    /// 与 `images` 分开记录：`images` 是恢复所需的全部副本，若重试时误用
    /// `images` 删除远端，会连带删掉被其他文章引用的共享图片。
    #[serde(default)]
    pub exclusive_images: Vec<String>,
    /// 删除前该文章在 `writing` 上 Markdown 的内容哈希（内容基线）。
    ///
    /// 重试时用于区分「上次推送失败、远端仍是原文」与「远端同路径已被他人
    /// 重建／改写」；后者必须停止并要求按新操作核对差异。
    ///
    /// 刻意记录**文件内容哈希**而非分支头：分支可能因其他文章推进而变化，
    /// 那与本篇无关，不应误判为冲突。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_writing_markdown_hash: Option<String>,
    /// 删除前该文章在 `main` 上 Markdown 的内容哈希（内容基线）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_main_markdown_hash: Option<String>,
    /// 写作分支是否已完成删除 / 撤下。
    pub writing_done: bool,
    /// `main` 分支是否已完成删除 / 撤下。
    pub main_done: bool,
    /// 删除前该文章在 `main` 上是否公开。
    pub was_published: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_writing_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_main_sha: Option<String>,
    /// 最近一次失败的原因（非敏感）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

impl TrashEntry {
    /// 两个分支是否都已完成。
    pub fn fully_done(&self) -> bool {
        self.writing_done && self.main_done
    }

    /// 面向用户的分支完成状态文案。
    pub fn branch_state_text(&self) -> String {
        match (self.writing_done, self.main_done) {
            (true, true) => "写作分支与网站均已处理".to_string(),
            (true, false) => "写作分支已处理／网站仍在".to_string(),
            (false, true) => "网站已处理／写作分支仍在".to_string(),
            (false, false) => "两个分支均未处理".to_string(),
        }
    }
}

/// 崩溃恢复副本。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryDraft {
    pub article_id: String,
    pub markdown_rel_path: String,
    /// 完整 Markdown 文本（含 front matter）。
    pub markdown: String,
    pub saved_at_unix: u64,
}

/// 本地数据存储。
pub struct LocalStore {
    root: PathBuf,
}

impl LocalStore {
    /// 打开默认应用数据目录。
    ///
    /// 优先使用 [`DATA_DIR_ENV`] 指定的目录（自动化测试与便携部署）；
    /// 否则用 Windows 每用户应用数据目录。
    pub fn open_default() -> Result<Self> {
        if let Some(custom) = std::env::var_os(DATA_DIR_ENV) {
            if !custom.is_empty() {
                return Self::open_at(PathBuf::from(custom));
            }
        }
        let base = dirs::data_local_dir()
            .or_else(dirs::data_dir)
            .or_else(|| dirs::home_dir().map(|h| h.join(".local/share")))
            .ok_or_else(|| {
                WriterError::new(ErrorCode::IoFailed, "无法定位本机应用数据目录")
            })?;
        Self::open_at(base.join(APP_DIR_NAME))
    }

    /// 在指定目录打开（测试夹具使用）。
    pub fn open_at(root: PathBuf) -> Result<Self> {
        let store = Self { root };
        fs::create_dir_all(&store.root).map_err(|e| {
            WriterError::new(ErrorCode::IoFailed, format!("无法创建应用数据目录：{e}"))
        })?;
        fs::create_dir_all(store.trash_dir()).ok();
        fs::create_dir_all(store.recovery_dir()).ok();
        fs::create_dir_all(store.preview_dir()).ok();
        // 首次打开即落盘一份默认配置：让仓库身份在磁盘上可核对，
        // 也让后续的配置迁移有明确起点。
        if !store.config_path().exists() {
            store.save_config(&AppConfig::default())?;
        }
        Ok(store)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn trash_dir(&self) -> PathBuf {
        self.root.join("trash")
    }

    pub fn recovery_dir(&self) -> PathBuf {
        self.root.join("recovery")
    }

    pub fn preview_dir(&self) -> PathBuf {
        self.root.join("preview")
    }

    fn config_path(&self) -> PathBuf {
        self.root.join("config.json")
    }

    fn versions_path(&self) -> PathBuf {
        self.root.join("versions.json")
    }

    /// 远端核对缓存文件。可随时删除：删除只失去缓存，不改变任何结论。
    fn remote_checks_path(&self) -> PathBuf {
        self.root.join("remote-checks.json")
    }

    /// 环境检查结果文件（含检查时间戳）。缺失即「尚未检查」。
    fn toolchain_path(&self) -> PathBuf {
        self.root.join("toolchain.json")
    }

    /// 读取上次的环境检查结果。
    ///
    /// 文件缺失或损坏时返回「尚未检查」——**不**退化成默认报告，
    /// 否则界面会把「没查过」显示成「环境齐备」。
    pub fn load_toolchain_state(&self) -> crate::preview::ToolchainState {
        let Ok(bytes) = fs::read(self.toolchain_path()) else {
            return crate::preview::ToolchainState::Unchecked;
        };
        serde_json::from_slice::<crate::preview::ToolchainState>(&bytes)
            .unwrap_or(crate::preview::ToolchainState::Unchecked)
    }

    /// 记录一次环境检查结果。
    pub fn save_toolchain_state(&self, state: &crate::preview::ToolchainState) -> Result<()> {
        let text = serde_json::to_string_pretty(state).map_err(|e| {
            WriterError::new(ErrorCode::IoFailed, format!("无法序列化环境检查结果：{e}"))
        })?;
        fs::write(self.toolchain_path(), text).map_err(|e| {
            WriterError::new(ErrorCode::IoFailed, format!("无法写入环境检查结果：{e}"))
        })
    }

    /// 读取远端核对缓存；文件缺失或损坏时返回空缓存。
    pub fn load_remote_checks(&self) -> RemoteCheckCache {
        RemoteCheckCache::load_or_default(&self.remote_checks_path())
    }

    /// 写入远端核对缓存。
    pub fn save_remote_checks(&self, cache: &RemoteCheckCache) -> Result<()> {
        let text = serde_json::to_string_pretty(cache).map_err(|e| {
            WriterError::new(ErrorCode::IoFailed, format!("无法序列化远端核对缓存：{e}"))
        })?;
        fs::write(self.remote_checks_path(), text).map_err(|e| {
            WriterError::new(ErrorCode::IoFailed, format!("无法写入远端核对缓存：{e}"))
        })
    }

    fn trash_index_path(&self) -> PathBuf {
        self.root.join("trash.json")
    }

    /// 读取设置；文件缺失或损坏时返回默认值并保留原文件（改写为 `.bak`）。
    pub fn load_config(&self) -> AppConfig {
        match fs::read_to_string(self.config_path()) {
            Ok(text) => match serde_json::from_str::<AppConfig>(&text) {
                Ok(mut cfg) => {
                    cfg.clamp();
                    cfg
                }
                Err(_) => {
                    let _ = fs::rename(self.config_path(), self.root.join("config.json.bak"));
                    AppConfig::default()
                }
            },
            Err(_) => AppConfig::default(),
        }
    }

    /// 原子写入设置。
    pub fn save_config(&self, config: &AppConfig) -> Result<()> {
        let mut cfg = config.clone();
        cfg.clamp();
        cfg.schema_version = SCHEMA_VERSION;
        let text = serde_json::to_string_pretty(&cfg)
            .map_err(|_| WriterError::new(ErrorCode::IoFailed, "设置序列化失败"))?;
        crate::article_io::atomic_write(&self.config_path(), text.as_bytes())
    }

    pub fn load_versions(&self) -> VersionIndex {
        VersionIndex::load_or_default(&self.versions_path())
    }

    pub fn save_versions(&self, index: &VersionIndex) -> Result<()> {
        let text = serde_json::to_string_pretty(index)
            .map_err(|_| WriterError::new(ErrorCode::IoFailed, "版本索引序列化失败"))?;
        crate::article_io::atomic_write(&self.versions_path(), text.as_bytes())
    }

    /// 更新单篇文章的基线。
    pub fn update_baseline<F>(&self, article_id: &str, mutate: F) -> Result<ArticleBaseline>
    where
        F: FnOnce(&mut ArticleBaseline),
    {
        let mut index = self.load_versions();
        let entry = index.articles.entry(article_id.to_string()).or_default();
        mutate(entry);
        let updated = entry.clone();
        self.save_versions(&index)?;
        Ok(updated)
    }

    /// 读取回收区索引。
    ///
    /// 解析失败时**保留一份 `.bak`** 再返回空列表：回收区是删除操作的安全网，
    /// 若静默清空，用户界面上的可恢复条目会凭空消失（文件还在磁盘上，但索引
    /// 已被覆盖）。留档后仍可能手工取回。
    pub fn load_trash(&self) -> Vec<TrashEntry> {
        let path = self.trash_index_path();
        let Ok(bytes) = fs::read(&path) else {
            return Vec::new();
        };
        let Ok(text) = String::from_utf8(bytes) else {
            preserve_corrupt(&path);
            return Vec::new();
        };
        match serde_json::from_str::<Vec<TrashEntry>>(&text) {
            Ok(entries) => entries,
            Err(_) => {
                preserve_corrupt(&path);
                Vec::new()
            }
        }
    }

    pub fn save_trash(&self, entries: &[TrashEntry]) -> Result<()> {
        let text = serde_json::to_string_pretty(entries)
            .map_err(|_| WriterError::new(ErrorCode::IoFailed, "回收区索引序列化失败"))?;
        crate::article_io::atomic_write(&self.trash_index_path(), text.as_bytes())
    }

    /// 追加或更新一条回收记录。
    pub fn upsert_trash(&self, entry: TrashEntry) -> Result<()> {
        let mut entries = self.load_trash();
        match entries.iter_mut().find(|e| e.op_id == entry.op_id) {
            Some(slot) => *slot = entry,
            None => entries.insert(0, entry),
        }
        self.save_trash(&entries)
    }

    /// 单条回收记录的目录。
    pub fn trash_entry_dir(&self, op_id: &str) -> PathBuf {
        self.trash_dir().join(op_id)
    }

    /// 写入一份崩溃恢复副本。
    pub fn write_recovery(&self, draft: &RecoveryDraft) -> Result<()> {
        fs::create_dir_all(self.recovery_dir())
            .map_err(|e| WriterError::new(ErrorCode::IoFailed, format!("创建恢复目录失败：{e}")))?;
        let file = self.recovery_file(&draft.article_id);
        let text = serde_json::to_string_pretty(draft)
            .map_err(|_| WriterError::new(ErrorCode::IoFailed, "恢复副本序列化失败"))?;
        crate::article_io::atomic_write(&file, text.as_bytes())
    }

    /// 读取一份崩溃恢复副本。
    pub fn read_recovery(&self, article_id: &str) -> Option<RecoveryDraft> {
        let text = fs::read_to_string(self.recovery_file(article_id)).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// 列出全部崩溃恢复副本。
    pub fn list_recovery(&self) -> Vec<RecoveryDraft> {
        let mut out = Vec::new();
        let Ok(dir) = fs::read_dir(self.recovery_dir()) else {
            return out;
        };
        for entry in dir.flatten() {
            if !entry.file_name().to_string_lossy().ends_with(".json") {
                continue;
            }
            if let Ok(text) = fs::read_to_string(entry.path()) {
                if let Ok(draft) = serde_json::from_str::<RecoveryDraft>(&text) {
                    out.push(draft);
                }
            }
        }
        out.sort_by(|a, b| b.saved_at_unix.cmp(&a.saved_at_unix));
        out
    }

    /// 清除某篇文章的崩溃恢复副本（保存成功后调用）。
    pub fn clear_recovery(&self, article_id: &str) {
        let _ = fs::remove_file(self.recovery_file(article_id));
    }

    fn recovery_file(&self, article_id: &str) -> PathBuf {
        // 用哈希命名，避免中文或其他字符在文件系统上的兼容问题。
        let key = util::hash_bytes(article_id.as_bytes());
        self.recovery_dir().join(format!("{}-{}.json", sanitize_key(article_id), &key[..8]))
    }

    /// 取得排他实例锁。第二个实例会得到明确的“已有窗口”错误。
    pub fn acquire_instance_lock(&self) -> Result<InstanceLock> {
        InstanceLock::acquire(&self.root.join("instance.lock"))
    }
}

/// 把任意字符串压成可安全用于文件名的短键。
fn sanitize_key(value: &str) -> String {
    let cleaned: String = value
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' })
        .take(32)
        .collect();
    if cleaned.is_empty() {
        "article".to_string()
    } else {
        cleaned
    }
}

/// 进程生命周期内持有的工作区排他锁。
pub struct InstanceLock {
    _file: fs::File,
    path: PathBuf,
}

impl InstanceLock {
    fn acquire(path: &Path) -> Result<Self> {
        use std::fs::OpenOptions;
        let mut options = OpenOptions::new();
        options.write(true).create(true).truncate(false);

        #[cfg(windows)]
        {
            // 共享模式 0：第二个进程打开同一文件会失败，从而天然排斥双实例。
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(0);
        }

        match options.open(path) {
            Ok(mut file) => {
                use std::io::Write;
                let _ = file.set_len(0);
                let _ = write!(file, "pid={}", std::process::id());
                let _ = file.flush();
                Ok(Self { _file: file, path: path.to_path_buf() })
            }
            Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => Err(WriterError::new(
                ErrorCode::InvalidArgument,
                "工作区已被另一个软件窗口占用。请切换到已打开的窗口，或先关闭它再重试",
            )),
            Err(err) => Err(WriterError::new(
                ErrorCode::IoFailed,
                format!("无法建立工作区锁：{err}"),
            )),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, LocalStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalStore::open_at(dir.path().to_path_buf()).unwrap();
        (dir, store)
    }

    #[test]
    fn opening_a_fresh_store_persists_default_config() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("appdata");
        let store = LocalStore::open_at(root.clone()).unwrap();

        // 首次打开即落盘默认配置，便于在磁盘上核对仓库身份。
        let path = root.join("config.json");
        assert!(path.exists(), "首次打开应写出默认配置");
        let parsed: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(parsed["repoLabel"], DEFAULT_REPO_LABEL);
        assert_eq!(parsed["connected"], false);
        assert_eq!(parsed["schemaVersion"], SCHEMA_VERSION);
        // 不落盘任何凭据字段。
        assert!(parsed.get("token").is_none());
        assert!(parsed.get("pat").is_none());

        // 已存在的配置不会被覆盖。
        let mut cfg = store.load_config();
        cfg.connected = true;
        store.save_config(&cfg).unwrap();
        let reopened = LocalStore::open_at(root).unwrap();
        assert!(reopened.load_config().connected, "重复打开不得重置已有配置");
    }

    #[test]
    fn data_dir_env_override_selects_custom_root() {
        let dir = tempfile::tempdir().unwrap();
        let custom = dir.path().join("custom-data");
        // 保存并恢复环境变量，避免影响其它测试。
        let previous = std::env::var_os(DATA_DIR_ENV);
        std::env::set_var(DATA_DIR_ENV, &custom);
        let store = LocalStore::open_default().unwrap();
        let root = store.root().to_path_buf();
        match previous {
            Some(value) => std::env::set_var(DATA_DIR_ENV, value),
            None => std::env::remove_var(DATA_DIR_ENV),
        }
        assert_eq!(root, custom);
        assert!(root.join("config.json").exists());
    }

    #[test]
    fn config_round_trips_with_clamped_preferences() {
        let (_dir, store) = store();
        let mut cfg = store.load_config();
        assert!(!cfg.connected);
        cfg.connected = true;
        cfg.preferences.font_size = 999; // 会被收敛。
        store.save_config(&cfg).unwrap();

        let loaded = store.load_config();
        assert!(loaded.connected);
        assert!(loaded.preferences.font_size <= 28);
        assert_eq!(loaded.schema_version, SCHEMA_VERSION);
    }

    #[test]
    fn corrupt_config_is_preserved_as_backup() {
        let (dir, store) = store();
        fs::write(dir.path().join("config.json"), "{ not json").unwrap();
        let cfg = store.load_config();
        assert!(!cfg.connected);
        assert!(dir.path().join("config.json.bak").exists(), "损坏的配置应保留为备份");
    }

    #[test]
    fn baselines_persist_per_article() {
        let (_dir, store) = store();
        store
            .update_baseline("read-code", |b| {
                b.writing_hash = Some("abc".to_string());
                b.main_published = Some(false);
            })
            .unwrap();
        let index = store.load_versions();
        let entry = index.get("read-code").unwrap();
        assert_eq!(entry.writing_hash.as_deref(), Some("abc"));
        assert_eq!(entry.main_published, Some(false));
        assert!(index.get("other").is_none());
    }

    #[test]
    fn trash_entries_round_trip_and_dedupe_by_op_id() {
        let (_dir, store) = store();
        let entry = TrashEntry {
            op_id: "op-1".to_string(),
            kind: TrashKind::Deleted,
            article_id: "a".to_string(),
            title: "标题".to_string(),
            deleted_at: "2026-09-27".to_string(),
            markdown_rel_path: "src/content/blog/a.md".to_string(),
            has_markdown: true,
            markdown_backup_name: Some("a.md".to_string()),
            images: vec![],
            exclusive_images: vec![],
            source_writing_markdown_hash: None,
            source_main_markdown_hash: None,
            writing_done: true,
            main_done: false,
            was_published: true,
            source_writing_sha: None,
            source_main_sha: None,
            last_error: None,
        };
        store.upsert_trash(entry.clone()).unwrap();
        assert_eq!(store.load_trash().len(), 1);
        assert_eq!(store.load_trash()[0].branch_state_text(), "写作分支已处理／网站仍在");

        let mut updated = entry;
        updated.main_done = true;
        store.upsert_trash(updated).unwrap();
        let entries = store.load_trash();
        assert_eq!(entries.len(), 1, "同一操作 ID 应更新而非新增");
        assert!(entries[0].fully_done());
    }

    #[test]
    fn recovery_drafts_round_trip_and_clear() {
        let (_dir, store) = store();
        let draft = RecoveryDraft {
            article_id: "中文/文章".to_string(),
            markdown_rel_path: "src/content/blog/中文/文章.md".to_string(),
            markdown: "---\ntitle: \"x\"\n---\n正文".to_string(),
            saved_at_unix: 1,
        };
        store.write_recovery(&draft).unwrap();
        assert_eq!(store.read_recovery("中文/文章").unwrap().markdown, draft.markdown);
        assert_eq!(store.list_recovery().len(), 1);
        store.clear_recovery("中文/文章");
        assert!(store.read_recovery("中文/文章").is_none());
    }

    #[test]
    fn instance_lock_excludes_second_holder() {
        let (_dir, store) = store();
        let first = store.acquire_instance_lock().unwrap();
        // 同一进程内再次打开同一路径会因共享模式 0 失败。
        let second = store.acquire_instance_lock();
        #[cfg(windows)]
        assert!(second.is_err(), "Windows 上第二个实例必须被拒绝");
        #[cfg(not(windows))]
        let _ = second;
        drop(first);
        assert!(store.acquire_instance_lock().is_ok(), "释放后应可再次获取");
    }

    /// 版本索引损坏时：不静默丢弃，先留一份 `.bak`，再以空索引继续。
    ///
    /// 回归的是真实缺陷：旧实现解析失败即返回空集合且不提示，随后任何一次
    /// `update_baseline` 都会把空集合写回，用户的版本基线永久丢失。
    #[test]
    fn corrupt_version_index_is_preserved_as_bak_then_reset() {
        let (_dir, store) = store();
        // 写入一份有效基线，再故意破坏文件内容。
        store
            .update_baseline("keep-a", |b| {
                b.writing_commit = Some("deadbeef".to_string());
                b.main_commit = Some("cafe".to_string());
            })
            .unwrap();
        assert_eq!(store.load_versions().get("keep-a").unwrap().writing_commit.as_deref(), Some("deadbeef"));

        let path = store.root().join("versions.json");
        std::fs::write(&path, b"{ this is not json").unwrap();

        // 读取：返回空索引（程序可用），但原文件必须被留档。
        assert!(store.load_versions().articles.is_empty());
        let bak = store.root().join("versions.json.bak");
        assert!(bak.exists(), "损坏的版本索引必须保留 .bak 备份");
        assert_eq!(std::fs::read_to_string(&bak).unwrap(), "{ this is not json");
        assert!(!path.exists(), "损坏的原文件应已被移走留档");

        // 后续保存不再覆盖可恢复内容：.bak 仍是原始损坏文本（可手工抢救）。
        store.update_baseline("new-a", |b| b.writing_commit = Some("n".to_string())).unwrap();
        assert_eq!(std::fs::read_to_string(&bak).unwrap(), "{ this is not json");
        assert!(store.load_versions().get("new-a").is_some());
    }

    /// 回收索引损坏时同样保留 `.bak`，避免「可恢复条目凭空消失」。
    #[test]
    fn corrupt_trash_index_is_preserved_as_bak() {
        let (_dir, store) = store();
        let entry = TrashEntry {
            op_id: "op-1758970000-a1b2c3d4e5f6".to_string(),
            kind: TrashKind::Deleted,
            article_id: "gone".to_string(),
            title: "被删文章".to_string(),
            deleted_at: "2026-09-27".to_string(),
            markdown_rel_path: "src/content/blog/gone.md".to_string(),
            has_markdown: true,
            markdown_backup_name: Some("gone.md".to_string()),
            images: vec![],
            exclusive_images: vec![],
            source_writing_markdown_hash: None,
            source_main_markdown_hash: None,
            writing_done: true,
            main_done: true,
            was_published: false,
            source_writing_sha: None,
            source_main_sha: None,
            last_error: None,
        };
        store.upsert_trash(entry.clone()).unwrap();
        assert_eq!(store.load_trash().len(), 1);

        let path = store.root().join("trash.json");
        std::fs::write(&path, b"[ { broken").unwrap();

        assert!(store.load_trash().is_empty(), "损坏时以空列表继续，程序仍可用");
        let bak = store.root().join("trash.json.bak");
        assert!(bak.exists(), "损坏的回收索引必须保留 .bak 备份");
        assert_eq!(std::fs::read_to_string(&bak).unwrap(), "[ { broken");
    }

    /// 读取失败（文件不存在）不应产生 `.bak`：无法区分「损坏」与「暂时读不到」。
    #[test]
    fn missing_index_does_not_create_bak() {
        let (_dir, store) = store();
        assert!(store.load_versions().articles.is_empty());
        assert!(store.load_trash().is_empty());
        assert!(!store.root().join("versions.json.bak").exists());
        assert!(!store.root().join("trash.json.bak").exists());
    }
}
