//! 图片随文章归档、引用检查与安全清理。
//!
//! 图片存放于 `public/blog/<article-id>/`；Markdown 中以站点根路径
//! `/blog/<article-id>/<name>` 引用。删除图片前必须确认它在**所有涉及分支**
//! 的受管 Markdown 中都没有引用，且属于该文章的独占目录；任何不确定的情况
//! 默认保留。

use crate::model::{ErrorCode, ImageRef, Result, WriterError};
use crate::paths;
use crate::util::{self, ImageKind};
use std::collections::BTreeSet;
use std::path::Path;

/// 由图片相对路径推导站点 URL 路径。
///
/// `public/blog/read-code/figure-01.png` → `/blog/read-code/figure-01.png`
pub fn url_path_for(rel_path: &str) -> Result<String> {
    paths::validate_managed_rel_path(rel_path)?;
    let normalized = rel_path.replace('\\', "/");
    let rest = normalized
        .strip_prefix("public/")
        .ok_or_else(|| out_of_scope("图片必须位于 public/blog/ 下"))?;
    Ok(format!("/{rest}"))
}

/// 由文章 ID 与文件名构造图片相对路径。
pub fn rel_path_for(article_id: &str, file_name: &str) -> Result<String> {
    paths::validate_existing_article_id(article_id)?;
    let rel = format!("{}{}/{}", paths::IMAGE_DIR_PREFIX, article_id, file_name);
    paths::validate_managed_rel_path(&rel)?;
    Ok(rel)
}

fn out_of_scope(message: &str) -> WriterError {
    WriterError::new(ErrorCode::PathOutOfScope, message)
}

/// 列出某篇文章专属图片目录中的图片文件。
///
/// 只返回经文件头确认的图片；未知文件保留但不计入。目录不存在时返回空列表。
pub fn article_images(workspace_root: &Path, article_id: &str) -> Result<Vec<ImageRef>> {
    // 文章标识必须合法，且其图片目录不得是链接类对象（否则会枚举仓库外的文件）。
    paths::validate_existing_article_id(article_id)?;
    let rel_dir = format!("{}{}", paths::IMAGE_DIR_PREFIX, article_id);
    paths::verify_no_link_escape(workspace_root, &rel_dir)?;
    let dir = workspace_root.join(paths::IMAGE_DIR_PREFIX).join(article_id);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    // 目录本身可能是链接（上面的复核覆盖已存在的段；这里再确认一次）。
    if let Ok(meta) = std::fs::symlink_metadata(&dir) {
        if crate::util::is_link_like(&meta) {
            return Err(WriterError::new(
                ErrorCode::PathOutOfScope,
                "图片目录是符号链接或目录联接，已拒绝访问以免读到仓库之外",
            )
            .with_detail(rel_dir));
        }
    }
    let mut out = Vec::new();
    let entries = std::fs::read_dir(&dir)
        .map_err(|e| WriterError::new(ErrorCode::IoFailed, format!("读取图片目录失败：{e}")))?;
    for entry in entries.flatten() {
        let path = entry.path();
        // 目录里的单个条目也必须是普通文件，不能是链接。
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if crate::util::is_link_like(&meta) || !meta.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        if ImageKind::from_magic(&bytes).is_none() {
            continue;
        }
        let rel_path = match rel_path_for(article_id, name) {
            Ok(p) => p,
            Err(_) => continue,
        };
        out.push(ImageRef {
            rel_path,
            size: bytes.len() as u64,
            content_hash: util::hash_bytes(&bytes),
            referenced_by: Vec::new(),
        });
    }
    out.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    Ok(out)
}

/// 在给定文本集合中查找引用了指定图片 URL 的文章。
///
/// 同时识别站点根路径（`/blog/id/name`）与相对路径（`./name`、`name`）形式，
/// 以减少「实际有引用但判断为无引用」的风险。
pub fn find_references_in_texts(
    texts: &[(String, String)],
    image_url_path: &str,
    image_file_name: &str,
) -> Vec<String> {
    let mut owners = BTreeSet::new();
    for (article_id, text) in texts {
        if text_references(text, image_url_path, image_file_name) {
            owners.insert(article_id.clone());
        }
    }
    owners.into_iter().collect()
}

/// 判断单份 Markdown 文本是否引用了目标图片。
pub fn text_references(text: &str, image_url_path: &str, image_file_name: &str) -> bool {
    if text.contains(image_url_path) {
        return true;
    }
    // 同目录相对引用，如 `./figure-01.png` 或 `figure-01.png`。
    for pattern in ["(./", "(", " \""] {
        let needle = format!("{pattern}{image_file_name}");
        if text.contains(&needle) {
            return true;
        }
    }
    // HTML `<img src="figure-01.png">` 一类写法。
    if text.contains(&format!("src=\"{image_file_name}\""))
        || text.contains(&format!("src='{image_file_name}'"))
    {
        return true;
    }
    false
}

/// 判断图片是否被**其他**文章引用（用于独占性判定）。
pub fn is_exclusively_owned(referenced_by: &[String], article_id: &str) -> bool {
    referenced_by.iter().all(|owner| owner == article_id)
}

/// 生成去重的图片文件名：可读基本名 + 短内容哈希 + 规范扩展名。
pub fn build_image_file_name(basename_source: &str, bytes: &[u8], kind: ImageKind) -> String {
    let base = paths::sanitize_image_basename(basename_source)
        .unwrap_or_else(|| "figure".to_string());
    format!("{}-{}.{}", base, util::short_hash(bytes), kind.extension())
}

/// 把图片字节写入文章专属目录，返回其相对路径与 URL 路径。
pub fn import_image_bytes(
    workspace_root: &Path,
    article_id: &str,
    basename_source: &str,
    bytes: &[u8],
) -> Result<(String, String)> {
    let kind = util::detect_image(bytes)?;
    let file_name = build_image_file_name(basename_source, bytes, kind);
    let rel_path = rel_path_for(article_id, &file_name)?;
    // 写入前复核：受管目录里的链接会让写盘落到仓库之外。
    paths::verify_no_link_escape(workspace_root, &rel_path)?;
    let abs = workspace_root.join(&rel_path);
    if let Some(parent) = abs.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            WriterError::new(ErrorCode::IoFailed, format!("创建图片目录失败：{e}"))
        })?;
    }
    // 复核创建过程中没有引入/经过链接（父目录可能是既有联接）。
    paths::verify_no_link_escape(workspace_root, &rel_path)?;
    crate::article_io::atomic_write(&abs, bytes)?;
    let url = url_path_for(&rel_path)?;
    Ok((rel_path, url))
}

/// 删除图片文件；仅在调用方已确认独占且无引用后使用。
pub fn remove_image(workspace_root: &Path, rel_path: &str) -> Result<()> {
    paths::validate_managed_rel_path(rel_path)?;
    let normalized = rel_path.replace('\\', "/");
    if !normalized.starts_with(paths::IMAGE_DIR_PREFIX) {
        return Err(out_of_scope("只能删除文章专属图片目录中的文件"));
    }
    // 复合复核：目录里若存在指向外部的链接，删除会落到仓库之外（不可逆）。
    paths::verify_no_link_escape(workspace_root, &normalized)?;
    let abs = workspace_root.join(&normalized);
    match std::fs::remove_file(&abs) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(WriterError::new(ErrorCode::IoFailed, format!("删除图片失败：{e}"))),
    }
}

/// 在 Markdown 中把图片引用从旧 URL 改写为新 URL。
pub fn rewrite_reference(text: &str, old_url: &str, new_url: &str) -> String {
    text.replace(old_url, new_url)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 1, 2, 3, 4];

    #[test]
    fn url_paths_map_from_public() {
        assert_eq!(
            url_path_for("public/blog/read-code/figure-01.png").unwrap(),
            "/blog/read-code/figure-01.png"
        );
        assert!(url_path_for("src/content/blog/a.md").is_err());
        assert!(url_path_for("public/guanlan-logo.png").is_err());
    }

    #[test]
    fn detects_references_in_several_forms() {
        assert!(text_references("![x](/blog/a/f.png)", "/blog/a/f.png", "f.png"));
        assert!(text_references("![x](./f.png)", "/blog/a/f.png", "f.png"));
        assert!(text_references("![x](f.png)", "/blog/a/f.png", "f.png"));
        assert!(text_references("<img src=\"f.png\">", "/blog/a/f.png", "f.png"));
        assert!(!text_references("![x](/blog/a/other.png)", "/blog/a/f.png", "f.png"));
    }

    #[test]
    fn shared_image_is_not_exclusively_owned() {
        // 独占：没有引用者，或只有本文章引用。
        assert!(is_exclusively_owned(&[], "a"));
        assert!(is_exclusively_owned(&["a".to_string()], "a"));
        // 非独占：存在其他引用者（哪怕本文章也引用）。
        assert!(!is_exclusively_owned(&["a".to_string(), "b".to_string()], "a"));
        assert!(!is_exclusively_owned(&["b".to_string()], "a"));

        // 与真实保护路径同一判据：`images_referenced_elsewhere` 用这个函数
        // 决定哪些图片必须留下，因此上面的语义直接决定删除行为。
        let texts = vec![
            ("a".to_string(), "![x](/blog/a/f.png)".to_string()),
            ("b".to_string(), "![x](/blog/a/f.png)".to_string()),
        ];
        let owners = find_references_in_texts(&texts, "/blog/a/f.png", "f.png");
        assert!(!is_exclusively_owned(&owners, "a"), "b 也引用时必须判定为非独占");
    }

    #[test]
    fn file_name_includes_content_hash_and_extension() {
        let name = build_image_file_name("My Figure.png", PNG, ImageKind::Png);
        assert!(name.starts_with("My-Figure-"));
        assert!(name.ends_with(".png"));
        // 相同内容得到相同名字，不同内容得到不同名字。
        assert_eq!(name, build_image_file_name("My Figure.png", PNG, ImageKind::Png));
        let mut other = PNG.to_vec();
        other.push(9);
        assert_ne!(name, build_image_file_name("My Figure.png", &other, ImageKind::Png));
    }

    #[test]
    fn jpeg_is_normalized_to_jpg_extension() {
        let jpg = [0xFF, 0xD8, 0xFF, 0xE0, 0, 0];
        let name = build_image_file_name("photo.jpeg", &jpg, ImageKind::Jpeg);
        assert!(name.ends_with(".jpg"), "{name}");
    }

    #[test]
    fn import_writes_into_article_directory_only() {
        let dir = tempfile::tempdir().unwrap();
        let (rel, url) =
            import_image_bytes(dir.path(), "read-code", "figure 01.png", PNG).unwrap();
        assert!(rel.starts_with("public/blog/read-code/"));
        assert!(url.starts_with("/blog/read-code/"));
        assert!(dir.path().join(&rel).exists());

        // 伪装成 png 的文本被拒绝，且不落盘。
        let err = import_image_bytes(dir.path(), "read-code", "fake.png", b"<html>").unwrap_err();
        assert_eq!(err.code, ErrorCode::ImageUnsupported);
    }

    #[test]
    fn images_are_listed_with_hashes() {
        let dir = tempfile::tempdir().unwrap();
        import_image_bytes(dir.path(), "a", "one", PNG).unwrap();
        let images = article_images(dir.path(), "a").unwrap();
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].content_hash.len(), 64);
        assert_eq!(images[0].size, PNG.len() as u64);
        assert!(article_images(dir.path(), "missing").unwrap().is_empty());
    }

    #[test]
    fn removal_refuses_paths_outside_image_dir() {
        let dir = tempfile::tempdir().unwrap();
        let err = remove_image(dir.path(), "src/content/blog/a.md").unwrap_err();
        assert_eq!(err.code, ErrorCode::PathOutOfScope);
        // 目录穿越也被拒绝。
        let err = remove_image(dir.path(), "public/blog/../../secret.txt").unwrap_err();
        assert_eq!(err.code, ErrorCode::PathOutOfScope);
    }

    #[test]
    fn rewrites_reference_urls() {
        let text = "![a](/blog/old/f.png) 和 ![b](/blog/old/f.png)";
        let out = rewrite_reference(text, "/blog/old/f.png", "/blog/new/f.png");
        assert_eq!(out.matches("/blog/new/f.png").count(), 2);
        assert!(!out.contains("/blog/old/"));
    }
}
