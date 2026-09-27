//! 通用小工具：内容哈希、时间戳、文件类型探测。
//!
//! 哈希使用 SHA-256 并以十六进制小写表示，作为「本地 / writing / main」
//! 三处版本比较的统一依据。

use crate::model::{ErrorCode, Result, WriterError};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// 计算字节内容的 SHA-256 十六进制摘要。
pub fn hash_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

/// 计算文件内容的 SHA-256 十六进制摘要。
pub fn hash_file(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            WriterError::new(ErrorCode::ArticleNotFound, "文件不存在，无法计算哈希")
        } else {
            WriterError::new(ErrorCode::IoFailed, "读取文件失败，无法计算哈希")
        }
    })?;
    Ok(hash_bytes(&bytes))
}

/// 当前时间的 Unix 秒数。
pub fn unix_seconds() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// 生成一个操作 ID：`<unix秒>-<随机十六进制>`，用于把多分支操作串起来。
pub fn new_operation_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seed = format!("{}-{}", std::process::id(), nanos);
    let digest = hash_bytes(seed.as_bytes());
    format!("op-{}-{}", unix_seconds(), &digest[..12])
}

/// `YYYY-MM-DD` 形式的本地日期。
pub fn today_local_date() -> String {
    let (y, m, d) = local_ymd();
    format!("{y:04}-{m:02}-{d:02}")
}

/// 由 Unix 秒数推算本地日期（仅用于人类可读的记录字段）。
///
/// 这里用 UTC 天数加上本机时区偏移的近似算法，避免引入日期库；
/// 站点要求的 `pubDate` 始终由用户在界面中确认，不依赖此函数。
fn local_ymd() -> (i32, u32, u32) {
    let offset_seconds = local_utc_offset_seconds();
    let total = unix_seconds() as i64 + offset_seconds;
    let days = total.div_euclid(86_400);
    civil_from_days(days)
}

#[cfg(windows)]
fn local_utc_offset_seconds() -> i64 {
    // 通过 Windows API 取得当前时区的 UTC 偏移。
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct SystemTime {
        year: u16,
        month: u16,
        day_of_week: u16,
        day: u16,
        hour: u16,
        minute: u16,
        second: u16,
        milliseconds: u16,
    }
    extern "system" {
        fn GetLocalTime(out: *mut SystemTime);
        fn GetSystemTime(out: *mut SystemTime);
    }
    fn to_seconds(t: &SystemTime) -> i64 {
        // 以日数与时分秒的差近似偏移；两侧同用一个坐标，差值即为偏移。
        (t.day as i64) * 86_400
            + (t.hour as i64) * 3600
            + (t.minute as i64) * 60
            + (t.second as i64)
    }
    unsafe {
        let mut local = SystemTime {
            year: 0,
            month: 0,
            day_of_week: 0,
            day: 0,
            hour: 0,
            minute: 0,
            second: 0,
            milliseconds: 0,
        };
        let mut utc = local;
        GetLocalTime(&mut local);
        GetSystemTime(&mut utc);
        to_seconds(&local) - to_seconds(&utc)
    }
}

#[cfg(not(windows))]
fn local_utc_offset_seconds() -> i64 {
    0
}

/// 由「1970-01-01 起的天数」换算公历年月日（Howard Hinnant 的 civil_from_days）。
fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    (year as i32, m as u32, d as u32)
}

/// 支持的图片类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageKind {
    Png,
    Jpeg,
    Gif,
    WebP,
}

impl ImageKind {
    /// 规范扩展名（小写，不含点）。
    pub fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::Gif => "gif",
            Self::WebP => "webp",
        }
    }

    pub fn mime(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Gif => "image/gif",
            Self::WebP => "image/webp",
        }
    }

    /// 由 Magic Bytes 判断类型；伪装扩展名不会被接受。
    pub fn from_magic(bytes: &[u8]) -> Option<Self> {
        if bytes.len() >= 8 && bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
            return Some(Self::Png);
        }
        if bytes.len() >= 3 && bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
            return Some(Self::Jpeg);
        }
        if bytes.len() >= 6 && (bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")) {
            return Some(Self::Gif);
        }
        if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
            return Some(Self::WebP);
        }
        None
    }
}

/// 单个图片的字节上限（10 MiB）。
pub const MAX_IMAGE_BYTES: u64 = 10 * 1024 * 1024;

/// 校验图片内容，返回其真实类型。
pub fn detect_image(bytes: &[u8]) -> Result<ImageKind> {
    if bytes.len() as u64 > MAX_IMAGE_BYTES {
        return Err(WriterError::new(
            ErrorCode::ImageTooLarge,
            format!("图片超过 {} MiB 上限", MAX_IMAGE_BYTES / 1024 / 1024),
        ));
    }
    ImageKind::from_magic(bytes).ok_or_else(|| {
        WriterError::new(
            ErrorCode::ImageUnsupported,
            "仅支持经文件头确认的 PNG、JPEG、WebP、GIF 图片",
        )
    })
}

/// 生成短内容哈希前缀，用于图片文件名去重。
pub fn short_hash(bytes: &[u8]) -> String {
    hash_bytes(bytes).chars().take(8).collect()
}

/// 把一个命令名解析为可直接启动的可执行文件路径。
///
/// Windows 上 `pnpm`、`corepack` 等是 `.cmd` 批处理垫片，而不是 `.exe`；
/// `std::process::Command` 不会自动补扩展名，也不经过 shell，因此直接
/// `Command::new("pnpm")` 会以「找不到文件」失败——这会让软件误报「缺少 pnpm」，
/// 从而永远无法启动 Astro 网站预览。
///
/// 解析顺序：
/// 1. 已含扩展名或含路径分隔符的，按原样使用（调用方明确指定）；
/// 2. 按 `PATH` 查找 `<name>.<ext>`（Windows 上依次尝试 exe/cmd/bat/com）；
/// 3. 都找不到时返回原值，交由调用方给出「未安装」提示。
pub fn resolve_program(name: &str) -> PathBuf {
    if name.contains(['/', '\\']) || Path::new(name).extension().is_some() {
        return PathBuf::from(name);
    }

    let result = executable_names(name)
        .into_iter()
        .find_map(|candidate| find_in_path(&candidate));
    result.unwrap_or_else(|| PathBuf::from(name))
}

/// 在 Windows 上需要尝试的候选可执行文件名。
#[cfg(windows)]
fn executable_names(name: &str) -> Vec<String> {
    vec![
        format!("{name}.exe"),
        format!("{name}.cmd"),
        format!("{name}.bat"),
        format!("{name}.com"),
        name.to_string(),
    ]
}

/// 其它平台上可执行文件通常没有扩展名。
#[cfg(not(windows))]
fn executable_names(name: &str) -> Vec<String> {
    vec![name.to_string()]
}

/// 在 `PATH` 中查找某个可执行文件。
fn find_in_path(file_name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(file_name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// 构造一个已按平台规则解析可执行文件的 `Command`。
pub fn program_command(name: &str) -> std::process::Command {
    std::process::Command::new(resolve_program(name))
}

/// 判断一组参数能否安全地交给 `cmd /c` 执行。
///
/// Windows 上 `mklink` 是 cmd 内建命令，只能经 `cmd /c` 调用；而 cmd 会在
/// 解析命令行时把 `&`、`|`、`<`、`>`、`^`、`%`、`(`、`)`、`!` 等当作元字符，
/// `Command` 的参数数组在这里**不能**提供保护（真正执行二次解析的是 cmd）。
/// 因此凡是拼进 `cmd /c` 的路径都必须先过这一关；不通过时调用方应回退到
/// 不经 cmd 的实现，而不是硬拼命令行。
pub fn cmd_args_are_literal(args: &[&str]) -> bool {
    const FORBIDDEN: [char; 12] = ['&', '|', '<', '>', '^', '%', '(', ')', '!', '"', '\r', '\n'];
    args.iter().all(|arg| !arg.contains(FORBIDDEN))
}

/// 把一个路径转成适合交给 `cmd /c` 的形式。
///
/// `cmd` 与 `mklink` 会把路径中**正斜杠开头**的片段当成命令行开关
/// （`src/content/blog` 会被读成 `/content`，报「无效语法」）。因此交给 cmd 的
/// 路径必须统一为反斜杠。返回 `None` 表示该路径含 cmd 元字符，不能安全传递。
pub fn cmd_path_arg(path: &Path) -> Option<String> {
    let text = path.to_string_lossy().replace('/', "\\");
    if cmd_args_are_literal(&[&text]) {
        Some(text)
    } else {
        None
    }
}

/// 递归删除目录，但**不跟随**符号链接与 Windows 目录联接。
///
/// `std::fs::remove_dir_all` 在 Windows 上遇到目录联接（junctions）时行为不可靠：
/// 可能沿链接删除**链接目标**的内容。预览副本里有指向 `node_modules` 的联接，
/// 一旦被跟随就会删掉用户真实的依赖目录。因此这里自己遍历：遇到链接只删除
/// 链接本身。
pub fn remove_dir_all_no_follow(target: &Path) -> std::io::Result<()> {
    let metadata = match std::fs::symlink_metadata(target) {
        Ok(metadata) => metadata,
        // 已经不存在：视为成功（幂等）。
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err),
    };

    if is_link_like(&metadata) {
        return remove_link(target);
    }

    if metadata.is_dir() {
        for entry in std::fs::read_dir(target)? {
            let entry = entry?;
            remove_dir_all_no_follow(&entry.path())?;
        }
        std::fs::remove_dir(target)
    } else {
        std::fs::remove_file(target)
    }
}

/// 判断一项是否是「链接类」文件系统对象：符号链接或 Windows 目录联接。
///
/// 公开供 [`crate::paths`] 的路径逃逸复核复用：受管目录里出现链接类对象时，
/// 后续读写会跟随链接落到仓库之外。
pub fn is_link_like(metadata: &std::fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        /// Windows 的 `FILE_ATTRIBUTE_REPARSE_POINT`，目录联接与符号链接都带这一位。
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        return metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0;
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// 删除一个链接本身（不触及目标）。
///
/// Windows 上目录联接必须用 `remove_dir` 删除；符号链接则可能是目录或文件，
/// 因此两个都尝试一次。
fn remove_link(target: &Path) -> std::io::Result<()> {
    match std::fs::remove_dir(target) {
        Ok(()) => Ok(()),
        Err(dir_err) => match std::fs::remove_file(target) {
            Ok(()) => Ok(()),
            // 已被删掉时视为成功。
            Err(file_err) if file_err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(file_err) => Err(std::io::Error::new(
                file_err.kind(),
                format!("无法删除目录联接 {}：{dir_err} / {file_err}", target.display()),
            )),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_are_stable_and_distinct() {
        assert_eq!(hash_bytes(b"abc"), hash_bytes(b"abc"));
        assert_ne!(hash_bytes(b"abc"), hash_bytes(b"abd"));
        assert_eq!(hash_bytes(b"").len(), 64);
    }

    #[test]
    fn detects_image_types_by_magic() {
        let png = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0];
        assert_eq!(ImageKind::from_magic(&png), Some(ImageKind::Png));
        let jpg = [0xFF, 0xD8, 0xFF, 0xE0];
        assert_eq!(ImageKind::from_magic(&jpg), Some(ImageKind::Jpeg));
        assert_eq!(ImageKind::from_magic(b"GIF89a....."), Some(ImageKind::Gif));
        let mut webp = Vec::from(*b"RIFF\0\0\0\0WEBP");
        webp.extend_from_slice(b"more");
        assert_eq!(ImageKind::from_magic(&webp), Some(ImageKind::WebP));
        assert_eq!(ImageKind::from_magic(b"<svg xmlns"), None);
        assert_eq!(ImageKind::from_magic(b"<html>"), None);
    }

    #[test]
    fn rejects_oversized_and_disguised_images() {
        let disguised = b"<html>not an image</html>";
        let err = detect_image(disguised).unwrap_err();
        assert_eq!(err.code, ErrorCode::ImageUnsupported);

        let big = vec![0u8; (MAX_IMAGE_BYTES + 1) as usize];
        let err = detect_image(&big).unwrap_err();
        assert_eq!(err.code, ErrorCode::ImageTooLarge);
    }

    #[test]
    fn operation_ids_are_unique_and_prefixed() {
        let a = new_operation_id();
        let b = new_operation_id();
        assert!(a.starts_with("op-"));
        assert_ne!(a, b);
    }

    #[test]
    fn today_date_is_well_formed() {
        let date = today_local_date();
        assert!(crate::paths::validate_calendar_date(&date).is_ok(), "{date}");
    }

    #[test]
    fn remove_dir_all_no_follow_deletes_plain_trees() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("tree");
        std::fs::create_dir_all(root.join("nested/deep")).unwrap();
        std::fs::write(root.join("a.txt"), "a").unwrap();
        std::fs::write(root.join("nested/deep/b.txt"), "b").unwrap();

        remove_dir_all_no_follow(&root).unwrap();
        assert!(!root.exists(), "整棵目录树应被删除");
        // 幂等：再次调用不报错。
        remove_dir_all_no_follow(&root).unwrap();
    }

    /// 关键安全属性：删除预览副本时**不得**跟随目录联接/符号链接。
    ///
    /// 预览副本里的 `node_modules` 是指向真实依赖目录的联接；跟随它就会删掉
    /// 用户真实的依赖。这个测试用联接/软链模拟该结构，断言目标内容完好。
    #[test]
    fn remove_dir_all_no_follow_does_not_touch_link_targets() {
        let dir = tempfile::tempdir().unwrap();
        // 「真实依赖」：绝不能被动到。
        let real_modules = dir.path().join("real-node_modules");
        std::fs::create_dir_all(real_modules.join("astro")).unwrap();
        std::fs::write(real_modules.join("astro/keep.txt"), "重要内容").unwrap();

        // 「预览副本」：内部有一个指向真实依赖的联接，外加一个普通文件。
        let worktree = dir.path().join("preview-worktree");
        std::fs::create_dir_all(worktree.join("src")).unwrap();
        std::fs::write(worktree.join("src/page.astro"), "page").unwrap();
        let link = worktree.join("node_modules");
        let linked = make_dir_link(&real_modules, &link);

        if !linked {
            eprintln!("[跳过] 本机无法创建目录联接/符号链接");
            return;
        }

        remove_dir_all_no_follow(&worktree).unwrap();

        assert!(!worktree.exists(), "预览副本应被删除");
        assert!(
            real_modules.join("astro/keep.txt").exists(),
            "链接目标绝不能被删除"
        );
        assert_eq!(
            std::fs::read_to_string(real_modules.join("astro/keep.txt")).unwrap(),
            "重要内容"
        );
    }

    /// 创建目录联接（Windows）或目录符号链接（其它平台）。
    fn make_dir_link(target: &Path, link: &Path) -> bool {
        #[cfg(windows)]
        {
            matches!(
                std::process::Command::new("cmd")
                    .args([
                        "/c",
                        "mklink",
                        "/J",
                        &link.to_string_lossy(),
                        &target.to_string_lossy(),
                    ])
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

    #[test]
    fn resolve_program_finds_platform_specific_executables() {
        // 已含扩展名或路径的输入按原样返回。
        assert_eq!(resolve_program("C:/tools/x.exe"), PathBuf::from("C:/tools/x.exe"));
        assert_eq!(resolve_program("script.cmd"), PathBuf::from("script.cmd"));

        // 无法解析时回退原值，由调用方给出「未安装」提示。
        assert_eq!(
            resolve_program("definitely-not-a-real-program-xyz"),
            PathBuf::from("definitely-not-a-real-program-xyz")
        );

        // 本机存在 git，应能被解析到真实的可执行文件路径。
        let resolved = resolve_program("git");
        assert!(
            resolved.is_file(),
            "应能把 git 解析为真实文件路径，实际：{}",
            resolved.display()
        );
    }

    #[cfg(windows)]
    #[test]
    fn resolve_program_handles_cmd_shims() {
        // pnpm 在 Windows 上是 .cmd 垫片而非 .exe；必须能被发现，
        // 否则软件会误报「未安装 pnpm」并永远无法启动网站预览。
        let resolved = resolve_program("pnpm");
        assert!(
            resolved.is_file(),
            "应能解析 pnpm（.cmd 垫片），实际：{}",
            resolved.display()
        );
        let extension = resolved
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        assert!(
            matches!(extension.as_str(), "cmd" | "exe" | "bat" | "com"),
            "解析结果应是可执行文件，实际：{}",
            resolved.display()
        );
    }

    /// `cmd /c` 的参数必须不含元字符，否则会被 cmd 二次解析成额外命令。
    #[test]
    fn cmd_literal_guard_rejects_metacharacters() {
        assert!(cmd_args_are_literal(&["C:\\ws\\preview\\node_modules", "D:\\ws\\node_modules"]));
        assert!(cmd_args_are_literal(&["中文目录/子目录"]));
        assert!(cmd_args_are_literal(&[]));
        // 这些字符会被 cmd 当作元字符（`&` 分隔命令、`%` 展开变量等）。
        for evil in [
            "a&ver",
            "a|b",
            "a>b",
            "a<b",
            "a^b",
            "a%b",
            "a(b)c",
            "a!b",
            "a\"b",
            "a\nb",
            "a\rb",
        ] {
            assert!(!cmd_args_are_literal(&["ok", evil]), "{evil:?} 必须被拒绝");
        }
    }

    /// 交给 cmd 的路径必须把正斜杠换成反斜杠：cmd 会把 `/content` 当成开关，
    /// 这会让 `src/content/blog/esc` 形式的目标路径创建联接失败。
    #[test]
    fn cmd_path_arg_normalizes_forward_slashes() {
        assert_eq!(
            cmd_path_arg(Path::new("ws/src/content/blog/esc")).as_deref(),
            Some("ws\\src\\content\\blog\\esc")
        );
        assert_eq!(cmd_path_arg(Path::new("C:\\ws\\node_modules")).as_deref(), Some("C:\\ws\\node_modules"));
        // 含 cmd 元字符的路径不能安全传递。
        assert_eq!(cmd_path_arg(Path::new("D:/a&b/c")), None);
        assert_eq!(cmd_path_arg(Path::new("D:/a%b")), None);
    }

    #[test]
    fn civil_from_days_matches_known_dates() {        // 1970-01-01 是第 0 天。
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
        // 1970-01-01 到 2026-09-27 共 20723 天（56 年含 14 个闰年 = 20454 天，再加 269 天）。
        assert_eq!(civil_from_days(20_723), (2026, 9, 27));
        // 闰年边界。
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
    }
}
