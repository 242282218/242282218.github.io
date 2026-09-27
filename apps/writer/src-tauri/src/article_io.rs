//! front matter 的无损读取与原子写盘。
//!
//! 设计要点：
//! - 读取时把 front matter 的**原文**独立保留，字段解析失败也不丢内容；
//! - 受控修改字段采用「按行替换」的字节级手术，保留未理解字段、字段顺序、
//!   引号形式和正文换行；
//! - 语法错误定位到行号并阻断同步/发布，绝不「修复为默认值」后覆盖原文。

use crate::model::{ArticleMeta, ErrorCode, Result, WriterError};
use crate::paths;
use serde_json::{Map, Value};
use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

const FENCE: &str = "---";

/// 站点已声明的字段名。软件只向站点写这些字段。
const KNOWN_FIELDS: [&str; 6] = ["title", "description", "pubDate", "updatedDate", "tags", "draft"];

/// 一份 Markdown 文件的拆分结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedMarkdown {
    /// front matter 正文（不含上下围栏）。
    pub front_matter: String,
    /// 正文部分（含围栏后换行，原样保留）。
    pub body: String,
    /// 围栏与正文之间的原始换行序列（`\n` 或 `\r\n`）。
    pub newline: String,
    /// front matter 区内的行分隔串（各行以该串连接）。
    pub fm_newline: String,
}

impl ParsedMarkdown {
    /// 还原为完整文件文本。
    pub fn render(&self) -> String {
        let n = &self.newline;
        let mut out = String::with_capacity(self.front_matter.len() + self.body.len() + 16);
        out.push_str(FENCE);
        out.push_str(n);
        out.push_str(&self.front_matter);
        out.push_str(n);
        out.push_str(FENCE);
        out.push_str(n);
        out.push_str(&self.body);
        out
    }

    /// 用新的 front matter 正文替换后还原完整文本。
    ///
    /// `front_matter` 内部已使用 `fm_newline` 分隔；这里只补围栏。
    pub fn render_with_front_matter(&self, front_matter: &str) -> String {
        self.render_with(front_matter, &self.body)
    }

    /// 同时替换 front matter 与正文，其余（换行风格）保持文件原样。
    pub fn render_with(&self, front_matter: &str, body: &str) -> String {
        let n = &self.newline;
        let mut out = String::with_capacity(front_matter.len() + body.len() + 16);
        out.push_str(FENCE);
        out.push_str(n);
        out.push_str(front_matter);
        out.push_str(n);
        out.push_str(FENCE);
        out.push_str(n);
        out.push_str(body);
        out
    }
}

/// 检测文本使用的主换行序列。默认 `\n`。
fn detect_newline(text: &str) -> String {
    match text.find('\n') {
        Some(idx) if idx > 0 && text.as_bytes()[idx - 1] == b'\r' => "\r\n".to_string(),
        _ => "\n".to_string(),
    }
}

fn strip_trailing_cr(line: &str) -> &str {
    line.strip_suffix('\r').unwrap_or(line)
}

/// 拆分 Markdown 文本为 front matter 与正文。
///
/// 文件必须以 `---` 开头；缺少结束围栏时报 [`ErrorCode::FrontMatterUnterminated`]。
pub fn parse_markdown(text: &str) -> Result<ParsedMarkdown> {
    let newline = detect_newline(text);
    // 第一行必须是围栏。
    let first_line_end = text.find('\n').map(|i| i + 1).unwrap_or(text.len());
    let first_line = strip_trailing_cr(&text[..first_line_end.min(text.len())]).trim_end_matches('\n');
    if first_line.trim() != FENCE {
        return Err(WriterError::new(
            ErrorCode::FrontMatterMissing,
            "文件未以 --- 包围的 front matter 开头，无法安全编辑",
        )
        .with_detail("第 1 行"));
    }

    // 逐行扫描，寻找闭合围栏（支持 CRLF）。
    let rest = &text[first_line_end..];
    let mut offset = 0usize;
    let mut fm_end: Option<usize> = None;
    let mut body_start: Option<usize> = None;
    while offset <= rest.len() {
        let line_end = match rest[offset..].find('\n') {
            Some(i) => offset + i + 1,
            None => rest.len(),
        };
        let raw = &rest[offset..line_end];
        let stripped = strip_trailing_cr(raw);
        let content = stripped.trim_end_matches('\n');
        if content.trim() == FENCE {
            fm_end = Some(offset);
            body_start = Some(line_end);
            break;
        }
        if line_end == rest.len() {
            break;
        }
        offset = line_end;
    }

    let (fm_end, body_start) = match (fm_end, body_start) {
        (Some(a), Some(b)) => (a, b),
        _ => {
            return Err(WriterError::new(
                ErrorCode::FrontMatterUnterminated,
                "front matter 缺少结束的 --- 围栏",
            ))
        }
    };

    let fm_raw = &rest[..fm_end];
    let fm_newline = detect_newline(fm_raw);
    // 去掉末尾的换行，front matter 正文本身不含结尾空行。
    let fm_trimmed = fm_raw
        .strip_suffix("\r\n")
        .or_else(|| fm_raw.strip_suffix('\n'))
        .unwrap_or(fm_raw)
        .to_string();

    Ok(ParsedMarkdown {
        front_matter: fm_trimmed,
        body: rest[body_start..].to_string(),
        newline,
        fm_newline,
    })
}

/// 解析 front matter 为有序 JSON 对象。字段类型只做 YAML 层面的还原。
pub fn parse_front_matter_map(front_matter: &str) -> Result<Map<String, Value>> {
    if front_matter.trim().is_empty() {
        return Ok(Map::new());
    }
    let yaml_value: yaml_serde::Value = yaml_serde::from_str(front_matter).map_err(|err| {
        let location = err.location().map(|l| format!("第 {} 行", l.line())).unwrap_or_default();
        let mut e = WriterError::new(
            ErrorCode::FrontMatterInvalid,
            "front matter 的 YAML 语法有误，已在源码模式保留原文",
        );
        e.detail = Some(if location.is_empty() { err.to_string() } else { location });
        e
    })?;
    // 经 serde 中转以复用成熟的映射类型；`preserve_order` 保证字段顺序。
    let json: Value = serde_json::to_value(yaml_value).map_err(|_| {
        WriterError::new(ErrorCode::FrontMatterInvalid, "front matter 无法转换为可编辑结构")
    })?;
    match json {
        Value::Object(map) => Ok(map),
        Value::Null => Ok(Map::new()),
        _ => Err(WriterError::new(
            ErrorCode::FrontMatterInvalid,
            "front matter 必须是键值映射",
        )),
    }
}

/// 从有序字段映射提取站点元数据。缺少必填字段或类型不符时报错。
pub fn meta_from_map(map: &Map<String, Value>) -> Result<ArticleMeta> {
    let require_str = |key: &str| -> Result<String> {
        map.get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| missing(key))
    };

    let title = require_str("title")?;
    let description = require_str("description")?;

    let pub_date = match map.get("pubDate") {
        Some(Value::String(s)) => s.clone(),
        Some(other) => scalar_to_date_string(other).ok_or_else(|| invalid_field("pubDate"))?,
        None => return Err(missing("pubDate")),
    };
    paths::validate_calendar_date(&pub_date).map_err(|_| invalid_field("pubDate"))?;

    let updated_date = match map.get("updatedDate") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if s.trim().is_empty() => None,
        Some(Value::String(s)) => {
            paths::validate_calendar_date(s).map_err(|_| invalid_field("updatedDate"))?;
            Some(s.clone())
        }
        Some(other) => {
            let s = scalar_to_date_string(other).ok_or_else(|| invalid_field("updatedDate"))?;
            paths::validate_calendar_date(&s).map_err(|_| invalid_field("updatedDate"))?;
            Some(s)
        }
    };

    let tags = match map.get("tags") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => {
            let mut out = Vec::with_capacity(items.len());
            let mut seen = HashSet::new();
            for item in items {
                let tag = item
                    .as_str()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| invalid_field("tags"))?;
                if seen.insert(tag.to_string()) {
                    out.push(tag.to_string());
                }
            }
            out
        }
        Some(_) => return Err(invalid_field("tags")),
    };

    let draft = match map.get("draft") {
        None => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err(invalid_field("draft")),
    };

    Ok(ArticleMeta { title, description, pub_date, updated_date, tags, draft })
}

/// YAML 会把未加引号的 `2026-09-23` 解析为日期标量；`yaml_serde` 经 serde 后
/// 可能变成字符串或带时间戳的字符串，这里统一还原为 `YYYY-MM-DD`。
fn scalar_to_date_string(value: &Value) -> Option<String> {
    let raw = match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => return None,
    };
    let trimmed = raw.trim();
    // 兼容 `2026-09-23T00:00:00Z` 之类的完整时间戳。
    let head = trimmed.split(['T', ' ']).next().unwrap_or(trimmed);
    if paths::validate_calendar_date(head).is_ok() {
        Some(head.to_string())
    } else {
        None
    }
}

fn missing(field: &str) -> WriterError {
    WriterError::new(ErrorCode::MetaFieldInvalid, format!("缺少必需字段 `{field}`"))
        .with_detail(field.to_string())
}

fn invalid_field(field: &str) -> WriterError {
    WriterError::new(ErrorCode::MetaFieldInvalid, format!("字段 `{field}` 的值不符合站点要求"))
        .with_detail(field.to_string())
}

fn render_scalar(raw: &str) -> String {
    // front matter 中站点字段都是短文本，统一加双引号最稳妥；
    // 只转义反斜杠与双引号，保留中文原样。
    let escaped = raw.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

/// 提取以 `key:` 开头的顶层行索引。
///
/// 同时接受 YAML 允许的「冒号前有空白」写法（`key : value`），否则这类合法
/// front matter 会被当成「没有该字段」而追加出重复键。
fn find_top_level_line(lines: &[String], key: &str) -> Option<usize> {
    lines.iter().position(|line| is_top_level_key(line, key))
}

/// 判断某一行是否为指定顶层键的开头。
fn is_top_level_key(line: &str, key: &str) -> bool {
    if line.starts_with(' ') || line.starts_with('\t') || line.trim_start().starts_with('#') {
        return false;
    }
    // `title : x` 与 `title: x` 都是合法 YAML；`titles:` 不是。
    match line.trim_start().strip_prefix(key) {
        Some(rest) => rest.trim_start().starts_with(':'),
        None => false,
    }
}

/// 判断顶层行是否为序列条目（零缩进 `- ` 开头）。`-x` 是普通标量，不算。
fn is_sequence_item(line: &str) -> bool {
    let trimmed = line.trim_end();
    trimmed == "-" || trimmed.starts_with("- ")
}

/// 判断 `[...]` / `{...}` 是否已配平（忽略引号内的括号）。
fn brackets_balanced(text: &str) -> bool {
    let mut depth: i32 = 0;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for ch in text.chars() {
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if ch == '\\' && q == '"' {
                escaped = true;
            } else if ch == q {
                quote = None;
            }
            continue;
        }
        match ch {
            '"' | '\'' => quote = Some(ch),
            '[' | '{' => depth += 1,
            ']' | '}' => depth -= 1,
            _ => {}
        }
    }
    depth <= 0
}

/// 判断顶层键值是否为块标量（`|`、`>` 及其变体）。
fn is_block_scalar(value: &str) -> bool {
    let v = value.trim();
    v.starts_with('|') || v.starts_with('>')
}

/// 找出某个顶层键的完整占用行区间。
///
/// 单行键值占 1 行。以下多行写法都属于同一字段，必须整体替换，否则会残留
/// 孤立行、产出不可解析的 YAML：
/// - 块标量（`|`/`>`）及其缩进续行；
/// - `key:` 后换行书写的缩进序列条目；
/// - `key:` 后换行书写的**零缩进**序列条目（`- a`，常见于生成器输出）；
/// - 跨行书写的 flow 集合（`key: [` … `]`）。
fn field_line_range(lines: &[String], index: usize) -> usize {
    let line = &lines[index];
    let value = line.split_once(':').map(|(_, v)| v).unwrap_or("");
    let trimmed = value.trim();

    // 跨行 flow 集合：消费到括号配平为止。
    if trimmed.starts_with('[') || trimmed.starts_with('{') {
        if brackets_balanced(trimmed) {
            return 1;
        }
        let mut count = 1usize;
        let mut acc = trimmed.to_string();
        while index + count < lines.len() && !brackets_balanced(&acc) {
            acc.push(' ');
            acc.push_str(lines[index + count].trim());
            count += 1;
        }
        return count;
    }

    let is_block = is_block_scalar(value);
    let is_empty_value = trimmed.is_empty();
    if !is_block && !is_empty_value {
        return 1;
    }

    // 块标量的续行必须缩进；空值后的序列条目可以是缩进或零缩进。
    let accepts = |candidate: &str| {
        if is_indented(candidate) {
            true
        } else {
            !is_block && is_sequence_item(candidate)
        }
    };

    let mut count = 1usize;
    while index + count < lines.len() {
        let next = &lines[index + count];
        if next.trim().is_empty() {
            // 空行可能是块的一部分，继续向后看是否仍有同字段内容。
            if index + count + 1 < lines.len() && accepts(&lines[index + count + 1]) {
                count += 1;
                continue;
            }
            break;
        }
        if accepts(next) {
            count += 1;
        } else {
            break;
        }
    }
    count
}

fn is_indented(line: &str) -> bool {
    line.starts_with(' ') || line.starts_with('\t')
}

/// 把标签列表渲染为块序列的多行文本（不含首行 `tags:`）。
fn render_tags_lines(tags: &[String], indent: &str) -> Vec<String> {
    tags.iter().map(|tag| format!("{indent}- {}", render_scalar(tag))).collect()
}

/// 在保留未知字段与顺序的前提下，用受控字段更新 front matter。
///
/// - 已存在的已知字段就地替换；
/// - 不存在但需要写入的字段追加到末尾；
/// - `updatedDate` 传 `None` 表示「本次不改动该字段」，而非删除。
///
/// **值未变化的字段整行保持原样**，只改真正变化的字段。这一点很重要：若把
/// `tags: [示例]` 无条件重写成块序列、把 `title: 无引号` 重写成带引号形式，
/// 即使作者没改任何内容，规范化后的网站哈希也会变，导致已发布文章被误判为
/// 「网站仍是旧版」；同时这样还能保留作者写在字段后的注释与原始引号形式。
pub fn apply_meta(
    front_matter: &str,
    newline: &str,
    meta: &ArticleMeta,
    updated_date_write: Option<Option<&str>>,
) -> String {
    let mut lines: Vec<String> =
        front_matter.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l).to_string()).collect();
    if lines.len() == 1 && lines[0].is_empty() {
        lines.clear();
    }

    // 当前 front matter 中已解析出的值，用于判断某字段是否真的需要改写。
    // 解析失败（例如字段缺失）时为 `None`，此时退回「一律写入」的保守行为。
    let current = parse_front_matter_map(front_matter)
        .ok()
        .and_then(|map| meta_from_map(&map).ok());

    let unchanged = |field: &str| -> bool {
        let Some(existing) = current.as_ref() else {
            return false;
        };
        match field {
            "title" => existing.title == meta.title,
            "description" => existing.description == meta.description,
            "pubDate" => existing.pub_date == meta.pub_date,
            "tags" => existing.tags == meta.tags,
            "draft" => existing.draft == meta.draft,
            _ => false,
        }
    };

    let assignments: Vec<(&str, String)> = vec![
        ("title", render_scalar(&meta.title)),
        ("description", render_scalar(&meta.description)),
        ("pubDate", render_scalar(&meta.pub_date)),
    ];

    for (key, rendered) in &assignments {
        if unchanged(key) {
            continue;
        }
        match find_top_level_line(&lines, key) {
            Some(idx) => {
                let span = field_line_range(&lines, idx);
                lines.splice(idx..idx + span, std::iter::once(format!("{key}: {rendered}")));
            }
            None => lines.push(format!("{key}: {rendered}")),
        }
    }

    // updatedDate：仅当调用方明确要求写入时才动。
    if let Some(action) = updated_date_write {
        match find_top_level_line(&lines, "updatedDate") {
            Some(idx) => match action {
                Some(value) => {
                    let span = field_line_range(&lines, idx);
                    let rendered = render_scalar(value);
                    lines.splice(idx..idx + span, std::iter::once(format!("updatedDate: {rendered}")));
                }
                None => {
                    let span = field_line_range(&lines, idx);
                    lines.drain(idx..idx + span);
                }
            },
            None => {
                if let Some(value) = action {
                    let rendered = render_scalar(value);
                    lines.push(format!("updatedDate: {rendered}"));
                }
            }
        }
    }

    // tags：空数组在 YAML 中写作 `[]`。值未变时整段保持原样（避免把
    // `tags: [示例]` 这种单行写法改写成块序列，从而改动网站哈希）。
    if !unchanged("tags") {
        match find_top_level_line(&lines, "tags") {
            Some(idx) => {
                let span = field_line_range(&lines, idx);
                let replacement: Vec<String> = if meta.tags.is_empty() {
                    vec!["tags: []".to_string()]
                } else {
                    let mut block = vec!["tags:".to_string()];
                    block.extend(render_tags_lines(&meta.tags, "  "));
                    block
                };
                lines.splice(idx..idx + span, replacement);
            }
            None => {
                if meta.tags.is_empty() {
                    lines.push("tags: []".to_string());
                } else {
                    lines.push("tags:".to_string());
                    lines.extend(render_tags_lines(&meta.tags, "  "));
                }
            }
        }
    }

    if !unchanged("draft") {
        match find_top_level_line(&lines, "draft") {
            Some(idx) => {
                let span = field_line_range(&lines, idx);
                lines.splice(idx..idx + span, std::iter::once(format!("draft: {}", meta.draft)));
            }
            None => lines.push(format!("draft: {}", meta.draft)),
        }
    }

    lines.join(newline)
}

/// 当前进程内唯一的临时文件序号，避免同目录并发写盘互相覆盖。
static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// 原子写盘：先写同目录临时文件，`fsync` 后替换目标。
///
/// 临时文件与目标同目录，保证 rename 在同一卷上原子完成。
pub fn atomic_write(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path.parent().ok_or_else(|| {
        WriterError::new(ErrorCode::IoFailed, "目标路径没有父目录，无法原子写入")
    })?;
    std::fs::create_dir_all(parent)
        .map_err(|e| io_error("无法创建目录", e))?;

    let seq = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("article.md");
    let tmp = parent.join(format!(".{file_name}.{}.{seq}.tmp", std::process::id()));

    {
        use std::io::Write;
        let mut file = std::fs::File::create(&tmp).map_err(|e| io_error("无法创建临时文件", e))?;
        file.write_all(contents).map_err(|e| io_error("写入临时文件失败", e))?;
        file.flush().map_err(|e| io_error("刷新临时文件失败", e))?;
        file.sync_all().map_err(|e| io_error("同步临时文件到磁盘失败", e))?;
    }

    // Windows 上 `rename` 覆盖已存在文件是允许的。
    if let Err(err) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(io_error("替换目标文件失败", err));
    }
    Ok(())
}

fn io_error(context: &str, err: std::io::Error) -> WriterError {
    let hint = match err.kind() {
        std::io::ErrorKind::PermissionDenied => "（权限不足）",
        std::io::ErrorKind::WriteZero | std::io::ErrorKind::StorageFull => "（磁盘空间不足）",
        _ => "",
    };
    WriterError::new(ErrorCode::IoFailed, format!("{context}{hint}"))
}

/// 读取一份 Markdown 文件并拆分 front matter。
pub fn read_markdown(path: &Path) -> Result<(ParsedMarkdown, ArticleMeta)> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            WriterError::new(ErrorCode::ArticleNotFound, "文章文件不存在")
        } else {
            io_error("读取文章失败", e)
        }
    })?;
    let parsed = parse_markdown(&text)?;
    let map = parse_front_matter_map(&parsed.front_matter)?;
    let meta = meta_from_map(&map)?;
    Ok((parsed, meta))
}

/// 组合一份完整 Markdown 文件文本。
pub fn compose_markdown(meta: &ArticleMeta, body: &str) -> String {
    let fm = apply_meta("", "\n", meta, None);
    let mut out = String::with_capacity(fm.len() + body.len() + 16);
    out.push_str(FENCE);
    out.push('\n');
    out.push_str(&fm);
    out.push('\n');
    out.push_str(FENCE);
    out.push('\n');
    out.push_str(body);
    out
}

/// 生成「面向网站」的规范化文本：`draft` 统一为 `false`，其余字节不动。
///
/// 本地工作稿是 `draft: true`，而 `main` 上发布版是 `draft: false`；只差这一个
/// 字段。比较网站版本时必须忽略该差异，否则刚发布的文章会被误判为「仍是旧版」。
///
/// 实现刻意只在**行级**改写 `draft:`，不做字段重建：这样对 `draft: false` 的
/// 文本是恒等变换（保证两侧结果可比），也不会顺手补出 `tags` 等新字段。
pub fn normalize_for_site(text: &str) -> Result<String> {
    let parsed = parse_markdown(text)?;
    let newline = parsed.fm_newline.clone();
    let mut lines: Vec<String> =
        parsed.front_matter.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l).to_string()).collect();

    match find_top_level_line(&lines, "draft") {
        Some(idx) => {
            let span = field_line_range(&lines, idx);
            lines.splice(idx..idx + span, std::iter::once("draft: false".to_string()));
        }
        None => return Ok(text.to_string()),
    }

    let front_matter = lines.join(&newline);
    Ok(parsed.render_with(&front_matter, &parsed.body))
}

/// 已知字段名列表（供 UI 展示与测试断言）。
pub fn known_fields() -> &'static [&'static str] {
    &KNOWN_FIELDS
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "---\ntitle: \"示例\"\ndescription: \"说明\"\npubDate: \"2026-09-23\"\ntags: [示例, 阅读]\ndraft: false\n---\n\n正文第一段。\n\n## 小标题\n";

    #[test]
    fn parses_front_matter_and_body() {
        let parsed = parse_markdown(SAMPLE).unwrap();
        assert!(parsed.front_matter.contains("title: \"示例\""));
        assert!(parsed.body.starts_with("\n正文第一段。"));
        assert_eq!(parsed.newline, "\n");
        assert_eq!(parsed.render(), SAMPLE);
    }

    #[test]
    fn parses_crlf_without_normalizing() {
        let crlf = SAMPLE.replace('\n', "\r\n");
        let parsed = parse_markdown(&crlf).unwrap();
        assert_eq!(parsed.newline, "\r\n");
        assert_eq!(parsed.render(), crlf);
    }

    #[test]
    fn rejects_missing_fences() {
        let err = parse_markdown("没有 front matter 的正文").unwrap_err();
        assert_eq!(err.code, ErrorCode::FrontMatterMissing);

        let err = parse_markdown("---\ntitle: x\n").unwrap_err();
        assert_eq!(err.code, ErrorCode::FrontMatterUnterminated);
    }

    #[test]
    fn extracts_site_meta() {
        let parsed = parse_markdown(SAMPLE).unwrap();
        let map = parse_front_matter_map(&parsed.front_matter).unwrap();
        let meta = meta_from_map(&map).unwrap();
        assert_eq!(meta.title, "示例");
        assert_eq!(meta.pub_date, "2026-09-23");
        assert_eq!(meta.tags, vec!["示例", "阅读"]);
        assert!(!meta.draft);
        assert!(meta.updated_date.is_none());
    }

    #[test]
    fn rejects_invalid_yaml_and_reports_line() {
        let bad = "---\ntitle: \"未闭合\ndescription: x\n---\n正文";
        let parsed = parse_markdown(bad).unwrap();
        let err = parse_front_matter_map(&parsed.front_matter).unwrap_err();
        assert_eq!(err.code, ErrorCode::FrontMatterInvalid);
        assert!(err.detail.is_some());
    }

    #[test]
    fn preserves_unknown_fields_and_order() {
        let src = "---\ntitle: \"旧标题\"\ncustom: keep-me\nnested:\n  a: 1\n  b: 2\ndescription: \"说明\"\npubDate: \"2026-01-02\"\ntags: [a]\ndraft: false\n---\n正文\n";
        let parsed = parse_markdown(src).unwrap();
        let mut meta = {
            let map = parse_front_matter_map(&parsed.front_matter).unwrap();
            meta_from_map(&map).unwrap()
        };
        meta.title = "新标题".to_string();

        let updated = apply_meta(&parsed.front_matter, &parsed.newline, &meta, None);
        let rendered = parsed.render_with_front_matter(&updated);

        // 未知字段与嵌套结构原样保留。
        assert!(rendered.contains("custom: keep-me"));
        assert!(rendered.contains("nested:\n  a: 1\n  b: 2"));
        // 标题已更新，位置仍在 custom 之前（顺序保持）。
        assert!(rendered.contains("title: \"新标题\""));
        let title_pos = rendered.find("title:").unwrap();
        let custom_pos = rendered.find("custom:").unwrap();
        assert!(title_pos < custom_pos, "字段顺序应保持");
        // 正文未被改动。
        assert!(rendered.ends_with("---\n正文\n"));
    }

    #[test]
    fn preserves_other_unknown_fields_after_second_save() {
        let src = "---\ntitle: \"T\"\nkeep: 1\ndescription: \"D\"\npubDate: \"2026-01-02\"\nslug-extra: yes\n---\n正文";
        let parsed = parse_markdown(src).unwrap();
        let mut meta = {
            let map = parse_front_matter_map(&parsed.front_matter).unwrap();
            meta_from_map(&map).unwrap()
        };
        meta.description = "D2".to_string();
        let once = apply_meta(&parsed.front_matter, &parsed.newline, &meta, None);
        let p2 = parse_markdown(&parsed.render_with_front_matter(&once)).unwrap();
        let meta2 = {
            let map = parse_front_matter_map(&p2.front_matter).unwrap();
            meta_from_map(&map).unwrap()
        };
        let twice = apply_meta(&p2.front_matter, &p2.newline, &meta2, None);
        assert!(twice.contains("keep: 1"));
        assert!(twice.contains("slug-extra: yes"));
        assert_eq!(once, twice, "二次保存不得增删无关数据");
    }

    #[test]
    fn updates_tags_between_forms() {
        let src = "---\ntitle: \"T\"\ndescription: \"D\"\npubDate: \"2026-01-02\"\ntags: [a, b]\ndraft: false\n---\n正文";
        let parsed = parse_markdown(src).unwrap();
        let mut meta = {
            let map = parse_front_matter_map(&parsed.front_matter).unwrap();
            meta_from_map(&map).unwrap()
        };
        meta.tags = vec!["乙".to_string(), "甲".to_string()];
        let out = apply_meta(&parsed.front_matter, &parsed.newline, &meta, None);
        assert!(out.contains("tags:\n  - \"乙\"\n  - \"甲\""));

        // 再解析回来应得到同样顺序的标签。
        let map = parse_front_matter_map(&out).unwrap();
        let meta2 = meta_from_map(&map).unwrap();
        assert_eq!(meta2.tags, vec!["乙", "甲"]);

        // 清空标签写回 `[]`。
        let mut meta3 = meta2;
        meta3.tags = Vec::new();
        let out3 = apply_meta(&out, &parsed.newline, &meta3, None);
        assert!(out3.contains("tags: []"));
    }

    /// P0-3 回归：**值未变化**的受控字段不得被改写形式。
    ///
    /// 旧实现无条件把 `tags: [a, b]` 重写成块序列、给 `title: 无引号` 补引号，
    /// 于是作者什么都没改的一次普通保存也会改变「面向网站」的规范化哈希，
    /// 让已发布文章被误判为「网站仍是旧版」。
    #[test]
    fn unchanged_fields_are_not_rewritten() {
        // 单行 flow 序列 + 无引号标量 + 字段后注释：都是站点现有文章的写法。
        let src = "---\ntitle: 用一篇记录开始\ndescription: 一篇演示\npubDate: 2026-09-26\ntags: [示例, 方法]  # 标签\ndraft: false\n---\n\n正文\n";
        let parsed = parse_markdown(src).unwrap();
        let map = parse_front_matter_map(&parsed.front_matter).unwrap();
        let meta = meta_from_map(&map).unwrap();

        // 用完全相同的元数据「保存」一次。
        let out = apply_meta(&parsed.front_matter, &parsed.newline, &meta, None);
        assert_eq!(out, parsed.front_matter, "元数据未变化时 front matter 必须逐字节保持原样");
        assert!(out.contains("tags: [示例, 方法]"), "单行 flow 序列不得被改写成块序列：{out}");
        assert!(out.contains("# 标签"), "字段后的注释不得被丢弃：{out}");
    }

    /// P0-3 回归：零缩进序列、多行 flow 序列、`key :` 三种写法都必须被正确
    /// 替换为可解析的 YAML，不得残留孤立行或产生重复键。
    #[test]
    fn multiline_field_forms_are_replaced_without_residue() {
        // 零缩进序列。
        let zero = "---\ntitle: \"T\"\ndescription: \"D\"\npubDate: \"2026-09-26\"\ntags:\n- 示例\n- 阅读\ndraft: true\n---\n\n正文\n";
        let parsed = parse_markdown(zero).unwrap();
        let map = parse_front_matter_map(&parsed.front_matter).unwrap();
        let mut meta = meta_from_map(&map).unwrap();
        assert_eq!(meta.tags, vec!["示例", "阅读"], "零缩进序列必须能被解析");
        meta.tags = vec!["新".to_string()];
        let out = apply_meta(&parsed.front_matter, &parsed.newline, &meta, None);
        assert!(!out.contains("- 示例") && !out.contains("- 阅读"), "旧条目不得残留：{out}");
        let reparsed = meta_from_map(&parse_front_matter_map(&out).unwrap()).unwrap();
        assert_eq!(reparsed.tags, vec!["新"]);

        // 多行 flow 序列。
        let flow = "---\ntitle: \"T\"\ndescription: \"D\"\npubDate: \"2026-09-26\"\ntags: [\n  示例,\n  阅读\n]\ndraft: true\n---\n\n正文\n";
        let parsed = parse_markdown(flow).unwrap();
        let map = parse_front_matter_map(&parsed.front_matter).unwrap();
        let mut meta = meta_from_map(&map).unwrap();
        assert_eq!(meta.tags, vec!["示例", "阅读"], "多行 flow 序列必须能被解析");
        meta.tags = vec!["新".to_string()];
        let out = apply_meta(&parsed.front_matter, &parsed.newline, &meta, None);
        assert!(!out.contains("  示例,"), "旧 flow 续行不得残留：{out}");
        assert!(!out.contains("]"), "旧 flow 的闭括号不得残留：{out}");
        let reparsed = meta_from_map(&parse_front_matter_map(&out).unwrap()).unwrap();
        assert_eq!(reparsed.tags, vec!["新"]);

        // `title :`（冒号前有空格）不得被当成「没有 title」而追加重复键。
        let spaced = "---\ntitle : \"空格\"\ndescription: \"D\"\npubDate: \"2026-09-26\"\ntags: []\ndraft: true\n---\n\n正文\n";
        let parsed = parse_markdown(spaced).unwrap();
        let map = parse_front_matter_map(&parsed.front_matter).unwrap();
        let mut meta = meta_from_map(&map).unwrap();
        meta.title = "改后".to_string();
        let out = apply_meta(&parsed.front_matter, &parsed.newline, &meta, None);
        assert_eq!(out.matches("title").count(), 1, "不得出现重复 title 键：{out}");
        let reparsed = meta_from_map(&parse_front_matter_map(&out).unwrap()).unwrap();
        assert_eq!(reparsed.title, "改后");
    }

    /// 恰好一个 `[` 开头但不以 `]` 结尾的普通标量不应被误判为跨行 flow。
    #[test]
    fn flow_balance_helper_handles_scalars() {
        assert!(brackets_balanced("[a, b]"));
        assert!(brackets_balanced("plain text"));
        // 引号内的 `]` 不闭合外层方括号，因此这个 flow 仍未配平。
        assert!(!brackets_balanced("[a, \"b]\""));
        // 引号内 `]` 之后仍有真正的闭合括号时才算配平。
        assert!(brackets_balanced("[a, \"b]\"]"));
        assert!(!brackets_balanced("[a,"));
        assert!(!brackets_balanced("{\"k\":"));
    }

    #[test]
    fn block_scalar_fields_survive_edit() {
        let src = "---\ntitle: \"T\"\ndescription: |\n  第一行\n  第二行\npubDate: \"2026-01-02\"\n---\n正文";
        let parsed = parse_markdown(src).unwrap();
        let meta = {
            let map = parse_front_matter_map(&parsed.front_matter).unwrap();
            meta_from_map(&map).unwrap()
        };
        assert_eq!(meta.description, "第一行\n第二行\n");

        // 只改标题：块标量 description 的值未变，必须**原样保留**，
        // 既不残留孤立缩进行，也不被强行改写成单行引号形式。
        let mut changed = meta.clone();
        changed.title = "T2".to_string();
        let out = apply_meta(&parsed.front_matter, &parsed.newline, &changed, None);
        assert!(out.contains("title: \"T2\""));
        assert!(out.contains("description: |"), "未变化的块标量应原样保留：{out}");
        assert!(out.contains("  第一行") && out.contains("  第二行"), "块内容不得丢失：{out}");
        let map = parse_front_matter_map(&out).unwrap();
        assert_eq!(meta_from_map(&map).unwrap(), changed, "二次解析必须得到相同元数据");

        // 若确实修改了 description，则整段被受控替换，不得残留旧缩进行。
        let mut retitled = meta.clone();
        retitled.description = "新的说明".to_string();
        let replaced = apply_meta(&parsed.front_matter, &parsed.newline, &retitled, None);
        assert!(replaced.contains("description: \"新的说明\""));
        assert!(!replaced.contains("  第二行"), "改写时块内容应随字段一并替换：{replaced}");
    }

    #[test]
    fn updated_date_only_written_when_asked() {
        let src = "---\ntitle: \"T\"\ndescription: \"D\"\npubDate: \"2026-01-02\"\n---\n正文";
        let parsed = parse_markdown(src).unwrap();
        let meta = {
            let map = parse_front_matter_map(&parsed.front_matter).unwrap();
            meta_from_map(&map).unwrap()
        };

        // 不请求写入 → 不出现该字段。
        let untouched = apply_meta(&parsed.front_matter, &parsed.newline, &meta, None);
        assert!(!untouched.contains("updatedDate"));

        // 明确写入。
        let with_date =
            apply_meta(&parsed.front_matter, &parsed.newline, &meta, Some(Some("2026-03-04")));
        assert!(with_date.contains("updatedDate: \"2026-03-04\""));

        // 明确移除。
        let removed = apply_meta(&with_date, "\n", &meta, Some(None));
        assert!(!removed.contains("updatedDate"));
    }

    #[test]
    fn atomic_write_replaces_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.md");
        atomic_write(&path, "one".as_bytes()).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "one");
        atomic_write(&path, "二".as_bytes()).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "二");
        // 不留临时文件。
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty());
    }

    #[test]
    fn unquoted_date_is_accepted() {
        let src = "---\ntitle: \"T\"\ndescription: \"D\"\npubDate: 2026-09-23\ntags: []\ndraft: false\n---\n正文";
        let parsed = parse_markdown(src).unwrap();
        let map = parse_front_matter_map(&parsed.front_matter).unwrap();
        let meta = meta_from_map(&map).unwrap();
        assert_eq!(meta.pub_date, "2026-09-23");
    }

    #[test]
    fn missing_required_field_is_reported_not_defaulted() {
        let src = "---\ntitle: \"T\"\npubDate: \"2026-01-02\"\n---\n正文";
        let parsed = parse_markdown(src).unwrap();
        let map = parse_front_matter_map(&parsed.front_matter).unwrap();
        let err = meta_from_map(&map).unwrap_err();
        assert_eq!(err.code, ErrorCode::MetaFieldInvalid);
        assert_eq!(err.detail.as_deref(), Some("description"));
    }

    #[test]
    fn compose_markdown_writes_schema_fields() {
        let meta = ArticleMeta {
            title: "新文章".to_string(),
            description: "摘要".to_string(),
            pub_date: "2026-09-27".to_string(),
            updated_date: None,
            tags: vec!["学习".to_string()],
            draft: true,
        };
        let text = compose_markdown(&meta, "正文\n");
        let parsed = parse_markdown(&text).unwrap();
        let map = parse_front_matter_map(&parsed.front_matter).unwrap();
        let round = meta_from_map(&map).unwrap();
        assert_eq!(round.title, "新文章");
        assert!(round.draft, "新建文章默认 draft: true");
        assert_eq!(round.tags, vec!["学习"]);
        assert_eq!(parsed.body, "正文\n");
    }

    /// `render()` 是**规范化**重建，可能不等于原始字节。
    ///
    /// 这条性质是「状态哈希必须建在磁盘原文上」的依据：若状态层用 `render()`
    /// 而同步基线用原始字节，两者基准不一致会让已同步文章恒显示「未同步」。
    /// 目前实测会发生规范化的写法是**围栏行带尾随空格**；其余常见写法
    /// （结尾无换行、结尾多余空行、围栏后直接接正文）都是恒等变换。
    #[test]
    fn render_normalizes_away_trailing_space_on_fence() {
        let with_trailing = "---\ntitle: \"T\"\ndescription: \"D\"\npubDate: \"2026-09-26\"\ntags: []\ndraft: true\n---  \n\n正文\n";
        let parsed = parse_markdown(with_trailing).unwrap();
        assert_ne!(parsed.render(), with_trailing, "围栏尾随空格会被规范化掉");

        // 规范写法下 render() 是恒等变换（保证规范化不会无端改动内容）。
        for canonical in [
            "---\ntitle: \"T\"\ndescription: \"D\"\npubDate: \"2026-09-26\"\ntags: []\ndraft: true\n---\n\n正文\n",
            "---\ntitle: \"T\"\ndescription: \"D\"\npubDate: \"2026-09-26\"\ntags: []\ndraft: true\n---\n\n正文",
            "---\ntitle: \"T\"\ndescription: \"D\"\npubDate: \"2026-09-26\"\ntags: []\ndraft: true\n---\n正文",
            "---\r\ntitle: \"T\"\r\ndescription: \"D\"\r\npubDate: \"2026-09-26\"\r\ntags: []\r\ndraft: true\r\n---\r\n\r\n正文\r\n",
        ] {
            assert_eq!(parse_markdown(canonical).unwrap().render(), canonical);
        }
    }
}
