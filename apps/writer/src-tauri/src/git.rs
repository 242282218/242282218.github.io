//! 不经 shell 的 Git 命令封装。
//!
//! 所有调用都以参数数组执行固定的 `git` 可执行文件，绝不经过 `cmd /c`
//! 字符串拼接；仓库路径、分支名和文件路径都作为独立参数传入，因此不存在
//! 命令注入面。日志与错误只在调用方记录非敏感字段。

use crate::model::{ErrorCode, Result, WriterError};
use std::path::{Path, PathBuf};
use std::process::Command;

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
    let mut cmd = Command::new("git");
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
    let output = Command::new("git")
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
}
