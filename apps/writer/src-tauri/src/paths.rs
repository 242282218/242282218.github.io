//! 路径规范化与受管范围校验。
//!
//! 软件只允许改动两处受管路径：`src/content/blog/**/*.md` 与
//! `public/blog/<article-id>/`。本模块拒绝 `..`、绝对路径、Windows 设备名、
//! 尾部点/空格、大小写冲突与越界符号链接，是所有文件操作的唯一入口。

use crate::model::{ErrorCode, Result, WriterError};
use std::path::Path;

/// 文章 Markdown 的受管根目录（相对仓库根，带尾斜杠）。
pub const BLOG_DIR_PREFIX: &str = "src/content/blog/";
/// 文章专属图片的受管根目录（相对仓库根，带尾斜杠）。
pub const IMAGE_DIR_PREFIX: &str = "public/blog/";

/// Windows 保留设备名，任何路径段命中即拒绝（含带扩展名形式）。
const RESERVED_NAMES: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// 单个路径段允许的最大长度（按**字符**计，不是字节）。
///
/// 中文文件名在 UTF-8 下每字 3 字节；若按字节算，约 34 个汉字就会「超长」，
/// 而这类名字在 Windows 上完全合法。按字符计数才与文件系统的实际限制一致，
/// 也避免合法中文文章名让整个目录扫描失败。
const MAX_SEGMENT_LEN: usize = 100;
/// 文章 ID 的最大总长度（按**字符**计，不是字节）。
const MAX_ARTICLE_ID_LEN: usize = 240;

fn illegal_chars() -> &'static [char] {
    &['<', '>', ':', '"', '|', '?', '*']
}

/// 判断路径段是否为 Windows 保留设备名（忽略大小写与扩展名）。
fn is_reserved_name(segment: &str) -> bool {
    let stem = segment.split('.').next().unwrap_or(segment);
    RESERVED_NAMES.iter().any(|reserved| stem.eq_ignore_ascii_case(reserved))
}

/// 校验单个路径段的通用安全规则（不区分新老文章）。
fn validate_segment(segment: &str) -> Result<()> {
    if segment.is_empty() {
        return Err(invalid("路径中存在空目录名"));
    }
    if segment == "." || segment == ".." {
        return Err(invalid("不允许使用相对路径片段 `.` 或 `..`"));
    }
    if segment.chars().count() > MAX_SEGMENT_LEN {
        return Err(invalid("单个目录名过长"));
    }
    if segment.contains(illegal_chars()) {
        return Err(invalid("目录名包含 Windows 不允许的字符"));
    }
    if segment.contains(['/', '\\']) {
        return Err(invalid("目录名不能包含路径分隔符"));
    }
    if segment.chars().any(|c| c.is_control()) {
        return Err(invalid("目录名包含控制字符"));
    }
    if segment.starts_with(' ') || segment.ends_with(' ') || segment.ends_with('.') {
        return Err(invalid("目录名不能以空格开头或以空格/点结尾"));
    }
    if is_reserved_name(segment) {
        return Err(invalid("目录名与 Windows 保留设备名冲突"));
    }
    Ok(())
}

fn invalid(message: &str) -> WriterError {
    WriterError::new(ErrorCode::ArticleIdInvalid, message)
}

/// 校验文章 ID 的分段形式。
fn validate_segments(article_id: &str) -> Result<()> {
    if article_id.is_empty() {
        return Err(invalid("文章标识不能为空"));
    }
    if article_id.chars().count() > MAX_ARTICLE_ID_LEN {
        return Err(invalid("文章标识过长"));
    }
    let normalized = article_id.replace('\\', "/");
    if normalized.starts_with('/') {
        return Err(invalid("文章标识不能是绝对路径"));
    }
    // Windows 盘符，如 `C:foo`。
    if normalized.len() >= 2 && normalized.as_bytes()[1] == b':' {
        return Err(invalid("文章标识不能包含盘符"));
    }
    for segment in normalized.split('/') {
        validate_segment(segment)?;
    }
    Ok(())
}

/// 校验**新建**文章时使用的 URL 标识。
///
/// 只允许英文小写字母、数字与短横线，每段至少含一个字母或数字，
/// 以可读的短横线命名并规避 Git 路径碰撞。
pub fn validate_new_article_id(article_id: &str) -> Result<()> {
    validate_segments(article_id)?;
    let normalized = article_id.replace('\\', "/");
    for segment in normalized.split('/') {
        let ok = segment
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        if !ok || !segment.chars().any(|c| c.is_ascii_alphanumeric()) {
            return Err(invalid(
                "新建文章标识只能使用小写英文字母、数字和短横线，且需含字母或数字",
            ));
        }
    }
    Ok(())
}

/// 校验**已有**文章 ID 的安全性。
///
/// 允许既有中文文件名与原样 URL，只拦截穿越、设备名和非法字符，
/// 不强制改名。
pub fn validate_existing_article_id(article_id: &str) -> Result<()> {
    validate_segments(article_id)
}

/// 校验操作 ID。
///
/// 操作 ID 会直接拼成回收区目录名（`<数据目录>/trash/<op_id>`），因此必须与
/// 文章 ID 同等严格地校验：只接受本软件自己生成的 `<unix秒>-<12位小写十六进制>`
/// 形式。否则 `..` 之类的值会让目录解析到数据目录之外并被递归删除。
pub fn validate_operation_id(op_id: &str) -> Result<()> {
    let err = || {
        WriterError::new(ErrorCode::InvalidArgument, "操作标识格式不正确")
            .with_detail(op_id.to_string())
    };
    let Some(rest) = op_id.strip_prefix("op-") else {
        return Err(err());
    };
    let Some((seconds, digest)) = rest.split_once('-') else {
        return Err(err());
    };
    if seconds.is_empty() || !seconds.bytes().all(|b| b.is_ascii_digit()) {
        return Err(err());
    }
    if digest.len() != 12 || !digest.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(err());
    }
    Ok(())
}

/// 判断相对路径是否落在受管目录内（纯字符串判断，不含符号链接解析）。
pub fn is_managed_rel_path(rel_path: &str) -> bool {
    let normalized = rel_path.replace('\\', "/");
    if !normalized.starts_with(BLOG_DIR_PREFIX) && !normalized.starts_with(IMAGE_DIR_PREFIX) {
        return false;
    }
    if !normalized.contains('/') {
        return false;
    }
    !normalized.split('/').any(|segment| segment == ".." || segment == ".")
}

/// 校验“仓库内相对路径”的通用安全规则，并确认位于受管目录内。
pub fn validate_managed_rel_path(rel_path: &str) -> Result<()> {
    let normalized = rel_path.replace('\\', "/");
    if normalized.starts_with('/') {
        return Err(out_of_scope("路径必须是仓库内相对路径"));
    }
    if normalized.len() >= 2 && normalized.as_bytes()[1] == b':' {
        return Err(out_of_scope("路径不能包含盘符"));
    }
    if !is_managed_rel_path(&normalized) {
        return Err(out_of_scope("路径不在软件受管的文章或图片目录内"));
    }
    let remainder = normalized
        .strip_prefix(BLOG_DIR_PREFIX)
        .or_else(|| normalized.strip_prefix(IMAGE_DIR_PREFIX))
        .unwrap_or("");
    for segment in remainder.split('/') {
        validate_segment(segment)?;
    }
    Ok(())
}

fn out_of_scope(message: &str) -> WriterError {
    WriterError::new(ErrorCode::PathOutOfScope, message)
}

/// 复核受管路径没有经由符号链接或 Windows 目录联接逃出工作区根目录。
///
/// 前面的字符串校验（`..`、绝对路径、保留名等）只看**名字**；如果受管目录里
/// 存在指向外部的链接类对象（例如 `src/content/blog/esc` 是指向仓库外目录的
/// 目录联接），`root.join(rel)` 之后的所有读写都会跟随链接落到仓库之外——
/// 可能覆盖或删除任意文件，且不可逆。
///
/// 因此所有落盘/删除前都要用本函数复核：从根目录逐段向下走，任一已存在的
/// 路径段是链接类对象即拒绝。软件自己从不在这两个受管目录里创建链接，因此
/// 「一律拒绝」既安全又不影响正常使用；路径段尚不存在时（新建文件）视为通过。
///
/// 注意：这仍是「先校验、后使用」，与真正的 `O_NOFOLLOW` 语义相比存在理论上的
/// 竞态窗口。Rust 标准库未提供可移植的「不跟随打开」，软件也只在单实例、
/// 独占工作区的前提下运行，因此这是当前可达成的防护上限。
pub fn verify_no_link_escape(root: &Path, rel_path: &str) -> Result<()> {
    validate_managed_rel_path(rel_path)?;
    let normalized = rel_path.replace('\\', "/");
    let mut current = root.to_path_buf();
    for segment in normalized.split('/') {
        current.push(segment);
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if crate::util::is_link_like(&metadata) {
                    return Err(WriterError::new(
                        ErrorCode::PathOutOfScope,
                        "受管路径中存在符号链接或目录联接，已拒绝访问以免写到仓库之外",
                    )
                    .with_detail(current.to_string_lossy().to_string()));
                }
            }
            // 该段（及其后续）尚不存在：安全，后续创建的都是普通目录/文件。
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            // 无法判断（权限、被占用等）：宁可拒绝，不冒险跟随。
            Err(err) => {
                return Err(WriterError::new(
                    ErrorCode::IoFailed,
                    format!("无法复核受管路径是否越界：{err}"),
                )
                .with_detail(current.to_string_lossy().to_string()))
            }
        }
    }
    Ok(())
}

/// 受管 Markdown 文件的相对路径是否合法（须以 `.md` 结尾且位于 blog 目录）。
pub fn validate_managed_markdown(rel_path: &str) -> Result<()> {
    validate_managed_rel_path(rel_path)?;
    let normalized = rel_path.replace('\\', "/");
    if !normalized.starts_with(BLOG_DIR_PREFIX) {
        return Err(out_of_scope("文章文件必须位于 src/content/blog/ 下"));
    }
    if !normalized.ends_with(".md") {
        return Err(out_of_scope("文章文件必须是 Markdown（.md）"));
    }
    Ok(())
}

/// 由文章 Markdown 的相对路径推导文章 ID。
pub fn article_id_from_rel_path(rel_path: &str) -> Result<String> {
    validate_managed_markdown(rel_path)?;
    let normalized = rel_path.replace('\\', "/");
    let id = normalized
        .strip_prefix(BLOG_DIR_PREFIX)
        .and_then(|rest| rest.strip_suffix(".md"))
        .ok_or_else(|| out_of_scope("无法从路径推导文章标识"))?;
    if id.is_empty() {
        return Err(out_of_scope("文章标识为空"));
    }
    // Windows 文件系统不区分大小写，重名判定复用大小写无关键。
    Ok(id.to_string())
}

/// 大小写无关的比较键，用于检测 Windows 上的大小写冲突与 Git 路径碰撞。
pub fn case_insensitive_key(value: &str) -> String {
    value.replace('\\', "/").to_lowercase()
}

/// 校验 `YYYY-MM-DD` 日期，拒绝不存在的日历日期。
pub fn validate_calendar_date(value: &str) -> Result<()> {
    let err = || {
        WriterError::new(
            ErrorCode::MetaFieldInvalid,
            "日期需使用 YYYY-MM-DD 格式且为真实存在的日期",
        )
        .with_detail(value.to_string())
    };
    let bytes = value.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return Err(err());
    }
    let parse = |s: &str| s.parse::<u32>().ok();
    let year = parse(&value[0..4]).ok_or_else(err)?;
    let month = parse(&value[5..7]).ok_or_else(err)?;
    let day = parse(&value[8..10]).ok_or_else(err)?;
    if year == 0 || !(1..=12).contains(&month) {
        return Err(err());
    }
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let max_day = match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if day == 0 || day > max_day {
        return Err(err());
    }
    Ok(())
}

/// 把任意用户输入转成可安全用作图片文件名的基本名。
///
/// 保留中文与常见字符，替换掉文件系统敏感字符；结果为空时返回 `None`。
pub fn sanitize_image_basename(input: &str) -> Option<String> {
    let stem = input.rsplit(['/', '\\']).next().unwrap_or(input);
    let stem = stem.split('.').next().unwrap_or(stem);
    let mut out = String::with_capacity(stem.len());
    let mut last_dash = false;
    for ch in stem.chars() {
        let keep = ch.is_alphanumeric() && !ch.is_control();
        if keep {
            out.push(ch);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        return None;
    }
    let truncated: String = trimmed.chars().take(60).collect();
    let trimmed = truncated.trim_matches('-').to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_readable_new_id() {
        assert!(validate_new_article_id("read-code").is_ok());
        assert!(validate_new_article_id("notes/2026-09-27-first").is_ok());
    }

    #[test]
    fn rejects_unsafe_new_id() {
        assert!(validate_new_article_id("../escape").is_err());
        assert!(validate_new_article_id("Read-Code").is_err());
        assert!(validate_new_article_id("read code").is_err());
        assert!(validate_new_article_id("con").is_err());
        assert!(validate_new_article_id("notes/con").is_err());
        assert!(validate_new_article_id("").is_err());
        assert!(validate_new_article_id("/abs").is_err());
        assert!(validate_new_article_id("C:win").is_err());
        // 前缀短横线（段内只允许出现在中间，但仍含字母数字，故合法）。
        assert!(validate_new_article_id("trailing-").is_ok());
        assert!(validate_new_article_id("-lead").is_ok());
    }

    /// 段长按**字符**计：中文名字在 UTF-8 下每字 3 字节，按字节算约 34 字就会
    /// 被误判为超长，而这类名字在 Windows 上完全合法。
    #[test]
    fn segment_length_counts_characters_not_bytes() {
        // 40 个汉字 = 120 字节，但只有 40 字符 → 合法。
        let forty_han = "观察笔记".repeat(10);
        assert_eq!(forty_han.chars().count(), 40);
        assert_eq!(forty_han.len(), 120);
        assert!(validate_existing_article_id(&forty_han).is_ok(), "40 个汉字应合法");

        // 恰好 100 字符（300 字节）仍合法；101 字符才拒绝。
        let exactly_100: String = std::iter::repeat('汉').take(100).collect();
        assert!(validate_existing_article_id(&exactly_100).is_ok(), "100 个汉字应合法");
        let over_100: String = std::iter::repeat('汉').take(101).collect();
        let err = validate_existing_article_id(&over_100).unwrap_err();
        assert!(err.message.contains("过长"), "101 个汉字应判为超长：{err:?}");

        // 纯 ASCII 的行为不变（100 字符合法，101 拒绝）。
        assert!(validate_existing_article_id(&"a".repeat(100)).is_ok());
        assert!(validate_existing_article_id(&"a".repeat(101)).is_err());
    }

    #[test]
    fn operation_id_rejects_traversal() {
        // 软件自己生成的形式。
        assert!(validate_operation_id("op-1758970000-a1b2c3d4e5f6").is_ok());
        // 回收区目录穿越：必须拒绝，否则会删到数据目录之外。
        assert!(validate_operation_id("..").is_err());
        assert!(validate_operation_id("../..").is_err());
        assert!(validate_operation_id(".").is_err());
        assert!(validate_operation_id("").is_err());
        assert!(validate_operation_id("op-../x").is_err());
        assert!(validate_operation_id("op-1758970000-ABCDEF012345").is_err());
        assert!(validate_operation_id("op-1758970000-a1b2c3d4e5f").is_err());
        assert!(validate_operation_id("op-x-a1b2c3d4e5f6").is_err());
        assert!(validate_operation_id("1758970000-a1b2c3d4e5f6").is_err());
        assert!(validate_operation_id("op-1758970000-a1b2c3d4e5f6/../..").is_err());
        // 带路径分隔符或盘符的一律拒绝。
        assert!(validate_operation_id("op-1-a1b2c3d4e5f6\\..").is_err());
        assert!(validate_operation_id("C:/op-1-a1b2c3d4e5f6").is_err());
    }

    #[test]
    fn existing_id_allows_chinese_but_blocks_traversal() {
        assert!(validate_existing_article_id("观澜/第一次记录").is_ok());
        assert!(validate_existing_article_id("..").is_err());
        assert!(validate_existing_article_id("a/../b").is_err());
        assert!(validate_existing_article_id("nul").is_err());
        assert!(validate_existing_article_id("a\\b").is_ok());
    }

    #[test]
    fn managed_path_membership() {
        assert!(is_managed_rel_path("src/content/blog/read-code.md"));
        assert!(is_managed_rel_path("public/blog/read-code/figure-01.png"));
        assert!(!is_managed_rel_path("src/content/config.ts"));
        assert!(!is_managed_rel_path("src/content/blog/../config.ts"));
        assert!(!is_managed_rel_path("public/guanlan-logo.png"));
    }

    #[test]
    fn derives_article_id() {
        assert_eq!(
            article_id_from_rel_path("src/content/blog/read-code.md").unwrap(),
            "read-code"
        );
        assert_eq!(
            article_id_from_rel_path("src/content/blog/notes/first.md").unwrap(),
            "notes/first"
        );
        assert!(article_id_from_rel_path("public/blog/a.md").is_err());
        assert!(article_id_from_rel_path("src/content/blog/a.txt").is_err());
    }

    #[test]
    fn validates_real_calendar_dates() {
        assert!(validate_calendar_date("2026-09-27").is_ok());
        assert!(validate_calendar_date("2024-02-29").is_ok());
        assert!(validate_calendar_date("2025-02-29").is_err());
        assert!(validate_calendar_date("2026-13-01").is_err());
        assert!(validate_calendar_date("2026-9-27").is_err());
        assert!(validate_calendar_date("2026-09-31").is_err());
    }

    #[test]
    fn sanitizes_image_names() {
        assert_eq!(sanitize_image_basename("My Figure 01.png").as_deref(), Some("My-Figure-01"));
        assert_eq!(sanitize_image_basename("图 1.jpg").as_deref(), Some("图-1"));
        assert_eq!(sanitize_image_basename("***.png"), None);
    }
}
