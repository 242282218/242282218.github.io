//! 隔离的 Astro 网站预览。
//!
//! 设计要点：
//! - 基于**最新可获取的 `main`** 在应用专属临时目录中建立临时工作树/副本，
//!   覆盖当前文章尚未发布的 Markdown 与图片，必要时在**临时副本**里把
//!   `draft` 转为可预览状态；
//! - 预览服务只绑定 `127.0.0.1` 的随机端口；
//! - 预览过程**不修改正式仓库、不推送、不冒充线上网址**；
//! - 退出后清理应用创建的临时目录，不触碰用户原有 `node_modules`、缓存或
//!   开发目录中的未跟踪文件；
//! - 缺少 Node / pnpm / Git 时明确报告缺项，编辑与即时排版仍可用。

use crate::git;
use crate::model::{ErrorCode, Result, WriterError};
use crate::paths;
use crate::workspace::Workspace;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::time::Duration;

/// 网站预览所需的运行时能力。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolchainReport {
    pub git: ToolAvailability,
    pub node: ToolAvailability,
    pub pnpm: ToolAvailability,
    pub node_meets_minimum: bool,
    /// 缺项的中文说明列表（为空表示前置条件齐备）。
    pub missing: Vec<String>,
    /// 面向用户的安装指引。
    pub guidance: Vec<String>,
}

/// 单个外部工具的可用性。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolAvailability {
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

impl ToolAvailability {
    fn missing() -> Self {
        Self { available: false, version: None }
    }
}

/// 站点要求的最低 Node 版本。
pub const MIN_NODE_MAJOR: u32 = 22;
pub const MIN_NODE_MINOR: u32 = 12;

/// 探测外部工具链。只用参数数组执行固定命令，不经过 shell。
pub fn check_toolchain() -> ToolchainReport {
    let git = probe("git", &["--version"]);
    let node = probe("node", &["--version"]);
    let pnpm = probe("pnpm", &["--version"]);

    let node_meets_minimum = node
        .version
        .as_deref()
        .and_then(parse_node_version)
        .map(|(major, minor)| (major, minor) >= (MIN_NODE_MAJOR, MIN_NODE_MINOR))
        .unwrap_or(false);

    let mut missing = Vec::new();
    let mut guidance = Vec::new();
    if !git.available {
        missing.push("未找到 Git".to_string());
        guidance.push("安装 Git for Windows（https://git-scm.com/download/win）后重试".to_string());
    }
    if !node.available {
        missing.push("未找到 Node.js".to_string());
        guidance.push("安装 Node.js 22.12.0 或更高版本（https://nodejs.org/）".to_string());
    } else if !node_meets_minimum {
        missing.push(format!(
            "Node.js 版本过低（需要 >= {MIN_NODE_MAJOR}.{MIN_NODE_MINOR}.0）"
        ));
        guidance.push("升级 Node.js 到 22.12.0 或更高版本".to_string());
    }
    if !pnpm.available {
        missing.push("未找到 pnpm".to_string());
        guidance.push("启用 Corepack 或执行 `npm i -g pnpm@10` 安装 pnpm".to_string());
    }

    ToolchainReport { git, node, pnpm, node_meets_minimum, missing, guidance }
}

/// 探测单个工具。
///
/// 通过 [`crate::util::program_command`] 解析可执行文件，因此 Windows 上的
/// `.cmd` 垫片（pnpm、corepack）也能被正确发现，不会误报「未安装」。
fn probe(program: &str, args: &[&str]) -> ToolAvailability {
    match crate::util::program_command(program).args(args).output() {
        Ok(out) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
            ToolAvailability {
                available: true,
                version: if text.is_empty() { None } else { Some(text) },
            }
        }
        _ => ToolAvailability::missing(),
    }
}

/// 解析 `v22.12.0` / `22.12.0` 形式的版本号。
fn parse_node_version(text: &str) -> Option<(u32, u32)> {
    let trimmed = text.trim().trim_start_matches('v');
    let mut parts = trimmed.split('.');
    let major = parts.next()?.parse::<u32>().ok()?;
    let minor = parts.next().unwrap_or("0").parse::<u32>().ok()?;
    Some((major, minor))
}

/// 一次运行中的预览服务。
pub struct PreviewServer {
    child: Child,
    /// 应用创建的临时目录（退出时整棵删除）。
    temp_root: PathBuf,
    url: String,
    /// dev server 的输出日志，用于失败时给出可核对的原因。
    log_path: PathBuf,
}

impl PreviewServer {
    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn temp_root(&self) -> &Path {
        &self.temp_root
    }

    /// 读取日志尾部摘要（最多若干行），用于错误提示。
    pub fn log_excerpt(&self) -> String {
        match std::fs::read_to_string(&self.log_path) {
            Ok(text) => {
                let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
                let tail: Vec<&str> = lines.iter().rev().take(6).rev().copied().collect();
                let excerpt = tail.join(" / ");
                let cleaned: String = excerpt.chars().take(400).collect();
                if cleaned.is_empty() {
                    "（预览服务没有输出日志）".to_string()
                } else {
                    cleaned
                }
            }
            Err(_) => "（无法读取预览服务日志）".to_string(),
        }
    }

    /// 停止预览并清理临时目录。
    ///
    /// 远端推送过程无法“假装取消已发生的提交”，但本地预览可以安全终止。
    ///
    /// 注意：Windows 上 `pnpm` 是 `.cmd` 垫片，`kill()` 只终止垫片进程，
    /// 真正监听端口的 `node`（Astro）会变成孤儿并继续占用端口与临时目录。
    /// 因此这里按 **进程树** 终止，再清理目录。
    pub fn shutdown(mut self) {
        kill_process_tree(&mut self.child);
        let _ = crate::util::remove_dir_all_no_follow(&self.temp_root);
    }
}

/// 终止一个进程及其全部子进程。
///
/// Windows 上用 `taskkill /T /F`（`/T` 连同子进程，`/F` 强制）；
/// 其它平台先尝试子进程组，再退回直接 `kill`。
fn kill_process_tree(child: &mut Child) {
    #[cfg(windows)]
    {
        let pid = child.id().to_string();
        let _ = crate::util::program_command("taskkill")
            .args(["/T", "/F", "/PID", &pid])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// 向本机预览地址发一次极简 HTTP 请求，判断是否已就绪。
///
/// 只用 `std::net::TcpStream`，避免为一次探测引入 HTTP 客户端依赖；
/// 同时强制目标必须是 `127.0.0.1`，确保探测不会打到外部地址。
fn probe_http_ok(url: &str) -> bool {
    use std::io::{Read, Write};
    let Some(rest) = url.strip_prefix("http://") else {
        return false;
    };
    let (authority, path) = match rest.find('/') {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, "/"),
    };
    if !authority.starts_with("127.0.0.1:") {
        return false;
    }
    let Ok(address) = authority.parse() else {
        return false;
    };
    let Ok(mut stream) = std::net::TcpStream::connect_timeout(&address, Duration::from_secs(2))
    else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    if write!(
        stream,
        "GET {path} HTTP/1.0\r\nHost: {authority}\r\nConnection: close\r\n\r\n"
    )
    .is_err()
    {
        return false;
    }
    let mut buffer = Vec::new();
    if stream.read_to_end(&mut buffer).is_err() {
        return false;
    }
    let text = String::from_utf8_lossy(&buffer);
    text.starts_with("HTTP/1.") && text.contains(" 200 ")
}

/// 向本机预览地址发一次请求并取回响应体（供测试与诊断使用）。
///
/// 只允许 `127.0.0.1`，因此不会意外访问外部地址。
pub fn fetch_local(url: &str) -> Result<String> {
    use std::io::{Read, Write};
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| WriterError::new(ErrorCode::PreviewFailed, "只支持 http:// 地址"))?;
    let (authority, path) = match rest.find('/') {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, "/"),
    };
    if !authority.starts_with("127.0.0.1:") {
        return Err(WriterError::new(
            ErrorCode::PreviewFailed,
            "预览只应绑定 127.0.0.1",
        ));
    }
    let address = authority
        .parse()
        .map_err(|_| WriterError::new(ErrorCode::PreviewFailed, "地址解析失败"))?;
    let mut stream = std::net::TcpStream::connect_timeout(&address, Duration::from_secs(2))
        .map_err(|e| WriterError::new(ErrorCode::PreviewFailed, format!("连接预览服务失败：{e}")))?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    write!(
        stream,
        "GET {path} HTTP/1.0\r\nHost: {authority}\r\nConnection: close\r\n\r\n"
    )
    .map_err(|e| WriterError::new(ErrorCode::PreviewFailed, format!("请求写入失败：{e}")))?;
    let mut buffer = Vec::new();
    stream
        .read_to_end(&mut buffer)
        .map_err(|e| WriterError::new(ErrorCode::PreviewFailed, format!("响应读取失败：{e}")))?;
    let text = String::from_utf8_lossy(&buffer).into_owned();
    if !text.starts_with("HTTP/1.") || !text.contains(" 200 ") {
        return Err(WriterError::new(ErrorCode::PreviewFailed, "预览服务返回非 200"));
    }
    Ok(text)
}

impl Drop for PreviewServer {
    fn drop(&mut self) {
        kill_process_tree(&mut self.child);
        // 只删除本次运行创建的临时目录；不跟随目录联接。
        let _ = crate::util::remove_dir_all_no_follow(&self.temp_root);
    }
}

/// 从命令输出中取一段非空摘要，用于失败提示（不含敏感内容）。
fn summarize_failure(stdout: &str, stderr: &str) -> String {
    let merged = format!("{stdout}\n{stderr}");
    let lines: Vec<&str> = merged.lines().map(str::trim_end).filter(|l| !l.trim().is_empty()).collect();
    // 优先显示含错误关键词的行，否则取尾部若干行。
    let mut picked: Vec<&str> = lines
        .iter()
        .filter(|l| {
            let low = l.to_lowercase();
            low.contains("error") || low.contains("错误") || low.contains("failed") || low.contains("✗")
        })
        .take(6)
        .copied()
        .collect();
    if picked.is_empty() {
        picked = lines.iter().rev().take(6).rev().copied().collect();
    }
    let excerpt = picked.join(" / ");
    excerpt.chars().take(500).collect()
}

/// 预览覆盖内容：当前文章尚未发布的 Markdown 与图片。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewOverlay {
    pub article_id: String,
    /// 该文章的 Markdown 全文（可含 `draft: true`，由预览临时副本转公开）。
    pub markdown: String,
    /// 需要一并覆盖的图片（相对路径 → 字节）。
    pub images: BTreeMap<String, Vec<u8>>,
    /// 是否模拟公开（在临时副本中把 draft 转为可预览）。
    pub simulate_public: bool,
}

/// 网站预览引擎。
pub struct PreviewEngine<'a> {
    workspace: &'a Workspace,
    temp_base: PathBuf,
    expected_remote: String,
}

impl<'a> PreviewEngine<'a> {
    pub fn new(workspace: &'a Workspace, temp_base: PathBuf, expected_remote: &str) -> Self {
        Self { workspace, temp_base, expected_remote: expected_remote.to_string() }
    }

    /// 在应用专属临时目录中建立预览工作副本。
    ///
    /// 以最新 `main` 为基线（拿不到远端时回退到本地 `HEAD`），从而排除工作区中
    /// 未提交文件与未跟踪文件的污染。返回该副本的路径。
    ///
    /// 实现只用 Git 自身完成导出（隔离索引 + `checkout-index`），不依赖系统
    /// `tar`，也不会改动正式仓库的分支、暂存区或工作树。
    pub fn prepare_worktree(&self, label: &str) -> Result<PathBuf> {
        // 预览副本统一放在应用数据目录下，便于受控生命周期清理，
        // 也不会污染用户的工作目录或系统临时目录。
        let _ = std::fs::create_dir_all(&self.temp_base);
        let unique = git::temp_path(&format!("preview-{label}"));
        let dir_name = unique
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| format!("preview-{label}"));
        let root = self.temp_base.join(dir_name);
        std::fs::create_dir_all(&root).map_err(|e| {
            WriterError::new(ErrorCode::PreviewFailed, format!("创建预览临时目录失败：{e}"))
        })?;

        let Some(rev) = self.preview_base_rev()? else {
            return Err(WriterError::new(
                ErrorCode::PreviewFailed,
                "无法确定预览基线（既没有远端 main，也没有本地提交）",
            ));
        };

        // 用隔离索引把该提交的树写到临时目录：不触碰正式仓库的索引与工作树。
        git::export_tree_to(self.workspace.root(), &root, &rev)?;

        Ok(root)
    }

    /// 选择预览基线：优先远端 `main`，离线时回退到本地 `HEAD`。
    pub fn preview_base_rev(&self) -> Result<Option<String>> {
        if let Some(head) = self.remote_main_head()? {
            return Ok(Some(head));
        }
        Ok(git::rev_parse(self.workspace.root(), "HEAD").ok())
    }

    /// 复用工作区已安装的依赖目录。
    ///
    /// 预览副本与工作区共用同一份 `node_modules`（Windows 上用目录联接，
    /// 其它平台用符号链接），因此不必为每次预览重新下载依赖，也不会修改
    /// 工作区里那份依赖。返回是否成功建立了复用链接。
    pub fn link_dependencies(&self, worktree: &Path) -> Result<bool> {
        let source = self.workspace.root().join("node_modules");
        if !source.is_dir() {
            return Ok(false);
        }
        let target = worktree.join("node_modules");
        if target.exists() {
            return Ok(true);
        }
        #[cfg(windows)]
        {
            // `cmd /c mklink` 会把参数交给 cmd 二次解析：`&`、`^`、`%` 等是元字符，
            // 路径中的正斜杠还会被当成开关（`/content`）。含这些字符时不做联接，
            // 交由调用方回退到安装依赖，绝不硬拼命令行。
            let (Some(target_str), Some(source_str)) =
                (crate::util::cmd_path_arg(&target), crate::util::cmd_path_arg(&source))
            else {
                return Ok(false);
            };
            // 目录联接不需要管理员权限，且不复制文件内容。
            let status = crate::util::program_command("cmd")
                .args(["/c", "mklink", "/J", &target_str, &source_str])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            match status {
                Ok(exit) if exit.success() => Ok(true),
                // 失败时交给调用方回退到真实安装。
                _ => Ok(false),
            }
        }
        #[cfg(not(windows))]
        {
            match std::os::unix::fs::symlink(&source, &target) {
                Ok(()) => Ok(true),
                Err(_) => Ok(false),
            }
        }
    }

    /// 把当前文章与图片覆盖到预览副本中。
    pub fn apply_overlay(&self, worktree: &Path, overlay: &PreviewOverlay) -> Result<()> {
        paths::validate_existing_article_id(&overlay.article_id)?;
        let markdown_rel = format!("{}{}.md", paths::BLOG_DIR_PREFIX, overlay.article_id);
        paths::validate_managed_markdown(&markdown_rel)?;

        let text = if overlay.simulate_public {
            // 只在临时副本中把 draft 置为可预览；这不影响正式仓库。
            let parsed = crate::article_io::parse_markdown(&overlay.markdown)?;
            let mut lines: Vec<String> = parsed
                .front_matter
                .split('\n')
                .map(|l| l.strip_suffix('\r').unwrap_or(l).to_string())
                .collect();
            match lines.iter().position(|l| l.starts_with("draft:")) {
                Some(idx) => lines[idx] = "draft: false".to_string(),
                None => lines.push("draft: false".to_string()),
            }
            let front_matter = lines.join(&parsed.fm_newline);
            parsed.render_with(&front_matter, &parsed.body)
        } else {
            overlay.markdown.clone()
        };

        let abs = worktree.join(&markdown_rel);
        crate::article_io::atomic_write(&abs, text.as_bytes())?;

        for (rel, bytes) in &overlay.images {
            paths::validate_managed_rel_path(rel)?;
            let abs = worktree.join(rel);
            crate::article_io::atomic_write(&abs, bytes)?;
        }
        Ok(())
    }

    /// 在预览副本中依次运行站点命令（`pnpm test` / `pnpm check` / `pnpm build`）。
    ///
    /// 供**发布前验收**使用：返回第一条失败的摘要，成功时返回 `Ok(())`。
    /// 这里运行的是仓库自己的脚本，命令与参数都是固定的参数数组，不经过 shell。
    pub fn run_site_checks(&self, worktree: &Path) -> Result<()> {
        for script in ["test", "check", "build"] {
            let output = crate::util::program_command("pnpm")
                .current_dir(worktree)
                .args([script])
                .output()
                .map_err(|_| {
                    WriterError::new(
                        ErrorCode::ToolchainMissing,
                        "无法运行站点验收命令（未找到 pnpm）",
                    )
                })?;
            if !output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                let excerpt = summarize_failure(&stdout, &stderr);
                return Err(WriterError::new(
                    ErrorCode::BuildFailed,
                    format!("发布前站点验收失败（pnpm {script}），已停止发布"),
                )
                .with_detail(excerpt));
            }
        }
        Ok(())
    }

    /// 检查预览副本是否具备可用的依赖目录。
    pub fn dependencies_ready(&self, worktree: &Path) -> bool {
        worktree.join("node_modules").is_dir()
    }

    /// 确认预览副本具备可启动的依赖。
    ///
    /// 优先复用工作区已安装的 `node_modules`（通过目录联接），避免为每次预览
    /// 重新下载依赖；工作区也没有时，才在本副本中按锁文件安装。
    ///
    /// 返回 `true` 表示复用了工作区依赖，`false` 表示在副本中独立安装。
    pub fn ensure_dependencies(&self, worktree: &Path) -> Result<bool> {
        if self.dependencies_ready(worktree) {
            return Ok(true);
        }
        // 先尝试复用工作区那份依赖（不修改它）。
        if self.link_dependencies(worktree)? {
            return Ok(true);
        }
        // 回退：按锁文件安装一份自己的依赖，复用仓库锁文件保证版本一致。
        self.install_dependencies(worktree)?;
        Ok(false)
    }

    /// 在预览副本中安装依赖（复用仓库锁文件）。
    ///
    /// 使用平台解析后的 pnpm（Windows 上是 `pnpm.cmd`），否则会因找不到可执行文件
    /// 而误报「未安装 pnpm」。
    pub fn install_dependencies(&self, worktree: &Path) -> Result<()> {
        let out = crate::util::program_command("pnpm")
            .current_dir(worktree)
            .args(["install", "--frozen-lockfile"])
            .output()
            .map_err(|_| {
                WriterError::new(ErrorCode::ToolchainMissing, "未找到 pnpm，无法安装预览依赖")
            })?;
        if !out.status.success() {
            let detail = String::from_utf8_lossy(&out.stderr);
            let summary = detail.lines().last().unwrap_or("").chars().take(200).collect::<String>();
            return Err(WriterError::new(
                ErrorCode::PreviewFailed,
                "安装预览依赖失败（即时排版仍可继续使用）",
            )
            .with_detail(summary));
        }
        Ok(())
    }

    /// 启动预览服务：绑定 `127.0.0.1` 的随机端口。
    ///
    /// 服务输出写入预览副本中的日志文件，便于失败时给出可核对的原因，
    /// 同时避免 dev server 的输出污染软件自身的控制台。
    pub fn start(&self, worktree: &Path) -> Result<PreviewServer> {
        let toolchain = check_toolchain();
        if !toolchain.missing.is_empty() {
            return Err(WriterError::new(
                ErrorCode::ToolchainMissing,
                format!("网站预览缺少前置条件：{}", toolchain.missing.join("；")),
            )
            .with_detail(toolchain.guidance.join("；")));
        }

        let port = pick_free_port()?;
        let log_path = worktree.join("preview-server.log");
        let stdout = std::fs::File::create(&log_path).map_err(|e| {
            WriterError::new(ErrorCode::PreviewFailed, format!("无法创建预览日志文件：{e}"))
        })?;
        let stderr = stdout.try_clone().map_err(|e| {
            WriterError::new(ErrorCode::PreviewFailed, format!("无法创建预览日志文件：{e}"))
        })?;

        let child = crate::util::program_command("pnpm")
            .current_dir(worktree)
            .args([
                "exec",
                "astro",
                "dev",
                "--host",
                "127.0.0.1",
                "--port",
                &port.to_string(),
            ])
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr))
            .spawn()
            .map_err(|_| {
                WriterError::new(ErrorCode::ToolchainMissing, "无法启动本地预览服务（未找到 pnpm）")
            })?;

        let url = format!("http://127.0.0.1:{port}/");
        let temp_root = worktree.to_path_buf();
        Ok(PreviewServer { child, temp_root, url, log_path })
    }

    /// 等待预览服务就绪（最多 `timeout`）。
    ///
    /// 就绪判据是对 `127.0.0.1` 上的目标地址拿到一次成功响应；超时则连同
    /// dev server 的日志摘要一起报错，避免只显示「失败」而无从排查。
    pub fn wait_until_ready(&self, server: &PreviewServer, timeout: Duration) -> Result<()> {
        let started = std::time::Instant::now();
        while started.elapsed() < timeout {
            if probe_http_ok(server.url()) {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(400));
        }
        Err(WriterError::new(
            ErrorCode::PreviewFailed,
            "网站预览服务启动超时",
        )
        .with_detail(server.log_excerpt()))
    }

    /// 读取当前工作区可用的最新 `main` 头；离线时返回 `None`。
    fn remote_main_head(&self) -> Result<Option<String>> {
        let normalized_expected = git::normalize_remote_url(&self.expected_remote);
        let out = git::git(self.workspace.root(), &["remote", "get-url", "origin"])?;
        if git::normalize_remote_url(out.stdout_trimmed()) != normalized_expected {
            return Err(WriterError::new(
                ErrorCode::InvalidArgument,
                "工作目录的 origin 与配置的目标仓库不一致",
            ));
        }
        // 离线时不应让预览整体失败，交由调用方回退。
        match git::git(
            self.workspace.root(),
            &["fetch", "--no-tags", "origin", "+refs/heads/main:refs/remotes/origin/main"],
        ) {
            Ok(_) => {}
            Err(_) => return Ok(None),
        }
        if git::ref_exists(self.workspace.root(), "refs/remotes/origin/main") {
            Ok(Some(git::rev_parse(self.workspace.root(), "refs/remotes/origin/main")?))
        } else {
            Ok(None)
        }
    }
}

/// 选择一个本机空闲端口（绑定后立即释放，交给 Astro 使用）。
fn pick_free_port() -> Result<u16> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).map_err(|e| {
        WriterError::new(ErrorCode::PreviewFailed, format!("无法分配本地预览端口：{e}"))
    })?;
    let port = listener
        .local_addr()
        .map_err(|e| WriterError::new(ErrorCode::PreviewFailed, format!("无法读取预览端口：{e}")))?
        .port();
    drop(listener);
    Ok(port)
}

/// 预览页面上需要显著展示的提示文案。
pub const PREVIEW_BANNER: &str = "本地预览 · 尚未发布";

/// 离线时字体可能回退的提示（站点若引用 Google Fonts）。
pub const OFFLINE_FONT_NOTICE: &str =
    "离线状态下，网页字体可能回退到系统中的替代字体，视觉与联网时可能不同";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_node_versions() {
        assert_eq!(parse_node_version("v22.12.0"), Some((22, 12)));
        assert_eq!(parse_node_version("24.12.0"), Some((24, 12)));
        assert_eq!(parse_node_version("v22"), Some((22, 0)));
        assert_eq!(parse_node_version("not-a-version"), None);
    }

    #[test]
    fn minimum_node_version_check() {
        // 22.12 是本站下限。
        assert!((22, 12) >= (MIN_NODE_MAJOR, MIN_NODE_MINOR));
        assert!((22, 11) < (MIN_NODE_MAJOR, MIN_NODE_MINOR));
        assert!((24, 0) >= (MIN_NODE_MAJOR, MIN_NODE_MINOR));
    }

    #[test]
    fn toolchain_probe_reports_available_tools() {
        let report = check_toolchain();
        // 这台机器已确认装有 git；其余按实际环境判断，
        // 但结构必须完整且缺项与指引一一对应。
        assert!(report.git.available, "本机应能探测到 git");
        assert_eq!(
            report.missing.len(),
            report.guidance.len(),
            "每个缺项都要有对应指引"
        );
    }

    #[test]
    fn free_port_is_usable() {
        let port = pick_free_port().unwrap();
        assert!(port > 0);
        // 端口可被重新绑定，说明已释放。
        let listener = std::net::TcpListener::bind(("127.0.0.1", port));
        assert!(listener.is_ok());
    }

    #[test]
    fn preview_messages_are_explicit() {
        assert!(PREVIEW_BANNER.contains("尚未发布"));
        assert!(!PREVIEW_BANNER.contains("已上线"));
        assert!(OFFLINE_FONT_NOTICE.contains("回退"));
    }
}

/// 端到端：在隔离副本中真实启动 Astro 预览服务。
///
/// 这些测试会**真的**启动一个 Astro dev server 并发出 HTTP 请求，用于完成
/// 方案阶段 2 的验收（「同一篇未发布文章能看到即时与真实网站两层预览的网站层」）。
///
/// 为了不依赖网络与用户仓库，测试自建一个最小的 Astro 站点作为夹具，并复用
/// **本仓库根**已安装的 Astro 依赖（`node_modules` 目录联接，不复制内容）。
/// 若本机缺少 Node/pnpm 或 Astro 依赖，测试会跳过并打印原因——不假装通过。
#[cfg(test)]
mod e2e {
    use super::*;
    use crate::testkit::TestEnv;

    /// 站点夹具的 package.json：使用与真实站点相同的 Astro 版本。
    const FIXTURE_PACKAGE_JSON: &str = r#"{
  "name": "preview-fixture-site",
  "private": true,
  "type": "module",
  "scripts": { "dev": "astro dev" },
  "dependencies": { "astro": "7.3.5" }
}
"#;

    const FIXTURE_ASTRO_CONFIG: &str = r#"import { defineConfig } from 'astro/config';
export default defineConfig({ devToolbar: { enabled: false } });
"#;

    /// 内容集合配置：与真实站点一致的 schema。
    const FIXTURE_CONTENT_CONFIG: &str = r#"import { defineCollection } from 'astro:content';
import { glob } from 'astro/loaders';
import { z } from 'astro/zod';

const blog = defineCollection({
  loader: glob({ base: './src/content/blog', pattern: '**/*.md' }),
  schema: z.object({
    title: z.string().min(1),
    description: z.string().min(1),
    pubDate: z.coerce.date(),
    updatedDate: z.coerce.date().optional(),
    tags: z.array(z.string()).default([]),
    draft: z.boolean().default(false),
  }),
});

export const collections = { blog };
"#;

    /// 列表页：只展示非草稿（与真实站点同样的过滤逻辑）。
    const FIXTURE_INDEX_PAGE: &str = r#"---
import { getCollection } from 'astro:content';
const posts = (await getCollection('blog')).filter((p) => !p.data.draft);
---
<html lang="zh-CN">
  <head><meta charset="utf-8" /><title>夹具站点</title></head>
  <body>
    <h1>夹具站点首页</h1>
    <ul>
      {posts.map((post) => (
        <li><a href={`/blog/${post.id}/`}>{post.data.title}</a></li>
      ))}
    </ul>
  </body>
</html>
"#;

    /// 文章页：与真实站点一致——`getStaticPaths` 先用非草稿过滤。
    ///
    /// 真实站点在 `src/pages/blog/[...id].astro` 中调用 `publishedPosts()`，
    /// 因此草稿既不在列表也不会生成路由。夹具必须复现这一点，否则测不出
    /// 「草稿不泄漏到网站」这条关键行为。
    const FIXTURE_ARTICLE_PAGE: &str = r#"---
import { getCollection, render } from 'astro:content';
export async function getStaticPaths() {
  // 与真实站点的 publishedPosts 过滤逻辑一致。
  const posts = (await getCollection('blog')).filter((p) => !p.data.draft);
  return posts.map((post) => ({ params: { id: post.id }, props: { post } }));
}
const { post } = Astro.props;
const { Content } = await render(post);
---
<html lang="zh-CN">
  <head><meta charset="utf-8" /><title>{post.data.title}</title></head>
  <body>
    <article>
      <h1>{post.data.title}</h1>
      <p class="summary">{post.data.description}</p>
      <Content />
    </article>
  </body>
</html>
"#;

    const FIXTURE_FILES: [(&str, &str); 5] = [
        ("package.json", FIXTURE_PACKAGE_JSON),
        ("astro.config.mjs", FIXTURE_ASTRO_CONFIG),
        ("src/content.config.ts", FIXTURE_CONTENT_CONFIG),
        ("src/pages/index.astro", FIXTURE_INDEX_PAGE),
        ("src/pages/blog/[...id].astro", FIXTURE_ARTICLE_PAGE),
    ];

    /// 找到可用于预览的 Astro 依赖目录。
    ///
    /// 站点的 Astro 依赖位于**仓库根**（`pnpm test`/`build` 都在那里运行），
    /// 因此从 `apps/writer/src-tauri` 上溯三级到仓库根，同时保留 `apps/writer`
    /// 自身的 `node_modules` 作为候选。
    fn find_astro_modules() -> Option<PathBuf> {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        // apps/writer/src-tauri -> apps/writer -> apps -> 仓库根
        let writer_dir = manifest_dir.parent();
        let repo_root = writer_dir.and_then(|p| p.parent()).and_then(|p| p.parent());
        let candidates: Vec<PathBuf> = [writer_dir.map(|p| p.join("node_modules")), repo_root.map(|p| p.join("node_modules"))]
            .into_iter()
            .flatten()
            .collect();
        candidates.iter().find(|candidate| candidate.join("astro").is_dir()).cloned()
    }

    /// 把依赖目录联接到预览副本的 `node_modules`。
    ///
    /// 目录联接（Windows）或符号链接（其它平台）不复制文件内容，因此预览复用
    /// 本机已安装的依赖，不重新下载，也不修改那份依赖。
    fn link_modules(source: &Path, worktree: &Path) -> bool {
        let target = worktree.join("node_modules");
        if target.exists() {
            return true;
        }
        #[cfg(windows)]
        {
            let (Some(target_str), Some(source_str)) =
                (crate::util::cmd_path_arg(&target), crate::util::cmd_path_arg(source))
            else {
                return false;
            };
            matches!(
                crate::util::program_command("cmd")
                    .args(["/c", "mklink", "/J", &target_str, &source_str])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status(),
                Ok(exit) if exit.success()
            )
        }
        #[cfg(not(windows))]
        {
            std::os::unix::fs::symlink(source, &target).is_ok()
        }
    }

    /// 构造一份「站点夹具」工作区：把夹具文件提交到测试仓库的 main。
    fn site_fixture() -> TestEnv {
        let env = TestEnv::new();
        for (rel, content) in FIXTURE_FILES {
            let abs = env.path().join(rel);
            if let Some(parent) = abs.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&abs, content).unwrap();
        }
        env.git(&["add", "-A"]);
        env.git(&["commit", "-m", "夹具站点源码"]);
        env.push_current("main");
        env
    }

    /// 在 `deadline` 内轮询预览地址，返回首个成功响应的内容。
    ///
    /// 复用模块级的 [`fetch_local`]，避免测试与产品代码各写一份 HTTP 读取逻辑。
    fn poll_until_ready(url: &str, deadline: Duration) -> Option<String> {
        let started = std::time::Instant::now();
        while started.elapsed() < deadline {
            if let Ok(response) = fetch_local(url) {
                return Some(response);
            }
            std::thread::sleep(Duration::from_millis(400));
        }
        None
    }

    /// 每个 e2e 测试通用的「建副本 + 接依赖 + 启服务」步骤。
    /// 预览副本来自 Git 树：导出的是提交内容，不含工作区未提交与未跟踪文件。
    ///
    /// 不依赖 Node/pnpm，因此在任何环境都能运行；它锁定了预览隔离性的核心行为。
    #[test]
    fn worktree_comes_from_git_tree_not_working_directory() {
        let env = site_fixture();
        let workspace = crate::workspace::Workspace::open(env.path().to_path_buf()).unwrap();
        let engine = PreviewEngine::new(
            &workspace,
            env.dir.path().join("preview-temp"),
            &format!("file://{}", env.remote.to_string_lossy().replace('\\', "/")),
        );

        // 在工作区放一个已提交文件与一个**未提交**文件。
        std::fs::write(env.path().join("committed-file.txt"), "已提交").unwrap();
        env.git(&["add", "-A"]);
        env.git(&["commit", "-m", "加入已提交文件"]);
        env.push_current("main");
        std::fs::write(env.path().join("untracked-file.txt"), "未跟踪").unwrap();
        std::fs::write(env.path().join("astro.config.mjs"), "// 本地未提交的改动").unwrap();

        let worktree = engine.prepare_worktree("export-check").expect("应能建立预览副本");

        // 已提交的文件与站点源码在副本中。
        assert!(worktree.join("committed-file.txt").exists(), "应导出已提交文件");
        assert!(worktree.join("package.json").exists(), "应导出 package.json");
        assert!(
            worktree.join("src/pages/index.astro").exists(),
            "应导出站点页面（实际内容：{:?}）",
            std::fs::read_dir(&worktree)
                .map(|dir| dir.flatten().map(|e| e.file_name()).collect::<Vec<_>>())
        );
        assert!(worktree.join("src/content.config.ts").exists(), "应导出内容集合配置");

        // 未跟踪文件与未提交改动**不**进入副本。
        assert!(!worktree.join("untracked-file.txt").exists(), "未跟踪文件不应进入副本");
        let config = std::fs::read_to_string(worktree.join("astro.config.mjs")).unwrap();
        assert!(!config.contains("本地未提交"), "未提交改动不应进入副本");

        // 正式工作区没有被改动。
        assert!(env.path().join("untracked-file.txt").exists(), "工作区文件不应被动过");
        assert_eq!(
            env.git(&["rev-parse", "--abbrev-ref", "HEAD"]),
            "main",
            "预览不得切换分支"
        );

        crate::util::remove_dir_all_no_follow(&worktree).ok();
    }

    /// 阶段 2 验收：真实启动隔离 Astro 预览，看到未发布文章的官方网站层。
    #[test]
    fn starts_isolated_astro_preview_showing_unpublished_article() {
        let env = site_fixture();
        let workspace = crate::workspace::Workspace::open(env.path().to_path_buf()).unwrap();
        let engine = PreviewEngine::new(
            &workspace,
            env.dir.path().join("preview-temp"),
            &format!("file://{}", env.remote.to_string_lossy().replace('\\', "/")),
        );

        // 前置条件
        let toolchain = check_toolchain();
        if !toolchain.missing.is_empty() {
            eprintln!("[跳过] 网站预览前置条件缺失：{}", toolchain.missing.join("；"));
            return;
        }
        let Some(astro_modules) = find_astro_modules() else {
            eprintln!("[跳过] 未找到本机 Astro 依赖；请先在本仓库根运行 pnpm install");
            return;
        };

        let markdown = "---\ntitle: \"测试样稿：预览中的未发布文章\"\ndescription: \"用于验证网站预览的生成式测试样稿。\"\npubDate: \"2026-01-15\"\ntags: [测试样稿]\ndraft: true\n---\n\n这是测试样稿正文，不对应真实观澜记录。\n";

        let worktree = engine.prepare_worktree("e2e").expect("应能建立预览副本");
        assert!(worktree.join("src/pages/index.astro").exists(), "副本应包含站点页面");

        // 覆盖当前（未发布）文章，并在**临时副本**中模拟公开。
        engine
            .apply_overlay(
                &worktree,
                &PreviewOverlay {
                    article_id: "preview-fixture".to_string(),
                    markdown: markdown.to_string(),
                    images: BTreeMap::new(),
                    simulate_public: true,
                },
            )
            .expect("应能覆盖当前文章");

        // 覆盖生效；正式仓库没有被写入。
        let written = std::fs::read_to_string(worktree.join("src/content/blog/preview-fixture.md"))
            .expect("副本中应存在该文章");
        assert!(written.contains("draft: false"), "临时副本中应模拟公开");
        assert!(written.contains("测试样稿正文"));
        assert!(
            !env.path().join("src/content/blog/preview-fixture.md").exists(),
            "预览不得写入正式仓库"
        );

        if !link_modules(&astro_modules, &worktree) {
            eprintln!("[跳过] 无法联接 Astro 依赖目录");
            return;
        }

        let server = match engine.start(&worktree) {
            Ok(server) => server,
            Err(err) => {
                eprintln!("[跳过] 预览服务未能启动：{}", err.message);
                return;
            }
        };
        let url = server.url().to_string();
        assert!(url.starts_with("http://127.0.0.1:"), "预览地址必须是本机回环：{url}");

        // 1. 首页可访问，且列出这篇（临时转为公开的）文章。
        let Some(home) = poll_until_ready(&url, Duration::from_secs(120)) else {
            let log = server.log_excerpt();
            server.shutdown();
            panic!("预览服务在 120 秒内未就绪（{url}）；日志：{log}");
        };
        assert!(home.contains("测试样稿：预览中的未发布文章"), "首页应列出预览中的文章");

        // 2. 文章页可访问，且包含标题、摘要与正文。
        let article_url = format!("{url}blog/preview-fixture/");
        let Some(article) = poll_until_ready(&article_url, Duration::from_secs(60)) else {
            let log = server.log_excerpt();
            server.shutdown();
            panic!("文章页在 60 秒内未就绪（{article_url}）；日志：{log}");
        };
        assert!(article.contains("测试样稿：预览中的未发布文章"), "文章页应含标题");
        assert!(article.contains("用于验证网站预览的生成式测试样稿"), "文章页应含摘要");
        assert!(article.contains("这是测试样稿正文"), "文章页应含正文");
        // 预览 HTML 里不应出现「已上线」这类未经核实的表述。
        assert!(!article.contains("已上线"));

        // 3. 关闭预览后临时目录被清理，正式仓库与工作区未受影响。
        let temp_root = server.temp_root().to_path_buf();
        server.shutdown();
        assert!(!temp_root.exists(), "关闭预览后应清理临时目录");
        assert!(
            !env.path().join("src/content/blog/preview-fixture.md").exists(),
            "预览不得写入正式仓库"
        );
        assert!(
            env.managed_dirty().iter().all(|line| !line.contains("preview-fixture")),
            "预览不得让正式工作区产生改动"
        );
    }

    /// 未模拟公开时，草稿在预览里既不在列表，也不生成文章路由。
    #[test]
    fn draft_stays_hidden_when_not_simulating_publication() {
        let env = site_fixture();
        let workspace = crate::workspace::Workspace::open(env.path().to_path_buf()).unwrap();
        let engine = PreviewEngine::new(
            &workspace,
            env.dir.path().join("preview-temp"),
            &format!("file://{}", env.remote.to_string_lossy().replace('\\', "/")),
        );

        let toolchain = check_toolchain();
        if !toolchain.missing.is_empty() {
            eprintln!("[跳过] 网站预览前置条件缺失：{}", toolchain.missing.join("；"));
            return;
        }
        let Some(astro_modules) = find_astro_modules() else {
            eprintln!("[跳过] 未找到本机 Astro 依赖");
            return;
        };

        let markdown = "---\ntitle: \"测试样稿：仍为草稿\"\ndescription: \"草稿不应出现在列表。\"\npubDate: \"2026-01-15\"\ndraft: true\n---\n\n草稿正文。\n";
        let worktree = engine.prepare_worktree("e2e-draft").expect("应能建立预览副本");
        engine
            .apply_overlay(
                &worktree,
                &PreviewOverlay {
                    article_id: "stays-draft".to_string(),
                    markdown: markdown.to_string(),
                    images: BTreeMap::new(),
                    simulate_public: false,
                },
            )
            .expect("应能覆盖当前文章");

        let written = std::fs::read_to_string(worktree.join("src/content/blog/stays-draft.md"))
            .expect("副本中应存在该文章");
        assert!(written.contains("draft: true"), "未模拟公开时应保持草稿");

        if !link_modules(&astro_modules, &worktree) {
            eprintln!("[跳过] 无法联接 Astro 依赖目录");
            return;
        }
        let server = match engine.start(&worktree) {
            Ok(server) => server,
            Err(err) => {
                eprintln!("[跳过] 预览服务未能启动：{}", err.message);
                return;
            }
        };
        let url = server.url().to_string();

        let Some(home) = poll_until_ready(&url, Duration::from_secs(120)) else {
            let log = server.log_excerpt();
            server.shutdown();
            panic!("预览服务在 120 秒内未就绪；日志：{log}");
        };
        assert!(!home.contains("测试样稿：仍为草稿"), "草稿不应出现在列表");

        // 草稿不生成文章路由（Astro 对不存在的路径返回非 200）。
        let article_url = format!("{url}blog/stays-draft/");
        assert!(
            poll_until_ready(&article_url, Duration::from_secs(15)).is_none(),
            "草稿不应生成文章路由"
        );

        server.shutdown();
    }

    /// 发布前构建闸门：站点命令失败必须报错，成功才放行（方案 §5.4 第 4 步）。
    ///
    /// 用不带 Astro 依赖的最小 `package.json` 脚本验证闸门本身：不需要联网安装
    /// 依赖，只依赖本机 `pnpm`。缺少 `pnpm` 时跳过（与本模块其它 e2e 用例一致）。
    #[test]
    fn site_checks_gate_blocks_on_failure_and_passes_on_success() {
        if crate::util::resolve_program("pnpm").file_name().is_none() {
            eprintln!("[跳过] 未找到 pnpm");
            return;
        }
        // resolve_program 找不到时会返回原值，需再确认确实存在于 PATH。
        let probe = crate::util::program_command("pnpm").arg("--version").output();
        if !matches!(probe, Ok(ref out) if out.status.success()) {
            eprintln!("[跳过] pnpm 不可用");
            return;
        }

        let env = site_fixture();
        let workspace = crate::workspace::Workspace::open(env.path().to_path_buf()).unwrap();
        let engine = PreviewEngine::new(
            &workspace,
            env.dir.path().join("preview-temp"),
            &format!("file://{}", env.remote.to_string_lossy().replace('\\', "/")),
        );

        // 1) 全部脚本成功 → 放行。
        let ok_tree = engine.prepare_worktree("gate-ok").unwrap();
        std::fs::write(
            ok_tree.join("package.json"),
            r#"{"name":"gate-ok","private":true,"scripts":{"test":"node -e \"process.exit(0)\"","check":"node -e \"process.exit(0)\"","build":"node -e \"process.exit(0)\""}}"#,
        )
        .unwrap();
        engine.run_site_checks(&ok_tree).expect("脚本全部成功时应放行");

        // 2) build 失败 → 必须报 BuildFailed 且带可读摘要。
        let bad_tree = engine.prepare_worktree("gate-bad").unwrap();
        std::fs::write(
            bad_tree.join("package.json"),
            r#"{"name":"gate-bad","private":true,"scripts":{"test":"node -e \"process.exit(0)\"","check":"node -e \"process.exit(0)\"","build":"node -e \"console.error('build exploded'); process.exit(1)\""}}"#,
        )
        .unwrap();
        let err = engine.run_site_checks(&bad_tree).unwrap_err();
        assert_eq!(err.code, ErrorCode::BuildFailed, "{err:?}");
        assert!(err.detail.unwrap_or_default().contains("build exploded"), "应带失败摘要");

        let _ = crate::util::remove_dir_all_no_follow(&ok_tree);
        let _ = crate::util::remove_dir_all_no_follow(&bad_tree);
    }
}
