//! 集成测试夹具：每个测试各自生成隔离的本地 bare remote 与工作副本。
//!
//! 这些夹具**绝不**连接真实 GitHub；远端是临时目录中的 bare 仓库，
//! 因此可以安全地模拟分支并发、推送拒绝、部分失败与断网场景。

#![cfg(test)]

use crate::git;
use crate::local_store::LocalStore;
use crate::workspace::Workspace;
use std::path::{Path, PathBuf};

/// 一份隔离的测试环境：一个 bare 远端 + 一个工作副本 + 应用数据目录。
pub struct TestEnv {
    pub dir: tempfile::TempDir,
    pub remote: PathBuf,
    pub clone: PathBuf,
    pub store: LocalStore,
    pub workspace: Workspace,
}

/// 站点骨架文件。只为让受管路径真实存在，不涉及 Astro 构建。
const SITE_FILES: [(&str, &str); 4] = [
    ("package.json", "{\n  \"name\": \"fixture-site\",\n  \"private\": true\n}\n"),
    ("src/content.config.ts", "// fixture content config\nexport const collections = {};\n"),
    ("src/pages/index.astro", "---\n---\n<p>fixture</p>\n"),
    // 让 public/blog 目录在 Git 中真实存在（Git 不跟踪空目录）。
    ("public/blog/.gitkeep", ""),
];

fn file_url(path: &Path) -> String {
    format!("file://{}", path.to_string_lossy().replace('\\', "/"))
}

/// 隔离 Git 环境变量，避免读取用户全局配置（签名、钩子、代理等）。
fn isolated_env(store: &Path) -> Vec<(&'static str, String)> {
    let empty = store.join("empty-gitconfig");
    let _ = std::fs::write(&empty, "");
    let hooks = store.join("empty-hooks");
    let _ = std::fs::create_dir_all(&hooks);
    vec![
        ("GIT_CONFIG_GLOBAL", empty.to_string_lossy().into_owned()),
        ("GIT_CONFIG_SYSTEM", empty.to_string_lossy().into_owned()),
        ("GIT_CONFIG_NOSYSTEM", "1".to_string()),
        ("GIT_AUTHOR_NAME", "test-fixture".to_string()),
        ("GIT_AUTHOR_EMAIL", "fixture@example.invalid".to_string()),
        ("GIT_COMMITTER_NAME", "test-fixture".to_string()),
        ("GIT_COMMITTER_EMAIL", "fixture@example.invalid".to_string()),
        ("GIT_AUTHOR_DATE", "2026-09-27T00:00:00+08:00".to_string()),
        ("GIT_COMMITTER_DATE", "2026-09-27T00:00:00+08:00".to_string()),
    ]
}

/// 在指定目录执行 git，使用隔离配置。
fn g(dir: &Path, args: &[&str], envs: &[(&'static str, String)]) -> String {
    let out = git::git_with_env(dir, args, envs).unwrap_or_else(|e| {
        panic!("git {args:?} failed: {e}");
    });
    out.stdout_trimmed().to_string()
}

/// 把提交身份与签名策略写入仓库本地配置。
///
/// 软件后端执行 `git commit-tree` 时不会设置进程环境变量，身份必须能从
/// 仓库配置读到，否则提交会以「Author identity unknown」失败。
fn configure_repo(repo: &Path, envs: &[(&'static str, String)]) {
    g(repo, &["config", "user.name", "test-fixture"], envs);
    g(repo, &["config", "user.email", "fixture@example.invalid"], envs);
    // 避免用户全局的签名配置让测试提交失败。
    g(repo, &["config", "commit.gpgsign", "false"], envs);
    g(repo, &["config", "tag.gpgsign", "false"], envs);
}

/// 按 NUL 分隔解析 git 的 `-z` 输出。
fn split_nul(text: &str) -> Vec<String> {
    text.split('\0').map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect()
}

/// 在 Windows 上让钩子脚本可执行。
///
/// Git for Windows 通过 sh 执行钩子；设置 Unix 可执行位可避免被跳过。
#[cfg(windows)]
fn make_executable(path: &Path) {
    use std::os::windows::ffi::OsStrExt;
    // 直接调用 chmod 不可用；用 git 的 update-index 不适用（不在仓库内），
    // 因此借助 `cmd /c` 之外的纯 Rust 方式：写到 hooks 后由 sh 读取。
    // Windows 无执行位概念，Git for Windows 只要求文件存在且可读，
    // 这里额外写入正确的 LF 换行以确保 sh 能解析。
    if let Ok(text) = std::fs::read_to_string(path) {
        let normalized = text.replace("\r\n", "\n");
        let _ = std::fs::write(path, normalized);
    }
    let _ = path.as_os_str().encode_wide().count();
}

#[cfg(not(windows))]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = std::fs::metadata(path) {
        let mut perms = meta.permissions();
        perms.set_mode(0o755);
        let _ = std::fs::set_permissions(path, perms);
    }
}

impl TestEnv {
    /// 创建一份测试环境：bare 远端（含 `main`，内容为站点骨架）与工作副本。
    pub fn new() -> Self {
        let dir = tempfile::tempdir().expect("create temp dir");
        let root = dir.path();
        let store_root = root.join("appdata");
        std::fs::create_dir_all(&store_root).unwrap();
        let envs = isolated_env(root);

        // 1. 建 bare 远端。
        let remote = root.join("remote.git");
        std::fs::create_dir_all(&remote).unwrap();
        g(&remote, &["init", "--bare", "--initial-branch=main"], &envs);

        // 2. 建一把「种子」工作副本，写入站点骨架并推送 main。
        let seed = root.join("seed");
        std::fs::create_dir_all(&seed).unwrap();
        g(&seed, &["init", "--initial-branch=main"], &envs);
        configure_repo(&seed, &envs);
        for (rel, content) in SITE_FILES {
            let abs = seed.join(rel);
            if let Some(parent) = abs.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&abs, content).unwrap();
        }
        std::fs::create_dir_all(seed.join("src/content/blog")).unwrap();
        std::fs::write(seed.join("src/content/blog/.gitkeep"), "").unwrap();
        g(&seed, &["add", "-A"], &envs);
        g(&seed, &["commit", "-m", "initial site skeleton"], &envs);
        g(&seed, &["remote", "add", "origin", &file_url(&remote)], &envs);
        g(&seed, &["push", "origin", "main"], &envs);

        // 3. 独立工作副本：从远端 main 克隆（模拟软件首次连接时建立的 clone）。
        let clone = root.join("workspace");
        g(root, &["clone", "--branch", "main", &file_url(&remote), "workspace"], &envs);
        g(&clone, &["remote", "set-url", "origin", &file_url(&remote)], &envs);
        configure_repo(&clone, &envs);

        let store = LocalStore::open_at(store_root).unwrap();
        let workspace = Workspace::open(clone.clone()).unwrap();

        TestEnv { dir, remote, clone, store, workspace }
    }

    pub fn envs(&self) -> Vec<(&'static str, String)> {
        isolated_env(self.dir.path())
    }

    pub fn git(&self, args: &[&str]) -> String {
        let envs = self.envs();
        g(&self.clone, args, &envs)
    }

    /// 在另一个「设备」上克隆远端并执行操作，用于模拟并发推进。
    pub fn other_device(&self, name: &str) -> OtherDevice {
        let envs = self.envs();
        let path = self.dir.path().join(name);
        g(self.dir.path(), &["clone", &file_url(&self.remote), name], &envs);
        configure_repo(&path, &envs);
        OtherDevice { path, envs }
    }

    /// 远端某个分支的头。
    pub fn remote_head(&self, branch: &str) -> Option<String> {
        let out = self.git(&["ls-remote", "--heads", "origin", &format!("refs/heads/{branch}")]);
        out.lines().find_map(|l| l.split_whitespace().next()).map(str::to_string)
    }

    /// 从本仓库跟踪引用读取远端分支上的文件内容。
    pub fn remote_file(&self, branch: &str, rel: &str) -> Option<String> {
        self.show_at(&self.clone, &format!("refs/remotes/origin/{branch}:{rel}"))
    }

    /// 从 bare 远端直接读取某分支上的文件（不依赖本地跟踪引用）。
    pub fn remote_file_direct(&self, branch: &str, rel: &str) -> Option<String> {
        self.show_at(&self.remote, &format!("refs/heads/{branch}:{rel}"))
    }

    fn show_at(&self, repo: &Path, spec: &str) -> Option<String> {
        let envs = self.envs();
        let out = crate::util::program_command("git")
            .current_dir(repo)
            .envs(envs.iter().map(|(k, v)| (*k, v.clone())))
            .args(["-c", "core.quotePath=false", "show", spec])
            .output()
            .ok()?;
        if out.status.success() {
            Some(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            None
        }
    }

    /// 列出 bare 远端某分支上受管目录中的文件。
    pub fn remote_ls(&self, branch: &str, dir: &str) -> Vec<String> {
        let envs = self.envs();
        let out = crate::util::program_command("git")
            .current_dir(&self.remote)
            .envs(envs.iter().map(|(k, v)| (*k, v.clone())))
            .args(["-c", "core.quotePath=false", "ls-tree", "-r", "-z", "--name-only", branch, "--", dir])
            .output()
            .expect("ls-tree");
        split_nul(&String::from_utf8_lossy(&out.stdout))
    }

    /// 当前工作副本的分支名。
    pub fn current_branch(&self) -> String {
        self.git(&["rev-parse", "--abbrev-ref", "HEAD"])
    }

    /// 工作副本中受管范围内的未提交改动。
    pub fn managed_dirty(&self) -> Vec<String> {
        let out = self.git(&[
            "-c",
            "core.quotePath=false",
            "status",
            "--porcelain",
            "-z",
            "--untracked-files=all",
            "--",
            "src/content/blog",
            "public/blog",
        ]);
        split_nul(&out)
    }

    /// 提交当前工作副本中的受管改动（模拟用户或其它工具的直接提交）。
    pub fn commit_managed(&self, message: &str) {
        self.git(&["add", "-A", "--", "src/content/blog", "public/blog"]);
        self.git(&["commit", "-m", message]);
    }

    /// 把当前分支推送到远端同名分支。
    pub fn push_current(&self, branch: &str) {
        self.git(&["push", "origin", &format!("HEAD:refs/heads/{branch}")]);
    }

    /// 在 bare 远端安装 `pre-receive` 钩子，拒绝推送到指定分支。
    ///
    /// 用于真实模拟「某一个分支推送失败」的部分成功场景。
    pub fn reject_pushes_to(&self, branch: &str) {
        let hooks = self.remote.join("hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        let script = format!(
            "#!/bin/sh\nwhile read old new ref; do\n  case \"$ref\" in\n    refs/heads/{branch})\n      echo \"remote rejected: {branch} is locked\" >&2\n      exit 1;;\n  esac\ndone\nexit 0\n"
        );
        let path = hooks.join("pre-receive");
        std::fs::write(&path, script).unwrap();
        make_executable(&path);
    }

    /// 移除 `pre-receive` 钩子，恢复可推送状态。
    pub fn clear_push_hooks(&self) {
        let _ = std::fs::remove_file(self.remote.join("hooks").join("pre-receive"));
    }

    /// 工作副本路径。
    pub fn path(&self) -> &Path {
        &self.clone
    }
}

/// 模拟另一台设备的克隆。
pub struct OtherDevice {
    pub path: PathBuf,
    envs: Vec<(&'static str, String)>,
}

impl OtherDevice {
    pub fn git(&self, args: &[&str]) -> String {
        g(&self.path, args, &self.envs)
    }

    /// 写入文件并提交。
    pub fn write_and_commit(&self, rel: &str, content: &str, message: &str) {
        let abs = self.path.join(rel);
        if let Some(parent) = abs.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&abs, content).unwrap();
        self.git(&["add", "-A", "--", rel]);
        self.git(&["commit", "-m", message]);
    }

    pub fn push(&self, branch: &str) {
        self.git(&["push", "origin", &format!("HEAD:refs/heads/{branch}")]);
    }

    pub fn checkout_new_branch(&self, branch: &str, from: &str) {
        self.git(&["checkout", "-B", branch, from]);
    }

    pub fn fetch(&self, branch: &str) {
        self.git(&["fetch", "origin", branch]);
    }
}

/// 生成一篇测试样稿的 Markdown 文本（明确标记为测试样稿）。
pub fn sample_markdown(title: &str, draft: bool) -> String {
    sample_markdown_with_body(title, draft, "这是测试样稿正文，不对应真实观澜记录。")
}

/// 生成一篇带指定正文的测试样稿。
pub fn sample_markdown_with_body(title: &str, draft: bool, body: &str) -> String {
    format!(
        "---\ntitle: \"{title}\"\ndescription: \"{title} 的测试摘要\"\npubDate: \"2026-09-23\"\ntags: [测试样稿]\ndraft: {draft}\n---\n\n{body}\n"
    )
}

/// 一段合法的 1×1 PNG 字节。
pub fn tiny_png() -> Vec<u8> {
    const PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, // signature
        0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52, // IHDR
        0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15,
        0xC4, 0x89, //
        0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, // IDAT
        0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, //
        0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82, // IEND
    ];
    PNG.to_vec()
}

/// 内容不同的另一张 PNG（用于冲突场景）。
pub fn other_png() -> Vec<u8> {
    let mut bytes = tiny_png();
    // 改动 IDAT 中的一个字节，保持签名与结构合法。
    let idx = bytes.len() - 8;
    bytes[idx] = bytes[idx].wrapping_add(1);
    bytes
}
