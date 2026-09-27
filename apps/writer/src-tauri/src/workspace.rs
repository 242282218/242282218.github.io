//! 独立 Git 工作目录中的文章本地读写与状态推导。
//!
//! 这一层只做本地文件操作：扫描、读取、新建、保存、导入、URL 改名。
//! 任何远端 Git 操作都留在 [`crate::sync`] 与 [`crate::publish`]。
//!
//! `main` 与 `writing` 的内容通过 [`RemoteSnapshot`] 由上层注入，
//! 使状态推导保持为纯函数，便于单元测试。

use crate::article_io::{self, ParsedMarkdown};
use crate::images;
use crate::local_store::{ArticleBaseline, LocalStore};
use crate::model::{
    ArticleContent, ArticleMeta, ArticleSource, ArticleStatus, ArticleSummary, ErrorCode,
    RemoteSync, Result, SiteState, WriterError,
};
use crate::paths;
use crate::util;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// 上层注入的远端版本信息（每个字段都是文章文件完整内容的哈希）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RemoteSnapshot {
    /// `writing` 上该文件的原始内容哈希（含 `draft` 值）。
    pub writing_hash: Option<String>,
    /// `main` 上该文件的原始内容哈希；`None` 表示该文章**不在** `main` 上。
    pub main_hash: Option<String>,
    /// `main` 上该文件按「网站发布版」规范化后的哈希（`draft: false`）。
    pub main_site_hash: Option<String>,
    /// 该文章当前 `main` 版本所在的提交（即读取内容时用的 `main` 头）。
    pub main_commit: Option<String>,
    /// `main` 上该文章是否公开（`draft: false`）。
    pub main_published: Option<bool>,
    /// 最近一次**已确认部署成功**的提交（来自 Pages 工作流，且为 `main` 上的提交）。
    pub deployed_commit: Option<String>,
    /// 最近一次**已确认部署失败**的提交。
    pub deploy_failed_commit: Option<String>,
    /// 最近一次查询到**部署进行中**的提交。
    pub deploying_commit: Option<String>,
    pub deployment_url: Option<String>,
}

/// 状态推导的完整输入。
#[derive(Debug, Clone)]
pub struct StatusInputs {
    /// 本地文件原始内容哈希（含 `draft` 值），用于同步/冲突判断与展示。
    pub local_hash: String,
    /// 本地内容按「网站发布版」规范化后的哈希，用于网站版本比较。
    pub local_site_hash: String,
    /// 本地磁盘内容是否已写入（编辑中的内存内容为 `false`）。
    pub locally_saved: bool,
    pub remote: RemoteSnapshot,
}

/// 由本地哈希与远端快照推导文章状态。
///
/// 这里刻意不使用单一 `published: boolean`：本地、`writing`、`main` 三处
/// 各自独立表达，且「已提交发布」与「网站已上线」分开。
///
/// 网站版本比较统一使用**忽略 `draft` 字段**的规范化哈希，否则刚发布的文章
/// （本地 `draft: true`、`main` `draft: false`）会被误判为「网站仍是旧版」。
///
/// 「是否发布过」以**文章是否存在于 `main`**（`main_hash`）为准，而不是以
/// `main_commit` 为准——后者是仓库级 `main` 头，任何文章都有值，用它会让
/// 从未发布的文章被显示成「已提交发布」。
///
/// 「网站已上线」只认**已确认的部署结论**（`deployed_commit` 与当前 `main`
/// 头一致）；仅推送成功而无部署确认时是「已提交发布」，绝不靠推送猜测上线。
pub fn derive_status(inputs: &StatusInputs) -> ArticleStatus {
    let remote_sync = match &inputs.remote.writing_hash {
        None => RemoteSync::LocalOnly,
        Some(hash) if hash == &inputs.local_hash => RemoteSync::Saved,
        // 远端保存过该文章，但当前本地内容尚未同步。
        Some(_) => RemoteSync::LocalOnly,
    };

    let site = match &inputs.remote.main_hash {
        // 文章从未出现在 `main` 上。
        None => SiteState::NeverPublished,
        Some(_) => {
            let head = inputs.remote.main_commit.as_deref();
            let concluded = |recorded: Option<&str>| matches!((recorded, head), (Some(a), Some(b)) if a == b);
            if inputs.remote.main_published == Some(false) {
                SiteState::Withdrawn
            } else if concluded(inputs.remote.deploy_failed_commit.as_deref()) {
                SiteState::DeployFailed
            } else if concluded(inputs.remote.deploying_commit.as_deref()) {
                SiteState::Deploying
            } else if concluded(inputs.remote.deployed_commit.as_deref()) {
                let content_current =
                    inputs.remote.main_site_hash.as_deref() == Some(inputs.local_site_hash.as_str());
                if content_current {
                    SiteState::LiveCurrentVersion
                } else {
                    SiteState::LiveOldVersion
                }
            } else {
                // 已推送到 `main`，但尚无已确认的部署结果 → 部署状态待确认。
                SiteState::PublicationSubmitted
            }
        }
    };

    ArticleStatus {
        locally_saved: inputs.locally_saved,
        remote_sync,
        site,
        local_body_hash: inputs.local_hash.clone(),
        writing_body_hash: inputs.remote.writing_hash.clone(),
        main_body_hash: inputs.remote.main_hash.clone(),
        main_commit: inputs.remote.main_commit.clone(),
        deployment_url: inputs.remote.deployment_url.clone(),
    }
}

/// 独立工作目录。
#[derive(Debug, Clone)]
pub struct Workspace {
    root: PathBuf,
}

impl Workspace {
    /// 打开一个已存在的目录作为工作区。
    pub fn open(root: PathBuf) -> Result<Self> {
        if !root.is_dir() {
            return Err(WriterError::new(
                ErrorCode::IoFailed,
                "工作目录不存在，请先完成首次连接",
            ));
        }
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn blog_dir(&self) -> PathBuf {
        self.root.join(paths::BLOG_DIR_PREFIX.trim_end_matches('/'))
    }

    pub fn images_root(&self) -> PathBuf {
        self.root.join(paths::IMAGE_DIR_PREFIX.trim_end_matches('/'))
    }

    /// 文章 Markdown 的绝对路径。
    ///
    /// 字符串校验之后必须再复核没有链接逃逸：受管目录里的目录联接/符号链接会
    /// 让后续读写跟随到仓库之外（可能覆盖或删除任意文件）。所有落盘路径都经
    /// 由本函数，因此这里是一处关键的收口点。
    pub fn markdown_abs_path(&self, article_id: &str) -> Result<PathBuf> {
        let rel = format!("{}{}.md", paths::BLOG_DIR_PREFIX, article_id);
        paths::validate_managed_markdown(&rel)?;
        paths::verify_no_link_escape(&self.root, &rel)?;
        Ok(self.root.join(rel))
    }

    /// 递归收集受管 Markdown 文件相对路径。
    pub fn list_markdown_rel_paths(&self) -> Result<Vec<String>> {
        let mut out = Vec::new();
        let base = self.blog_dir();
        if !base.is_dir() {
            return Ok(out);
        }
        collect_markdown(&base, &base, &mut out)?;
        out.sort();
        Ok(out)
    }

    /// 判断文章是否已存在（含大小写冲突检测）。
    ///
    /// Windows 文件系统不区分大小写；`collision` 返回实际冲突的既有 ID。
    ///
    /// 目录中其它**文件名不合法**的文章不会让检查失败；但仍会参与大小写冲突
    /// 比较（用相对路径推导的原始标识），避免新建出会撞车的路径。
    pub fn check_id_available(&self, article_id: &str) -> Result<Option<String>> {
        paths::validate_new_article_id(article_id)?;
        let key = paths::case_insensitive_key(article_id);
        for rel in self.list_markdown_rel_paths()? {
            let existing = match paths::article_id_from_rel_path(&rel) {
                Ok(id) => id,
                // 不合法的名字无法推导标准 ID，但不该阻断新建；跳过它即可。
                Err(_) => continue,
            };
            if paths::case_insensitive_key(&existing) == key {
                return Ok(Some(existing));
            }
        }
        Ok(None)
    }

    /// 扫描全部文章，返回列表项。解析失败的文章仍然出现，并带上错误信息。
    ///
    /// 单个文件名不合法（超长、Windows 保留名等）时**不**让整个列表失败：
    /// 该条目照样以错误形式列出，其余文章不受影响。否则一个坏名字会让软件
    /// 无法列出任何文章、也无法新建。
    pub fn scan(
        &self,
        store: &LocalStore,
        snapshots: &BTreeMap<String, RemoteSnapshot>,
    ) -> Result<Vec<ArticleSummary>> {
        let versions = store.load_versions();
        let mut out = Vec::new();
        for rel in self.list_markdown_rel_paths()? {
            let summary = match paths::article_id_from_rel_path(&rel) {
                Ok(article_id) => {
                    let abs = self.root.join(&rel);
                    self.summarize(&article_id, &rel, &abs, &versions, snapshots)
                }
                Err(err) => self.summarize_invalid_name(&rel, err),
            };
            out.push(summary);
        }
        Ok(out)
    }

    /// 为「文件名本身不合法」的文章构造列表项：保留路径作为标识与标题，
    /// 并带上错误说明，绝不静默跳过。
    ///
    /// 不查版本基线：合规 ID 都推导不出来，基线自然无从关联。
    fn summarize_invalid_name(&self, rel_path: &str, err: WriterError) -> ArticleSummary {
        let abs = self.root.join(rel_path);
        let content_hash =
            util::hash_file(&abs).unwrap_or_else(|_| util::hash_bytes(rel_path.as_bytes()));
        ArticleSummary {
            id: rel_path.to_string(),
            title: rel_path.to_string(),
            description: String::new(),
            tags: Vec::new(),
            pub_date: String::new(),
            updated_date: None,
            draft: true,
            image_count: 0,
            source: ArticleSource::Workspace,
            last_edited_unix: None,
            status: derive_status(&StatusInputs {
                local_hash: content_hash.clone(),
                local_site_hash: content_hash,
                locally_saved: true,
                remote: RemoteSnapshot::default(),
            }),
            load_error: Some(err),
        }
    }

    fn summarize(
        &self,
        article_id: &str,
        rel_path: &str,
        abs: &Path,
        versions: &crate::local_store::VersionIndex,
        snapshots: &BTreeMap<String, RemoteSnapshot>,
    ) -> ArticleSummary {
        let source = ArticleSource::Workspace;
        let image_count = images::article_images(&self.root, article_id)
            .map(|list| list.len())
            .unwrap_or(0);
        let remote = snapshots.get(article_id).cloned().unwrap_or_default();
        let last_edited_unix = versions.get(article_id).and_then(|b| b.last_edited_unix);

        match read_parsed(abs) {
            Ok((_parsed, meta, raw)) => {
                // 哈希与网站哈希都建在**磁盘原文**上，与同步基线同一基准。
                let content_hash = raw_content_hash(abs)
                    .unwrap_or_else(|_| util::hash_bytes(raw.as_bytes()));
                let status = derive_status(&StatusInputs {
                    local_hash: content_hash.clone(),
                    local_site_hash: site_hash_of(&raw),
                    locally_saved: true,
                    remote,
                });
                ArticleSummary {
                    id: article_id.to_string(),
                    title: meta.title,
                    description: meta.description,
                    tags: meta.tags,
                    pub_date: meta.pub_date,
                    updated_date: meta.updated_date,
                    draft: meta.draft,
                    image_count,
                    source,
                    last_edited_unix,
                    status,
                    load_error: None,
                }
            }
            Err(err) => {
                // 读取或解析失败时，用文件路径占位并暴露错误，绝不静默跳过。
                let content_hash =
                    util::hash_file(abs).unwrap_or_else(|_| util::hash_bytes(rel_path.as_bytes()));
                ArticleSummary {
                    id: article_id.to_string(),
                    title: rel_path.to_string(),
                    description: String::new(),
                    tags: Vec::new(),
                    pub_date: String::new(),
                    updated_date: None,
                    draft: true,
                    image_count,
                    source,
                    last_edited_unix,
                    status: derive_status(&StatusInputs {
                        local_hash: content_hash.clone(),
                        local_site_hash: content_hash,
                        locally_saved: true,
                        remote,
                    }),
                    load_error: Some(err),
                }
            }
        }
    }

    /// 读取单篇文章的完整内容。
    pub fn read(
        &self,
        article_id: &str,
        snapshots: &BTreeMap<String, RemoteSnapshot>,
    ) -> Result<ArticleContent> {
        let rel = format!("{}{}.md", paths::BLOG_DIR_PREFIX, article_id);
        paths::validate_managed_markdown(&rel)?;
        paths::verify_no_link_escape(&self.root, &rel)?;
        let abs = self.root.join(&rel);
        let (parsed, meta, raw) = read_parsed(&abs)?;
        // 与同步基线同一基准：用磁盘原文算哈希与网站哈希。
        let content_hash = raw_content_hash(&abs)?;
        let remote = snapshots.get(article_id).cloned().unwrap_or_default();
        let status = derive_status(&StatusInputs {
            local_hash: content_hash.clone(),
            local_site_hash: site_hash_of(&raw),
            locally_saved: true,
            remote,
        });
        Ok(ArticleContent {
            id: article_id.to_string(),
            meta,
            raw_front_matter: parsed.front_matter,
            body: parsed.body,
            content_hash,
            status,
        })
    }

    /// 新建文章。ID 必须通过新文章校验且不冲突；默认 `draft: true`。
    pub fn create(&self, article_id: &str, meta: &ArticleMeta, body: &str) -> Result<ArticleContent> {
        paths::validate_new_article_id(article_id)?;
        if let Some(existing) = self.check_id_available(article_id)? {
            return Err(WriterError::new(
                ErrorCode::ArticleExists,
                format!("已存在相同标识的文章：{existing}"),
            )
            .with_detail(existing));
        }
        // 不支持嵌套在图片目录中的 ID（图片归属按 ID 一级目录判定）。
        let abs = self.markdown_abs_path(article_id)?;
        if let Some(parent) = abs.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                WriterError::new(ErrorCode::IoFailed, format!("创建文章目录失败：{e}"))
            })?;
        }
        let text = article_io::compose_markdown(meta, body);
        article_io::atomic_write(&abs, text.as_bytes())?;
        self.read(article_id, &BTreeMap::new())
    }

    /// 保存文章：受控更新 front matter 字段并原子替换文件。
    ///
    /// `updated_date_write`：
    /// - `None` 表示本次不改动 `updatedDate`（自动保存的默认行为）；
    /// - `Some(None)` 删除该字段；
    /// - `Some(Some(date))` 写入指定日期。
    pub fn save(
        &self,
        article_id: &str,
        meta: &ArticleMeta,
        body: &str,
        updated_date_write: Option<Option<&str>>,
    ) -> Result<ArticleContent> {
        let abs = self.markdown_abs_path(article_id)?;
        if !abs.is_file() {
            return Err(WriterError::new(ErrorCode::ArticleNotFound, "文章文件不存在，无法保存"));
        }
        // 先按当前磁盘内容重新解析，保证未知字段与顺序来自真实文件。
        let existing_text = std::fs::read_to_string(&abs).map_err(|e| {
            WriterError::new(ErrorCode::IoFailed, format!("读取文章失败，未保存：{e}"))
        })?;
        let existing = article_io::parse_markdown(&existing_text)?;
        let updated_front_matter =
            article_io::apply_meta(&existing.front_matter, &existing.fm_newline, meta, updated_date_write);
        let rendered = existing.render_with(&updated_front_matter, body);

        // 先校验**将要写入的文本**，通过后才落盘。顺序不能反：若先写盘再解析，
        // 一旦渲染出的 front matter 不可解析，磁盘上留下的就是被改坏的文件。
        let verify = article_io::parse_markdown(&rendered)?;
        let map = article_io::parse_front_matter_map(&verify.front_matter)?;
        let verified_meta = article_io::meta_from_map(&map)?;

        article_io::atomic_write(&abs, rendered.as_bytes())?;

        Ok(ArticleContent {
            id: article_id.to_string(),
            meta: verified_meta,
            raw_front_matter: verify.front_matter,
            body: verify.body,
            content_hash: util::hash_bytes(rendered.as_bytes()),
            status: derive_status(&StatusInputs {
                local_hash: util::hash_bytes(rendered.as_bytes()),
                local_site_hash: site_hash_of(&rendered),
                locally_saved: true,
                remote: RemoteSnapshot::default(),
            }),
        })
    }

    /// 收集全部受管 Markdown 的（文章 ID，原文）对，用于图片引用检查。
    pub fn markdown_texts(&self) -> Result<Vec<(String, String)>> {
        let mut out = Vec::new();
        for rel in self.list_markdown_rel_paths()? {
            let id = paths::article_id_from_rel_path(&rel)?;
            // 复核链接逃逸：不跟随受管目录里指向外部的联接。
            if paths::verify_no_link_escape(&self.root, &rel).is_err() {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(self.root.join(&rel)) {
                out.push((id, text));
            }
        }
        Ok(out)
    }

    /// 显式导入一份已有的本地 Markdown 文件到工作区。
    ///
    /// 只复制到目标文章路径，**不**读取或改动源文件的 Git 状态，
    /// 也不触碰源目录中的其他文件。
    pub fn import_markdown_file(
        &self,
        source: &Path,
        article_id: &str,
    ) -> Result<ArticleContent> {
        paths::validate_new_article_id(article_id)?;
        let text = std::fs::read_to_string(source).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                WriterError::new(ErrorCode::ArticleNotFound, "要导入的文件不存在")
            } else {
                WriterError::new(ErrorCode::IoFailed, format!("读取要导入的文件失败：{e}"))
            }
        })?;
        // 导入前必须能解析，避免把无法安全编辑的文件放进工作区。
        let parsed = article_io::parse_markdown(&text)?;
        let map = article_io::parse_front_matter_map(&parsed.front_matter)?;
        let _ = article_io::meta_from_map(&map)?;
        if let Some(existing) = self.check_id_available(article_id)? {
            return Err(WriterError::new(
                ErrorCode::ArticleExists,
                format!("工作区中已有标识为 {existing} 的文章，请换一个标识"),
            )
            .with_detail(existing));
        }
        let abs = self.markdown_abs_path(article_id)?;
        if let Some(parent) = abs.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                WriterError::new(ErrorCode::IoFailed, format!("创建文章目录失败：{e}"))
            })?;
        }
        article_io::atomic_write(&abs, text.as_bytes())?;
        self.read(article_id, &BTreeMap::new())
    }

    /// 删除文章 Markdown 文件（仅本地；远端操作由上层编排）。
    pub fn remove_markdown(&self, article_id: &str) -> Result<()> {
        let abs = self.markdown_abs_path(article_id)?;
        match std::fs::remove_file(&abs) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(WriterError::new(ErrorCode::IoFailed, format!("删除文章失败：{e}"))),
        }
    }

    /// 读取文章文件的原始字节（用于备份，即使解析失败也能备份原文）。
    pub fn read_raw(&self, article_id: &str) -> Result<Option<Vec<u8>>> {
        let abs = self.markdown_abs_path(article_id)?;
        match std::fs::read(&abs) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(WriterError::new(ErrorCode::IoFailed, format!("读取文章原文失败：{e}"))),
        }
    }

    /// 直接写入文章原始字节（恢复备份时使用）。
    pub fn write_raw(&self, article_id: &str, bytes: &[u8]) -> Result<()> {
        let abs = self.markdown_abs_path(article_id)?;
        article_io::atomic_write(&abs, bytes)
    }

    /// URL 标识改名：把文章 Markdown 与专属图片目录整体迁移到新 ID。
    ///
    /// 只做本地文件迁移；调用方负责检查冲突、改写引用并处理远端。
    pub fn rename_article_id(&self, old_id: &str, new_id: &str) -> Result<()> {
        paths::validate_existing_article_id(old_id)?;
        paths::validate_new_article_id(new_id)?;
        if let Some(existing) = self.check_id_available(new_id)? {
            return Err(WriterError::new(
                ErrorCode::ArticleExists,
                format!("目标标识已被占用：{existing}"),
            )
            .with_detail(existing));
        }
        let old_abs = self.markdown_abs_path(old_id)?;
        if !old_abs.is_file() {
            return Err(WriterError::new(ErrorCode::ArticleNotFound, "原文章文件不存在"));
        }
        let new_abs = self.markdown_abs_path(new_id)?;
        if let Some(parent) = new_abs.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                WriterError::new(ErrorCode::IoFailed, format!("创建目标目录失败：{e}"))
            })?;
        }
        std::fs::rename(&old_abs, &new_abs).map_err(|e| {
            WriterError::new(ErrorCode::IoFailed, format!("重命名文章文件失败：{e}"))
        })?;

        // 迁移专属图片目录（若存在）。
        let old_img = self.images_root().join(old_id);
        let new_img = self.images_root().join(new_id);
        if old_img.is_dir() {
            if let Some(parent) = new_img.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            std::fs::rename(&old_img, &new_img).map_err(|e| {
                WriterError::new(ErrorCode::IoFailed, format!("迁移图片目录失败：{e}"))
            })?;
        }

        // 改写正文中指向旧图片目录的引用。
        let text = std::fs::read_to_string(&new_abs)
            .map_err(|e| WriterError::new(ErrorCode::IoFailed, format!("读取新文章失败：{e}")))?;
        let old_url_prefix = format!("/blog/{old_id}/");
        let new_url_prefix = format!("/blog/{new_id}/");
        if text.contains(&old_url_prefix) {
            let rewritten = images::rewrite_reference(&text, &old_url_prefix, &new_url_prefix);
            article_io::atomic_write(&new_abs, rewritten.as_bytes())?;
        }
        Ok(())
    }

    /// 备份文章本体与全部专属图片到回收条目目录。
    pub fn backup_to_trash(&self, entry_dir: &Path, article_id: &str) -> Result<BackupReport> {
        std::fs::create_dir_all(entry_dir).map_err(|e| {
            WriterError::new(ErrorCode::IoFailed, format!("创建回收目录失败：{e}"))
        })?;
        let mut report =
            BackupReport { has_markdown: false, markdown_backup_name: String::new(), images: Vec::new() };
        let md_name = format!("{}.md", paths::sanitize_image_basename(article_id).unwrap_or_else(|| "article".into()));
        if let Some(bytes) = self.read_raw(article_id)? {
            article_io::atomic_write(&entry_dir.join(&md_name), &bytes)?;
            report.has_markdown = true;
            report.markdown_backup_name = md_name;
        }
        for image in images::article_images(&self.root, article_id)? {
            let file_name = Path::new(&image.rel_path)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("image");
            let dest = entry_dir.join(file_name);
            let src = self.root.join(&image.rel_path);
            let bytes = std::fs::read(&src).map_err(|e| {
                WriterError::new(ErrorCode::IoFailed, format!("备份图片失败：{e}"))
            })?;
            article_io::atomic_write(&dest, &bytes)?;
            report.images.push(crate::local_store::TrashImage {
                rel_path: image.rel_path.clone(),
                backup_name: file_name.to_string(),
                size: image.size,
                content_hash: image.content_hash.clone(),
            });
        }
        Ok(report)
    }
}

/// 备份结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupReport {
    pub has_markdown: bool,
    pub markdown_backup_name: String,
    pub images: Vec<crate::local_store::TrashImage>,
}

/// 读取并解析一篇文章。第三个返回值是**磁盘原文**。
///
/// 原文必须一并返回：状态哈希与网站哈希都要建在**磁盘字节**上，而不是
/// `render()` 的规范化重建结果——`render()` 会吸收围栏尾随空格之类的差异，
/// 与同步基线（`hash_file`）的基准不一致时，已同步的文章会恒显示「未同步」。
fn read_parsed(abs: &Path) -> Result<(ParsedMarkdown, ArticleMeta, String)> {
    let text = std::fs::read_to_string(abs).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            WriterError::new(ErrorCode::ArticleNotFound, "文章文件不存在")
        } else {
            WriterError::new(ErrorCode::IoFailed, format!("读取文章失败：{e}"))
        }
    })?;
    let parsed = article_io::parse_markdown(&text)?;
    let map = article_io::parse_front_matter_map(&parsed.front_matter)?;
    let meta = article_io::meta_from_map(&map)?;
    Ok((parsed, meta, text))
}

/// 文章本地内容哈希：统一取**磁盘原文**的哈希。
///
/// 与 `sync::assess` 和 `sync::record_sync_success` 的基准保持一致（它们都用
/// 文件原始字节），否则 `derive_status` 的 `local_hash == writing_hash` 比较
/// 会在 `render() != 原文` 时永远不成立。
fn raw_content_hash(abs: &Path) -> Result<String> {
    util::hash_file(abs)
}

fn collect_markdown(base: &Path, dir: &Path, out: &mut Vec<String>) -> Result<()> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| WriterError::new(ErrorCode::IoFailed, format!("读取文章目录失败：{e}")))?;
    for entry in entries.flatten() {
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        // 不跟随符号链接，避免跳出受管目录。
        if file_type.is_symlink() {
            continue;
        }
        // Windows 目录联接在 `file_type()` 里表现为普通目录，必须另查 metadata
        // 的 reparse 位，否则会沿联接枚举到仓库之外的文件。
        if let Ok(meta) = std::fs::symlink_metadata(&path) {
            if crate::util::is_link_like(&meta) {
                continue;
            }
        } else {
            continue;
        }
        if file_type.is_dir() {
            collect_markdown(base, &path, out)?;
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.ends_with(".md") || name.starts_with('.') {
            continue;
        }
        let Ok(rel) = path.strip_prefix(base) else {
            continue;
        };
        let rel = rel.to_string_lossy().replace('\\', "/");
        out.push(format!("{}{}", paths::BLOG_DIR_PREFIX, rel));
    }
    Ok(())
}

/// 由基线记录最近编辑时间。
pub fn touch_edited(store: &LocalStore, article_id: &str) -> Result<ArticleBaseline> {
    store.update_baseline(article_id, |baseline| {
        baseline.last_edited_unix = Some(util::unix_seconds());
    })
}

/// 计算「面向网站」的规范化内容哈希。
///
/// 解析失败时回退为原始哈希：无法解析的内容本就不允许发布，这里只需保证
/// 状态推导不因此 panic。
pub fn site_hash_of(text: &str) -> String {
    match article_io::normalize_for_site(text) {
        Ok(normalized) => util::hash_bytes(normalized.as_bytes()),
        Err(_) => util::hash_bytes(text.as_bytes()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        _dir: tempfile::TempDir,
        store: LocalStore,
        workspace: Workspace,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::fs::create_dir_all(root.join("src/content/blog")).unwrap();
        std::fs::create_dir_all(root.join("public/blog")).unwrap();
        let store = LocalStore::open_at(root.join("appdata")).unwrap();
        let workspace = Workspace::open(root).unwrap();
        Fixture { _dir: dir, store, workspace }
    }

    fn meta(title: &str, draft: bool) -> ArticleMeta {
        ArticleMeta {
            title: title.to_string(),
            description: format!("{title} 的摘要"),
            pub_date: "2026-09-27".to_string(),
            updated_date: None,
            tags: vec!["学习".to_string()],
            draft,
        }
    }

    /// 创建目录联接（Windows）或目录符号链接（其它平台）。返回是否创建成功。
    ///
    /// 复用生产代码的 `cmd_path_arg`：路径里的正斜杠会被 cmd 当成开关，
    /// 必须先转成反斜杠，否则 `src/content/blog/esc` 会因 `/content` 报无效语法。
    fn make_dir_link(target: &Path, link: &Path) -> bool {
        if let Some(parent) = link.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        #[cfg(windows)]
        {
            let (Some(link_str), Some(target_str)) =
                (crate::util::cmd_path_arg(link), crate::util::cmd_path_arg(target))
            else {
                return false;
            };
            matches!(
                std::process::Command::new("cmd")
                    .args(["/c", "mklink", "/J", &link_str, &target_str])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status(),
                Ok(status) if status.success()
            )
        }
        #[cfg(not(windows))]
        {
            std::os::unix::fs::symlink(target, link).is_ok()
        }
    }

    /// P0 回归：受管目录里的目录联接不得让读写逃出仓库。
    ///
    /// 旧实现只做字符串校验，`root.join(rel)` 之后会跟随链接落到仓库之外：
    /// 实测可覆盖、删除仓库外的任意文件（不可逆）。现在所有落盘点都经
    /// `verify_no_link_escape` 复核，遇到链接类对象一律拒绝。
    #[test]
    fn junction_in_managed_dir_cannot_escape_workspace() {
        let f = fixture();
        // 仓库外的「受害者」目录与文件。
        let outside = f._dir.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        let victim = outside.join("victim.md");
        std::fs::write(&victim, "仓库外的原文件，必须保持不变\n").unwrap();

        // `src/content/blog/esc` 是指向仓库外目录的目录联接。
        let link = f.workspace.blog_dir().join("esc");
        if !make_dir_link(&outside, &link) {
            eprintln!("[跳过] 无法创建目录联接（可能缺少权限）");
            return;
        }

        // 读取：不得读到仓库外的内容。
        let read = f.workspace.read("esc/victim", &BTreeMap::new());
        assert!(
            read.is_err(),
            "经链接读取仓库外文件必须被拒绝，实际：{read:?}"
        );

        // 写入：不得覆盖仓库外的文件。
        let err = f.workspace
            .write_raw("esc/victim", "被软件覆盖了\n".as_bytes())
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::PathOutOfScope, "{err:?}");
        assert_eq!(
            std::fs::read_to_string(&victim).unwrap(),
            "仓库外的原文件，必须保持不变\n",
            "仓库外文件绝不能被覆盖"
        );

        // 删除：不得删掉仓库外的文件。
        let err = f.workspace.remove_markdown("esc/victim").unwrap_err();
        assert_eq!(err.code, ErrorCode::PathOutOfScope, "{err:?}");
        assert!(victim.exists(), "仓库外文件绝不能被删除");

        // 扫描：不得把链接后的文件当作受管文章列出来。
        let list = f.workspace.scan(&f.store, &BTreeMap::new()).unwrap();
        assert!(
            !list.iter().any(|s| s.id.contains("esc")),
            "链接背后的文件不得出现在受管列表：{list:?}"
        );
    }

    /// 图片目录是目录联接时，读、写、删都必须被拒绝。
    #[test]
    fn junction_image_dir_cannot_escape_workspace() {
        let f = fixture();
        let outside = f._dir.path().join("outside-images");
        std::fs::create_dir_all(&outside).unwrap();
        let fig = outside.join("figure-01.png");
        // 造一份合法 PNG 头，确保拒绝来自链接复核而非格式校验。
        let png = crate::testkit::tiny_png();
        std::fs::write(&fig, &png).unwrap();

        let link = f.workspace.images_root().join("gallery");
        if !make_dir_link(&outside, &link) {
            eprintln!("[跳过] 无法创建目录联接（可能缺少权限）");
            return;
        }

        // 枚举：不得列出仓库外的图片（否则会被当作「本文章的图」参与删除决策）。
        let images = crate::images::article_images(f.workspace.root(), "gallery");
        assert!(images.is_err(), "链接目录不得被当作图片目录枚举：{images:?}");

        // 删除：不得删掉仓库外的图片。
        let rel = format!("{}gallery/figure-01.png", crate::paths::IMAGE_DIR_PREFIX);
        let err = crate::images::remove_image(f.workspace.root(), &rel).unwrap_err();
        assert_eq!(err.code, ErrorCode::PathOutOfScope, "{err:?}");
        assert!(fig.exists(), "仓库外图片绝不能被删除");

        // 写入：不得把图片写到仓库外。
        let err = crate::images::import_image_bytes(f.workspace.root(), "gallery", "new", &png)
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::PathOutOfScope, "{err:?}");
    }

    #[test]
    fn create_then_read_round_trips() {
        let f = fixture();
        let created = f.workspace.create("first-post", &meta("第一篇", true), "正文\n").unwrap();
        assert_eq!(created.meta.title, "第一篇");
        assert!(created.meta.draft);
        assert_eq!(created.body, "正文\n");

        let read = f.workspace.read("first-post", &BTreeMap::new()).unwrap();
        assert_eq!(read.content_hash, created.content_hash);
    }

    #[test]
    fn duplicate_and_invalid_ids_are_rejected() {
        let f = fixture();
        f.workspace.create("first-post", &meta("A", true), "x").unwrap();

        let err = f.workspace.create("first-post", &meta("B", true), "x").unwrap_err();
        assert_eq!(err.code, ErrorCode::ArticleExists);

        // Windows 大小写不敏感：`First-Post` 与 `first-post` 视为冲突。
        let err = f.workspace.create("First-Post", &meta("C", true), "x").unwrap_err();
        assert_eq!(err.code, ErrorCode::ArticleIdInvalid, "大写不符合新文章命名规则");

        let err = f.workspace.create("../escape", &meta("D", true), "x").unwrap_err();
        assert_eq!(err.code, ErrorCode::ArticleIdInvalid);
    }

    #[test]
    fn save_updates_meta_and_keeps_body() {
        let f = fixture();
        f.workspace.create("a-post", &meta("旧标题", true), "原始正文\n").unwrap();
        let mut new_meta = meta("新标题", false);
        new_meta.tags = vec!["标签一".to_string(), "标签二".to_string()];
        let saved = f.workspace.save("a-post", &new_meta, "改过的正文\n", None).unwrap();
        assert_eq!(saved.meta.title, "新标题");
        assert!(!saved.meta.draft);
        assert_eq!(saved.meta.tags, vec!["标签一", "标签二"]);
        assert_eq!(saved.body, "改过的正文\n");
    }

    /// P0-3 回归：一次普通保存不得改变「面向网站」的规范化哈希。
    ///
    /// 旧实现无条件把 `tags: [示例]` 改写成块序列，导致已发布文章保存一次就被
    /// 判成「网站仍是旧版」；同时校验发生在写盘之后，渲染出坏 YAML 时坏文件
    /// 已经落盘。
    #[test]
    fn plain_save_preserves_site_hash_and_file_on_validation_failure() {
        let f = fixture();
        // 用站点现有文章的写法：单行 flow 序列、无引号标量。
        let raw = "---\ntitle: 用一篇记录开始\ndescription: 一篇演示\npubDate: 2026-09-26\ntags: [示例, 方法]\ndraft: false\n---\n\n正文\n";
        std::fs::write(f.workspace.blog_dir().join("drift-a.md"), raw).unwrap();

        let before_site = site_hash_of(raw);
        let parsed = article_io::parse_markdown(raw).unwrap();
        let map = article_io::parse_front_matter_map(&parsed.front_matter).unwrap();
        let meta = article_io::meta_from_map(&map).unwrap();

        // 元数据与正文都没变的一次保存。
        let saved = f.workspace.save("drift-a", &meta, &parsed.body, None).unwrap();
        let disk = std::fs::read_to_string(f.workspace.blog_dir().join("drift-a.md")).unwrap();
        assert_eq!(
            site_hash_of(&disk),
            before_site,
            "未改动内容的保存不得改变网站哈希（否则会被误判为旧版）：{disk}"
        );
        assert!(disk.contains("tags: [示例, 方法]"), "tags 写法不得被改写：{disk}");
        assert_eq!(saved.meta.tags, vec!["示例", "方法"]);

        // 校验失败时磁盘必须保持原样（先校验、后写盘）。
        let mangled = "---\ntitle: \"T\"\ndescription: \"D\"\npubDate: \"2026-09-26\"\ntags: [a, b]\ndraft: false\n---\n\n正文\n";
        std::fs::write(f.workspace.blog_dir().join("bad-a.md"), mangled).unwrap();
        let broken_meta = ArticleMeta {
            title: "T".to_string(),
            // 空 description 会被站点 schema 拒绝，从而在写盘前报错。
            description: String::new(),
            pub_date: "2026-09-26".to_string(),
            updated_date: None,
            tags: vec!["a".to_string(), "b".to_string()],
            draft: false,
        };
        assert!(f.workspace.save("bad-a", &broken_meta, "正文\n", None).is_err());
        let after = std::fs::read_to_string(f.workspace.blog_dir().join("bad-a.md")).unwrap();
        assert_eq!(after, mangled, "校验失败时磁盘文件必须逐字节保持原样");
    }

    /// P1 回归：一个超长的中文文件名不得让整个列表扫描失败。
    ///
    /// 旧实现按**字节**判断段长（汉字 3 字节，约 34 字即「超长」），且 `scan`
    /// 用 `?` 把 ID 校验错误向上抛，结果一篇长中文名的文章会让软件**列不出
    /// 任何文章、也无法新建**。现在：长度按字符计，且不合法名字单独作为错误
    /// 条目展示，不影响其它文章。
    #[test]
    fn long_chinese_file_name_does_not_break_scan_or_create() {
        let f = fixture();
        f.workspace.create("normal-a", &meta("正常文章", true), "正文\n").unwrap();
        // 40 个汉字（120 字节，旧实现判为超长）+ 一段超长标题。
        let long_name: String = "观察笔记".repeat(10); // 40 字
        std::fs::write(
            f.workspace.blog_dir().join(format!("{long_name}.md")),
            "---\ntitle: \"长名\"\ndescription: \"d\"\npubDate: \"2026-09-26\"\ntags: []\ndraft: true\n---\n\n正文\n",
        )
        .unwrap();

        // 40 个汉字的名字本身是合法的（按字符计未超 100）。
        assert!(
            paths::validate_existing_article_id(&long_name).is_ok(),
            "40 个汉字不应被判为超长"
        );

        let list = f.workspace.scan(&f.store, &BTreeMap::new()).unwrap();
        assert_eq!(list.len(), 2, "长中文名不得让整个扫描失败：{list:?}");
        assert!(list.iter().any(|s| s.id == "normal-a"), "正常文章仍应列出");

        // 新建仍可用（不被坏名字阻断）。
        assert!(f.workspace.check_id_available("brand-new").unwrap().is_none());
        f.workspace.create("brand-new", &meta("新文章", true), "x\n").unwrap();
    }

    /// 真正不合法的文件名（超长到超过字符上限）只影响它自己：仍出现在列表里
    /// 并带错误说明，其余文章可正常使用。
    #[test]
    fn invalid_file_name_is_listed_with_error_but_does_not_block_others() {
        let f = fixture();
        f.workspace.create("good-a", &meta("好文章", true), "正文\n").unwrap();
        // 120 个汉字：超过 100 字符的段长上限。
        let too_long: String = "很长的目录名".repeat(20); // 120 字
        std::fs::write(
            f.workspace.blog_dir().join(format!("{too_long}.md")),
            "---\ntitle: \"x\"\ndescription: \"d\"\npubDate: \"2026-09-26\"\ntags: []\ndraft: true\n---\n\n正文\n",
        )
        .unwrap();
        assert!(paths::validate_existing_article_id(&too_long).is_err(), "120 字应判为超长");

        let list = f.workspace.scan(&f.store, &BTreeMap::new()).unwrap();
        assert_eq!(list.len(), 2, "坏名字仍要出现在列表里，不被静默跳过");
        let bad = list.iter().find(|s| s.load_error.is_some()).expect("坏名字条目应带错误");
        assert_eq!(bad.load_error.as_ref().unwrap().code, ErrorCode::ArticleIdInvalid);
        assert!(list.iter().any(|s| s.id == "good-a"), "其它文章不受影响");

        // 新建与冲突检查不被坏名字阻断。
        assert!(f.workspace.check_id_available("fresh-id").unwrap().is_none());
    }

    /// P1-4 回归：状态哈希必须与同步基线同一基准（磁盘原文）。
    ///
    /// 旧实现用 `render()` 的重建结果算本地哈希，而同步基线用文件原始字节。
    /// 当围栏行带尾随空格时 `render() != 原文`，`derive_status` 的
    /// `local_hash == writing_hash` 永远不成立，已同步的文章会恒显示「未同步」。
    #[test]
    fn status_hash_uses_raw_bytes_not_rendered_text() {
        let f = fixture();
        // 结束围栏带两个尾随空格：`render()` 会把它规范掉，原文不会。
        let raw = "---\ntitle: \"尾随空格\"\ndescription: \"d\"\npubDate: \"2026-09-26\"\ntags: []\ndraft: false\n---  \n\n正文\n";
        std::fs::write(f.workspace.blog_dir().join("raw-a.md"), raw).unwrap();

        // 前置：确实发生了规范化（否则本用例证明不了基准问题）。
        let parsed = article_io::parse_markdown(raw).unwrap();
        assert_ne!(parsed.render(), raw, "该写法应发生规范化");

        // 同步基线记录的正是**原文**哈希；状态层必须据此判定为「已同步」。
        let raw_hash = util::hash_bytes(raw.as_bytes());
        let render_hash = util::hash_bytes(parsed.render().as_bytes());
        assert_ne!(render_hash, raw_hash, "前置条件：两种基准确实不同");

        let mut snapshots = BTreeMap::new();
        snapshots.insert(
            "raw-a".to_string(),
            RemoteSnapshot { writing_hash: Some(raw_hash.clone()), ..Default::default() },
        );
        let content = f.workspace.read("raw-a", &snapshots).unwrap();
        assert_eq!(content.content_hash, raw_hash, "内容哈希应取磁盘原文");
        assert_eq!(
            content.status.remote_sync,
            crate::model::RemoteSync::Saved,
            "原文与写作分支一致时必须显示已同步（用 render() 哈希会恒为未同步）：{:?}",
            content.status
        );
    }

    #[test]
    fn save_does_not_touch_updated_date_by_default() {
        let f = fixture();
        f.workspace.create("a-post", &meta("T", false), "正文").unwrap();
        // 写入一次 updatedDate，之后默认保存不得刷新它。
        let with_date =
            f.workspace.save("a-post", &meta("T", false), "正文", Some(Some("2026-01-01"))).unwrap();
        assert_eq!(with_date.meta.updated_date.as_deref(), Some("2026-01-01"));

        let later = f.workspace.save("a-post", &meta("T", false), "正文2", None).unwrap();
        assert_eq!(
            later.meta.updated_date.as_deref(),
            Some("2026-01-01"),
            "自动保存不应刷新 updatedDate"
        );
    }

    #[test]
    fn scan_never_silently_skips_broken_files() {
        let f = fixture();
        f.workspace.create("good", &meta("正常", false), "正文").unwrap();
        std::fs::write(f.workspace.blog_dir().join("broken.md"), "没有 front matter").unwrap();

        let list = f.workspace.scan(&f.store, &BTreeMap::new()).unwrap();
        assert_eq!(list.len(), 2, "坏文件也必须出现在列表中");
        let broken = list.iter().find(|s| s.id == "broken").expect("坏文件条目应存在");
        assert!(broken.load_error.is_some());
        assert_eq!(
            broken.load_error.as_ref().unwrap().code,
            ErrorCode::FrontMatterMissing
        );
        let good = list.iter().find(|s| s.id == "good").unwrap();
        assert!(good.load_error.is_none());
    }

    #[test]
    fn scan_ignores_gitkeep_and_hidden_files() {
        let f = fixture();
        std::fs::write(f.workspace.blog_dir().join(".gitkeep"), "").unwrap();
        std::fs::write(f.workspace.blog_dir().join(".hidden.md"), "x").unwrap();
        assert!(f.workspace.scan(&f.store, &BTreeMap::new()).unwrap().is_empty());
    }

    #[test]
    fn nested_ids_are_supported() {
        let f = fixture();
        f.workspace.create("notes/first", &meta("嵌套", true), "正文").unwrap();
        let list = f.workspace.scan(&f.store, &BTreeMap::new()).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, "notes/first");
        assert!(f.workspace.markdown_abs_path("notes/first").unwrap().exists());
    }

    #[test]
    fn image_count_is_reported_per_article() {
        let f = fixture();
        f.workspace.create("with-img", &meta("T", true), "正文").unwrap();
        let png = [0x89u8, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 1];
        images::import_image_bytes(f.workspace.root(), "with-img", "fig", &png).unwrap();
        let list = f.workspace.scan(&f.store, &BTreeMap::new()).unwrap();
        assert_eq!(list[0].image_count, 1);
    }

    #[test]
    fn status_derivation_covers_all_site_states() {
        let local = "hash-local".to_string();
        let site = "hash-site".to_string();
        let base = StatusInputs {
            local_hash: local.clone(),
            local_site_hash: site.clone(),
            locally_saved: true,
            remote: RemoteSnapshot::default(),
        };
        assert_eq!(derive_status(&base).site, SiteState::NeverPublished);
        assert_eq!(derive_status(&base).remote_sync, RemoteSync::LocalOnly);

        // writing 已存且与本地一致。
        let synced = StatusInputs {
            remote: RemoteSnapshot { writing_hash: Some(local.clone()), ..Default::default() },
            ..base.clone()
        };
        assert_eq!(derive_status(&synced).remote_sync, RemoteSync::Saved);

        // A3：main 已发布且已部署，但写作分支有新稿 → 网站仍是旧版。
        let old_site = StatusInputs {
            remote: RemoteSnapshot {
                writing_hash: Some(local.clone()),
                main_hash: Some("hash-old".to_string()),
                main_site_hash: Some("hash-old-site".to_string()),
                main_commit: Some("c1".to_string()),
                main_published: Some(true),
                deployed_commit: Some("c1".to_string()),
                ..Default::default()
            },
            ..base.clone()
        };
        assert_eq!(derive_status(&old_site).site, SiteState::LiveOldVersion);

        // 已推送 main 但尚无部署确认。
        let submitted = StatusInputs {
            remote: RemoteSnapshot {
                writing_hash: Some(local.clone()),
                main_hash: Some("hash-published".to_string()),
                main_site_hash: Some(site.clone()),
                main_commit: Some("c2".to_string()),
                main_published: Some(true),
                ..Default::default()
            },
            ..base.clone()
        };
        assert_eq!(derive_status(&submitted).site, SiteState::PublicationSubmitted);

        // 部署失败必须与已上线区分。
        let failed = StatusInputs {
            remote: RemoteSnapshot {
                deploy_failed_commit: Some("c2".to_string()),
                ..submitted.remote.clone()
            },
            ..base.clone()
        };
        assert_eq!(derive_status(&failed).site, SiteState::DeployFailed);

        // 网站内容与本地一致且已部署 → 当前版本已上线。
        let current = StatusInputs {
            remote: RemoteSnapshot {
                writing_hash: Some(local.clone()),
                main_hash: Some("hash-published".to_string()),
                main_site_hash: Some(site.clone()),
                main_commit: Some("c3".to_string()),
                main_published: Some(true),
                deployed_commit: Some("c3".to_string()),
                deployment_url: Some("https://example.invalid/".to_string()),
                ..Default::default()
            },
            ..base.clone()
        };
        let status = derive_status(&current);
        assert_eq!(status.site, SiteState::LiveCurrentVersion);
        assert_eq!(status.deployment_url.as_deref(), Some("https://example.invalid/"));

        // 撤下。
        let withdrawn = StatusInputs {
            remote: RemoteSnapshot {
                main_hash: Some("hash-old".to_string()),
                main_site_hash: Some("hash-old-site".to_string()),
                main_commit: Some("c4".to_string()),
                main_published: Some(false),
                deployed_commit: Some("c4".to_string()),
                ..Default::default()
            },
            ..base.clone()
        };
        assert_eq!(derive_status(&withdrawn).site, SiteState::Withdrawn);

        // 未保存到磁盘时不得声称本地已保存。
        let unsaved = StatusInputs { locally_saved: false, ..current.clone() };
        assert!(!derive_status(&unsaved).locally_saved);
    }

    /// P0-2 回归：仓库级 `main` 头存在，但文章**从未出现在 `main`** 时，
    /// 不得显示成「已提交发布」。旧实现以 `main_commit.is_some()` 作为
    /// 「发布过」的判据，而 `main_commit` 是仓库级头，任何文章都有值。
    #[test]
    fn never_published_article_stays_never_published() {
        let inputs = StatusInputs {
            local_hash: "h".to_string(),
            local_site_hash: "s".to_string(),
            locally_saved: true,
            remote: RemoteSnapshot {
                writing_hash: Some("h".to_string()),
                // 文章不在 main 上：main_hash / main_commit 均为 None。
                main_hash: None,
                main_commit: None,
                ..Default::default()
            },
        };
        let status = derive_status(&inputs);
        assert_eq!(status.site, SiteState::NeverPublished);
        assert!(status.main_commit.is_none(), "不在 main 上的文章不应带 main 提交");
    }

    /// P0-2 回归：刚推送成功、部署尚未核实时必须是「已提交发布」，
    /// 而不是「网站已上线」；只有确认部署成功（且提交一致）才显示上线。
    #[test]
    fn pushed_without_deploy_confirmation_is_submitted_not_live() {
        let base = StatusInputs {
            local_hash: "h".to_string(),
            local_site_hash: "s".to_string(),
            locally_saved: true,
            remote: RemoteSnapshot::default(),
        };
        let pushed = StatusInputs {
            remote: RemoteSnapshot {
                writing_hash: Some("h".to_string()),
                main_hash: Some("h".to_string()),
                main_site_hash: Some("s".to_string()),
                main_commit: Some("c1".to_string()),
                main_published: Some(true),
                // 没有任何部署结论。
                ..Default::default()
            },
            ..base.clone()
        };
        assert_eq!(
            derive_status(&pushed).site,
            SiteState::PublicationSubmitted,
            "推送成功但未核实部署时不得显示已上线"
        );

        // 部署结论属于**另一个**提交时，同样只能是「已提交发布」。
        let other_commit = StatusInputs {
            remote: RemoteSnapshot {
                deployed_commit: Some("c-old".to_string()),
                ..pushed.remote.clone()
            },
            ..base.clone()
        };
        assert_eq!(derive_status(&other_commit).site, SiteState::PublicationSubmitted);

        // 部署进行中。
        let deploying = StatusInputs {
            remote: RemoteSnapshot { deploying_commit: Some("c1".to_string()), ..pushed.remote.clone() },
            ..base.clone()
        };
        assert_eq!(derive_status(&deploying).site, SiteState::Deploying);

        // 部署失败。
        let failed = StatusInputs {
            remote: RemoteSnapshot { deploy_failed_commit: Some("c1".to_string()), ..pushed.remote.clone() },
            ..base.clone()
        };
        assert_eq!(derive_status(&failed).site, SiteState::DeployFailed);

        // 确认部署成功且提交一致 → 才显示上线。
        let live = StatusInputs {
            remote: RemoteSnapshot { deployed_commit: Some("c1".to_string()), ..pushed.remote.clone() },
            ..base.clone()
        };
        assert_eq!(derive_status(&live).site, SiteState::LiveCurrentVersion);
    }

    /// 刚发布的文章（本地 `draft: true`、`main` `draft: false`）不应被判为旧版。
    #[test]
    fn draft_flag_alone_does_not_mark_site_as_old() {
        let draft_md = "---\ntitle: \"T\"\ndescription: \"D\"\npubDate: \"2026-09-23\"\ndraft: true\n---\n\n正文\n";
        let published_md =
            "---\ntitle: \"T\"\ndescription: \"D\"\npubDate: \"2026-09-23\"\ndraft: false\n---\n\n正文\n";

        // 两份文本只差 draft 值，规范化后哈希必须一致。
        assert_eq!(site_hash_of(draft_md), site_hash_of(published_md));
        // 原始哈希不同，说明确实只是 draft 造成差异。
        assert_ne!(
            util::hash_bytes(draft_md.as_bytes()),
            util::hash_bytes(published_md.as_bytes())
        );

        let status = derive_status(&StatusInputs {
            local_hash: util::hash_bytes(draft_md.as_bytes()),
            local_site_hash: site_hash_of(draft_md),
            locally_saved: true,
            remote: RemoteSnapshot {
                writing_hash: Some(util::hash_bytes(draft_md.as_bytes())),
                main_hash: Some(util::hash_bytes(published_md.as_bytes())),
                main_site_hash: Some(site_hash_of(published_md)),
                main_commit: Some("c1".to_string()),
                main_published: Some(true),
                deployed_commit: Some("c1".to_string()),
                ..Default::default()
            },
        });
        assert_eq!(status.site, SiteState::LiveCurrentVersion, "只差 draft 字段不应算作旧版");
    }

    #[test]
    fn import_copies_only_the_chosen_file() {
        let f = fixture();
        let outside = tempfile::tempdir().unwrap();
        let src = outside.path().join("reading-code-example.md");
        let content = "---\ntitle: \"导入示例\"\ndescription: \"摘要\"\npubDate: \"2026-09-23\"\ntags: [示例]\ndraft: false\n---\n\n正文。\n";
        std::fs::write(&src, content).unwrap();
        // 同目录的另一个文件不应被带入。
        std::fs::write(outside.path().join("other.md"), "不应导入").unwrap();

        let imported = f.workspace.import_markdown_file(&src, "reading-code-example").unwrap();
        assert_eq!(imported.meta.title, "导入示例");
        assert_eq!(std::fs::read_to_string(&src).unwrap(), content, "源文件不应被改动");
        assert!(!f.workspace.blog_dir().join("other.md").exists(), "只导入选中的文件");
        assert!(outside.path().join("other.md").exists());
    }

    #[test]
    fn import_rejects_unparsable_file() {
        let f = fixture();
        let outside = tempfile::tempdir().unwrap();
        let src = outside.path().join("bad.md");
        std::fs::write(&src, "没有 front matter").unwrap();
        let err = f.workspace.import_markdown_file(&src, "bad-file").unwrap_err();
        assert_eq!(err.code, ErrorCode::FrontMatterMissing);
        assert!(!f.workspace.blog_dir().join("bad-file.md").exists());
    }

    #[test]
    fn rename_moves_markdown_and_images_and_rewrites_urls() {
        let f = fixture();
        let png = [0x89u8, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 1];
        f.workspace.create("old-id", &meta("标题", false), "").unwrap();
        let (_, url) = images::import_image_bytes(f.workspace.root(), "old-id", "fig", &png).unwrap();
        let body = format!("![图]({url})\n");
        f.workspace.save("old-id", &meta("标题", false), &body, None).unwrap();

        f.workspace.rename_article_id("old-id", "new-id").unwrap();

        assert!(!f.workspace.markdown_abs_path("old-id").unwrap().exists());
        let renamed = f.workspace.read("new-id", &BTreeMap::new()).unwrap();
        assert!(renamed.body.contains("/blog/new-id/"), "正文中的图片引用应改写：{}", renamed.body);
        assert!(!renamed.body.contains("/blog/old-id/"));
        assert!(f.workspace.images_root().join("new-id").is_dir());
        assert!(!f.workspace.images_root().join("old-id").exists());
    }

    #[test]
    fn rename_refuses_occupied_target() {
        let f = fixture();
        f.workspace.create("a", &meta("A", true), "x").unwrap();
        f.workspace.create("b", &meta("B", true), "x").unwrap();
        let err = f.workspace.rename_article_id("a", "b").unwrap_err();
        assert_eq!(err.code, ErrorCode::ArticleExists);
        assert!(f.workspace.markdown_abs_path("a").unwrap().exists(), "失败时不得移动文件");
    }

    #[test]
    fn backup_copies_markdown_and_images() {
        let f = fixture();
        let png = [0x89u8, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 1];
        f.workspace.create("victim", &meta("被删", false), "正文").unwrap();
        images::import_image_bytes(f.workspace.root(), "victim", "fig", &png).unwrap();

        let entry_dir = f.store.trash_entry_dir("op-test");
        let report = f.workspace.backup_to_trash(&entry_dir, "victim").unwrap();
        assert!(report.has_markdown);
        assert_eq!(report.images.len(), 1);
        assert!(entry_dir.join(&report.markdown_backup_name).exists());
        assert!(entry_dir.join(&report.images[0].backup_name).exists());

        // 删除正文后仍可从备份还原。
        f.workspace.remove_markdown("victim").unwrap();
        let backup = std::fs::read_to_string(entry_dir.join(&report.markdown_backup_name)).unwrap();
        f.workspace.write_raw("victim", backup.as_bytes()).unwrap();
        assert_eq!(f.workspace.read("victim", &BTreeMap::new()).unwrap().meta.title, "被删");
    }

    #[test]
    fn markdown_texts_returns_all_articles_for_reference_checks() {
        let f = fixture();
        f.workspace.create("one", &meta("一", true), "x").unwrap();
        f.workspace.create("notes/two", &meta("二", true), "y").unwrap();
        let texts = f.workspace.markdown_texts().unwrap();
        let ids: Vec<&str> = texts.iter().map(|(id, _)| id.as_str()).collect();
        assert!(ids.contains(&"one"));
        assert!(ids.contains(&"notes/two"));
    }

    #[test]
    fn open_requires_existing_directory() {
        let err = Workspace::open(PathBuf::from("Z:/definitely/missing/dir")).unwrap_err();
        assert_eq!(err.code, ErrorCode::IoFailed);
    }
}
