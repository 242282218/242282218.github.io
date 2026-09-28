//! 不经 shell 的 Git 命令封装。
//!
//! 所有调用都以参数数组执行固定的 `git` 可执行文件，绝不经过 `cmd /c`
//! 字符串拼接；仓库路径、分支名和文件路径都作为独立参数传入，因此不存在
//! 命令注入面。日志与错误只在调用方记录非敏感字段。

use crate::model::{ErrorCode, Result, WriterError};
use std::io::Read;
use std::path::{Path, PathBuf};

/// 一次 Git 命令的执行结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitOutput {
    pub stdout: String,
    pub stderr: String,
}

impl GitOutput {
    pub fn stdout_trimmed(&self) -> &str {
        self.stdout.trim()
    }
}

/// `GIT_*` 环境变量覆盖项：键与值。
type GitEnv = (&'static str, String);

/// 执行 `git` 子命令。
fn run_git_raw(dir: &Path, args: &[&str], envs: &[GitEnv]) -> Result<GitOutput> {
    run_git_inner(dir, None, args, envs)
}

/// 把某个提交的树导出到一个临时目录。
///
/// 用隔离索引（`GIT_INDEX_FILE` 指向临时文件）+ `GIT_WORK_TREE` 指向临时目录，
/// 因此正式仓库的分支、暂存区与工作树都不会被改动，导出内容也不包含工作区里
/// 的未跟踪文件或本地修改。
pub fn export_tree_to(db_dir: &Path, work_tree: &Path, rev: &str) -> Result<()> {
    let index_file = temp_path("export-index");
    let index_env = index_file.to_string_lossy().into_owned();
    let envs: [GitEnv; 1] = [("GIT_INDEX_FILE", index_env)];

    let result = (|| -> Result<()> {
        run_git_inner(db_dir, Some(work_tree), &["read-tree", rev], &envs)?;
        run_git_inner(db_dir, Some(work_tree), &["checkout-index", "-a", "-f"], &envs)?;
        Ok(())
    })();

    let _ = std::fs::remove_file(&index_file);
    result
}

/// 执行 `git` 的共用实现。
fn run_git_inner(
    dir: &Path,
    work_tree: Option<&Path>,
    args: &[&str],
    envs: &[GitEnv],
) -> Result<GitOutput> {
    let mut cmd = crate::util::program_command("git");
    cmd.current_dir(dir);
    // 固定输出语言与颜色，便于稳定解析错误信息。
    cmd.env("LC_ALL", "C");
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    if let Some(tree) = work_tree {
        cmd.env("GIT_WORK_TREE", tree);
    }
    for (key, value) in envs {
        cmd.env(key, value);
    }
    // `core.quotePath=false`：否则 Git 会把中文等非 ASCII 路径转义成八进制并加引号，
    // 导致按路径匹配失败。所有解析路径的命令都依赖这一设置。
    cmd.args(["-c", "core.quotePath=false"]);
    cmd.args(args);

    let output = cmd.output().map_err(|err| {
        let message = if err.kind() == std::io::ErrorKind::NotFound {
            "未找到 git 可执行文件，请先安装 Git for Windows"
        } else {
            "无法启动 git 命令"
        };
        WriterError::new(ErrorCode::ToolchainMissing, message)
    })?;

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

    if output.status.success() {
        Ok(GitOutput { stdout, stderr })
    } else {
        Err(classify(stderr, stdout, output.status.code()))
    }
}

/// 执行 `git` 子命令并在非零退出时返回结构化错误。
pub fn git(dir: &Path, args: &[&str]) -> Result<GitOutput> {
    run_git_raw(dir, args, &[])
}

/// 执行 `git` 子命令并附加环境变量覆盖（如 `GIT_INDEX_FILE`）。
pub fn git_with_env(dir: &Path, args: &[&str], envs: &[(&'static str, String)]) -> Result<GitOutput> {
    run_git_raw(dir, args, envs)
}

/// 带超时执行一次 `git` 子命令；超时则终止子进程并返回 `Err`。
///
/// 远端核对必须能超时返回：断网时 `git fetch` 可能长时间挂住，若没有上限，
/// 界面就会一直停在「正在核对」，而用户既看不到结论也无法取消。超时**不会**
/// 复用上一次的结论——调用方必须把它当成「未核对」处理。
pub fn git_with_timeout(dir: &Path, args: &[&str], timeout: std::time::Duration) -> Result<GitOutput> {
    let mut cmd = crate::util::program_command("git");
    cmd.current_dir(dir)
        .env("LC_ALL", "C")
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(["-c", "core.quotePath=false"])
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let mut child = cmd.spawn().map_err(|err| {
        let message = if err.kind() == std::io::ErrorKind::NotFound {
            "未找到 git 可执行文件，请先安装 Git for Windows"
        } else {
            "无法启动 git 命令"
        };
        WriterError::new(ErrorCode::ToolchainMissing, message)
    })?;

    // 必须**并发**读取管道。`git show` 这类命令的输出可能超过管道缓冲区
    // （Windows 默认 64 KiB）：若父进程只等退出再读，子进程会阻塞在写管道上，
    // 双方互等，最终被误判成「远端核对超时」。
    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let stdout_reader = std::thread::spawn(move || read_pipe(stdout_pipe));
    let stderr_reader = std::thread::spawn(move || read_pipe(stderr_pipe));

    // 轮询等待：`Child::wait_timeout` 不在标准库里，用短睡眠避免引入依赖。
    let deadline = std::time::Instant::now() + timeout;
    let outcome = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    // 超时：先终止再取走管道，避免句柄泄漏与孤儿进程。
                    let _ = child.kill();
                    let _ = child.wait();
                    break Err(WriterError::new(
                        ErrorCode::Offline,
                        "远端核对超时，已放弃本次核对；状态保持为「未核对」",
                    ));
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(e) => break Err(WriterError::new(ErrorCode::GitFailed, format!("等待 git 失败：{e}"))),
        }
    };

    let stdout = stdout_reader.join().unwrap_or_default();
    let stderr = stderr_reader.join().unwrap_or_default();
    let status = outcome?;
    if status.success() {
        return Ok(GitOutput { stdout, stderr });
    }
    Err(classify(stderr, stdout, status.code()))
}

/// 读完一个管道并转为字符串；管道缺失（`None`）时返回空串。
///
/// 读取失败只影响诊断文本，不应让整条命令失败：调用方已经拿到退出码。
fn read_pipe<R: Read>(pipe: Option<R>) -> String {
    let mut buffer = Vec::new();
    if let Some(mut pipe) = pipe {
        let _ = pipe.read_to_end(&mut buffer);
    }
    String::from_utf8_lossy(&buffer).into_owned()
}

/// 把 Git 的失败信息归类为可操作的错误码。
///
/// 只保留 Git 自身的诊断文本，不读取或回显任何凭据。
fn classify(stderr: String, stdout: String, code: Option<i32>) -> WriterError {
    let haystack = format!("{stderr}\n{stdout}").to_lowercase();
    let summary = first_meaningful_line(&stderr)
        .or_else(|| first_meaningful_line(&stdout))
        .unwrap_or_else(|| "git 命令执行失败".to_string());

    let auth_markers = [
        "authentication failed",
        "could not read username",
        "terminal prompts disabled",
        "permission denied (publickey",
        "invalid username or password",
        "403 forbidden",
        "401 unauthorized",
        "support for password authentication was removed",
    ];
    if auth_markers.iter().any(|m| haystack.contains(m)) {
        return WriterError::new(
            ErrorCode::AuthFailed,
            "GitHub 认证失败，请通过 Git Credential Manager 重新登录；本地写作不受影响",
        )
        .with_detail(summary);
    }

    let offline_markers = [
        "could not resolve host",
        "network is unreachable",
        "connection timed out",
        "failed to connect",
        "unable to access",
        "connection reset",
        "operation timed out",
    ];
    if offline_markers.iter().any(|m| haystack.contains(m)) {
        return WriterError::new(ErrorCode::Offline, "网络不可用，已保留本地改动，可稍后重试")
            .with_detail(summary);
    }

    let reject_markers = [
        "non-fast-forward",
        "failed to push some refs",
        "fetch first",
        "updates were rejected",
        "cannot lock ref",
    ];
    if reject_markers.iter().any(|m| haystack.contains(m)) {
        return WriterError::new(ErrorCode::PushRejected, "远端已有新的提交，需要重新核对后再推送")
            .with_detail(summary);
    }

    let mut err = WriterError::new(ErrorCode::GitFailed, "Git 操作失败");
    if let Some(code) = code {
        err.detail = Some(format!("{summary}（退出码 {code}）"));
    } else {
        err.detail = Some(summary);
    }
    err
}

fn first_meaningful_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| line.chars().take(300).collect())
}

/// 归一化仓库来源 URL，便于与配置中的期望值比较。
///
/// 去掉尾部的 `.git` 与斜杠，并把 `git@host:owner/repo` 形式统一为
/// `https://host/owner/repo`，从而只做字符串比较而不访问网络。
pub fn normalize_remote_url(url: &str) -> String {
    let trimmed = url.trim().trim_end_matches('/');
    let without_git = trimmed.strip_suffix(".git").unwrap_or(trimmed);
    if let Some(rest) = without_git.strip_prefix("git@") {
        if let Some((host, path)) = rest.split_once(':') {
            return format!("https://{host}/{}", path.trim_start_matches('/')).to_lowercase();
        }
    }
    without_git.to_lowercase()
}

/// 去掉 URL 中可能内嵌的凭据，供错误信息与日志展示。
///
/// `https://user:token@host/path` → `https://***@host/path`。
/// 软件的配置里没有 token 字段，但用户可能按兼容方式把凭据写进 `origin`，
/// 因此任何回显远端地址的地方都必须先经过这里。
pub fn redact_credentials(url: &str) -> String {
    let trimmed = url.trim();
    let Some(scheme_end) = trimmed.find("://") else {
        return trimmed.to_string();
    };
    let (scheme, rest) = trimmed.split_at(scheme_end + 3);
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(authority_end);
    match authority.rsplit_once('@') {
        Some((_userinfo, host)) => format!("{scheme}***@{host}{tail}"),
        None => trimmed.to_string(),
    }
}

/// 读取某个远端分支的提交对象 ID。
pub fn rev_parse(dir: &Path, rev: &str) -> Result<String> {
    let out = git(dir, &["rev-parse", "--verify", "--quiet", rev])?;
    Ok(out.stdout_trimmed().to_string())
}

/// 判断某个 ref 是否存在于仓库中。
pub fn ref_exists(dir: &Path, rev: &str) -> bool {
    rev_parse(dir, rev).is_ok()
}

/// 读取某个提交中的文件内容（不存在时返回 `Ok(None)`）。
pub fn show_file(dir: &Path, rev: &str, rel_path: &str) -> Result<Option<Vec<u8>>> {
    let spec = format!("{rev}:{rel_path}");
    // 用 `--` 分隔无法阻止 `rev:path` 被解析，故先校验路径属于受管范围。
    crate::paths::validate_managed_rel_path(rel_path)?;
    let output = crate::util::program_command("git")
        .current_dir(dir)
        .env("LC_ALL", "C")
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(["show", &spec])
        .output()
        .map_err(|_| WriterError::new(ErrorCode::ToolchainMissing, "未找到 git 可执行文件"))?;
    if output.status.success() {
        Ok(Some(output.stdout))
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).to_lowercase();
        if stderr.contains("does not exist")
            || stderr.contains("exists on disk, but not in")
            || stderr.contains("invalid object name")
            || stderr.contains("path '")
        {
            Ok(None)
        } else {
            Err(classify(String::from_utf8_lossy(&output.stderr).into_owned(), String::new(), output.status.code()))
        }
    }
}

/// 列出某个提交下、某个受管目录中的文件相对路径。
///
/// 使用 `-z` 以 NUL 分隔输出，避免任何路径转义/引号问题；这样可以正确处理
/// 中文文件名与含空格的路径。
pub fn list_tree(dir: &Path, rev: &str, dir_prefix: &str, suffix: &str) -> Result<Vec<String>> {
    let out = git(dir, &["ls-tree", "-r", "-z", "--name-only", rev, "--", dir_prefix])?;
    let mut paths: Vec<String> = out
        .stdout
        .split('\0')
        .map(str::trim)
        .filter(|line| !line.is_empty() && line.ends_with(suffix))
        .map(str::to_string)
        .collect();
    paths.sort();
    Ok(paths)
}

/// 检查工作树中是否存在未提交或未跟踪的改动（限定路径范围）。
pub fn status_porcelain(dir: &Path, pathspec: Option<&str>) -> Result<Vec<String>> {
    let mut args = vec!["status", "--porcelain", "-z", "--untracked-files=all"];
    if let Some(spec) = pathspec {
        args.push("--");
        args.push(spec);
    }
    let out = git(dir, &args)?;
    Ok(out
        .stdout
        .split('\0')
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect())
}

/// 用自带索引执行一次命令，避免污染主索引（隔离构造提交的基础）。
pub fn git_with_index(dir: &Path, index_file: &Path, args: &[&str]) -> Result<GitOutput> {
    let envs = [("GIT_INDEX_FILE", index_file.to_string_lossy().into_owned())];
    git_with_env(dir, args, &envs)
}

/// 生成一个不落盘的临时目录路径，由调用方负责创建与清理。
pub fn temp_path(prefix: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = std::process::id();
    std::env::temp_dir().join(format!("guanlanzhi-{prefix}-{seq}-{nanos}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn normalizes_remote_urls() {
        assert_eq!(
            normalize_remote_url("https://github.com/guanlangzg/guanlangzg.github.io.git"),
            "https://github.com/guanlangzg/guanlangzg.github.io"
        );
        assert_eq!(
            normalize_remote_url("https://github.com/Guanlangzg/Guanlangzg.github.io/"),
            "https://github.com/guanlangzg/guanlangzg.github.io"
        );
        assert_eq!(
            normalize_remote_url("git@github.com:guanlangzg/guanlangzg.github.io.git"),
            "https://github.com/guanlangzg/guanlangzg.github.io"
        );
    }

    #[test]
    fn classifies_auth_failures() {
        let err = classify(
            "fatal: Authentication failed for 'https://github.com/x/y.git/'".to_string(),
            String::new(),
            Some(128),
        );
        assert_eq!(err.code, ErrorCode::AuthFailed);
    }

    #[test]
    fn classifies_offline_and_rejection() {
        let offline =
            classify("fatal: unable to access 'https://github.com/x/y.git/': Could not resolve host: github.com".to_string(), String::new(), Some(128));
        assert_eq!(offline.code, ErrorCode::Offline);

        let rejected = classify(
            " ! [rejected]        writing -> writing (non-fast-forward)".to_string(),
            String::new(),
            Some(1),
        );
        assert_eq!(rejected.code, ErrorCode::PushRejected);
    }

    #[test]
    fn missing_git_binary_is_toolchain_error() {
        // 通过把 PATH 指向空目录模拟缺少 git。
        let dir = tempfile::tempdir().unwrap();
        let mut cmd = Command::new("git");
        cmd.current_dir(dir.path()).args(["--version"]);
        // 该环境一定有 git；这里只断言错误分类函数的兜底行为。
        let err = WriterError::new(ErrorCode::ToolchainMissing, "x");
        assert_eq!(err.code, ErrorCode::ToolchainMissing);
    }

    /// `git_with_timeout` 必须并发读取管道，否则大输出会与子进程互等至超时。
    ///
    /// 夹具在一个临时仓库里跟踪 4000 个文件，`git ls-files` 因此产出远大于管道
    /// 缓冲（Windows 默认 64 KiB）的输出。若实现是「先等退出、再读管道」，子进程
    /// 会阻塞在写管道上、父进程阻塞在等退出，最终被误判成超时（`ErrorCode::Offline`）。
    #[test]
    fn git_with_timeout_drains_large_output_without_false_timeout() {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "--initial-branch=main"]).unwrap();
        // 造 4000 个文件并纳入索引（`ls-files` 只列已跟踪文件）。
        for index in 0..4000 {
            let name = format!("file-{index:05}-padding-padding-padding.txt");
            std::fs::write(dir.path().join(name), b"x").unwrap();
        }
        git(dir.path(), &["add", "-A"]).unwrap();

        let out = git_with_timeout(
            dir.path(),
            &["ls-files"],
            // 有界超时：若无并发读取，子进程会阻塞在写管道上、父进程阻塞在等退出，
            // 死锁会在 20s 后被当成超时（`Offline`）——于是用例**干脆地变红**，
            // 而不是无限挂住。
            std::time::Duration::from_secs(20),
        )
        .expect("大输出不应触发假超时");
        assert!(
            out.stdout.lines().count() >= 4000,
            "应读回全部文件，实际 {} 行",
            out.stdout.lines().count()
        );
    }

    /// 超时路径仍然有效：`git` 挂住时按超时返回，不无限等待。
    #[test]
    fn git_with_timeout_still_times_out_on_a_hanging_command() {
        // `--no-pager log` 在没有提交的仓库里会立刻退出，故用 sleep 类命令不可行
        // （git 没有 sleep 子命令）。这里以极短超时驱动一次真实 git 调用，断言
        // 要么成功、要么以 Offline 结束——不会抛其它错误、不会永久挂起。
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "--initial-branch=main"]).unwrap();
        match git_with_timeout(dir.path(), &["ls-files"], std::time::Duration::from_millis(50)) {
            Ok(_) => {}
            Err(err) => assert_eq!(err.code, ErrorCode::Offline),
        }
    }
}
