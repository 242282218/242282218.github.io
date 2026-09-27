//! 首次连接与工作区初始化。
//!
//! 首次连接在应用数据目录中创建**独立 clone**，绝不操作开发者手头的 repo。
//! 读取 GitHub `main` 作为起点；`writing` 分支在第一次明确点击「同步」时
//! 才以远端 `main` 快照为起点建立，并由界面预告。

use crate::git;
use crate::local_store::{AppConfig, LocalStore, DEFAULT_REPO_LABEL};
use crate::model::{ErrorCode, Result, WriterError};
use crate::paths;
use crate::workspace::Workspace;
use std::path::{Path, PathBuf};

/// 首次连接前的环境探测结果。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionStatus {
    /// 已配置的目标仓库（只读展示）。
    pub repo_label: String,
    /// 实际的远端 URL。
    pub repo_url: String,
    /// 独立工作目录（空表示尚未创建）。
    pub workspace_dir: String,
    pub connected: bool,
    /// 工作目录是否真实存在且是 Git 仓库。
    pub workspace_ready: bool,
    pub toolchain: crate::preview::ToolchainReport,
    /// 必须让用户确认的公开性说明。
    pub public_disclosure: String,
    /// 是否已向用户说明「公开仓库会公开远程草稿」。
    pub disclosed_public_drafts: bool,
    /// 需要用户填写时的问题（如缺失目录）。
    pub blocking_issue: Option<String>,
}

/// 公开性说明文案（首次启动必须展示）。
pub const PUBLIC_DISCLOSURE: &str =
    "本软件使用公开仓库保存远程草稿：在写作分支中同步的草稿对任何访问者可见，网站上不会展示未发布的文章。";

/// 首次连接的编排器。
pub struct Connector {
    store: LocalStore,
}

impl Connector {
    pub fn new(store: LocalStore) -> Self {
        Self { store }
    }

    /// 读取当前的连接状态。
    pub fn status(&self) -> ConnectionStatus {
        let config = self.store.load_config();
        let workspace_ready = !config.workspace_dir.is_empty()
            && Path::new(&config.workspace_dir).join(".git").exists();
        ConnectionStatus {
            repo_label: config.repo_label.clone(),
            repo_url: config.repo_url.clone(),
            workspace_dir: config.workspace_dir.clone(),
            connected: config.connected && workspace_ready,
            workspace_ready,
            toolchain: crate::preview::check_toolchain(),
            public_disclosure: PUBLIC_DISCLOSURE.to_string(),
            disclosed_public_drafts: config.disclosed_public_drafts,
            blocking_issue: None,
        }
    }

    /// 确认已向用户说明公开仓库的草稿可见性。
    pub fn acknowledge_disclosure(&self) -> Result<()> {
        let mut config = self.store.load_config();
        config.disclosed_public_drafts = true;
        self.store.save_config(&config)
    }

    /// 默认的工作目录位置（应用数据目录下的 `workspace`）。
    pub fn default_workspace_dir(&self) -> PathBuf {
        self.store.root().join("workspace")
    }

    /// 建立独立工作目录：在应用数据目录中 clone 目标仓库的 `main`。
    ///
    /// - 目标仓库固定为配置中的公开仓库，不接受任意路径输入；
    /// - 不使用开发者手头的 repo；
    /// - 离线时可指向一个已存在的工作目录继续使用（`adopt_existing`）。
    pub fn connect(&self, workspace_dir: Option<PathBuf>) -> Result<ConnectionStatus> {
        let mut config = self.store.load_config();
        if !config.disclosed_public_drafts {
            return Err(WriterError::new(
                ErrorCode::InvalidArgument,
                "请先确认已知悉公开仓库会公开远程草稿",
            ));
        }

        let target = workspace_dir.unwrap_or_else(|| self.default_workspace_dir());
        // 固定仓库身份：只接受「默认公开仓库」或可被规范化的远端 URL
        // （`https://` / `http://` / `file://` / `git@host:...`）。
        // 拒绝相对路径、盘符与任意本机路径，避免用户输入被当成 Git 命令参数。
        let expected = git::normalize_remote_url(&config.repo_url);
        if !is_supported_remote_url(&config.repo_url) {
            return Err(WriterError::new(
                ErrorCode::InvalidArgument,
                "目标仓库地址不是受支持的 Git 远端，已停止连接",
            )
            .with_detail(config.repo_url.clone()));
        }
        let _ = expected;

        if target.join(".git").exists() {
            // 已存在工作目录：校验 origin 是否指向配置仓库，是则直接采用。
            let out = git::git(&target, &["remote", "get-url", "origin"])?;
            if git::normalize_remote_url(out.stdout_trimmed()) != expected {
                return Err(WriterError::new(
                    ErrorCode::InvalidArgument,
                    "该目录的 origin 与目标仓库不一致。切换仓库需要选择新的空目录，不能原地改动 origin",
                )
                .with_detail(git::redact_credentials(out.stdout_trimmed())));
            }
            // 关键防护：绝不把**开发者手头的 checkout** 当成工作区。
            // 否则同步/发布/删除会直接作用在该目录上（推真实远端、删真实文件）。
            // 应用数据目录内的目录是自己的 clone，不受此限制。
            if !is_inside_app_data(&self.store, &target) && looks_like_development_checkout(&target) {
                return Err(WriterError::new(
                    ErrorCode::InvalidArgument,
                    "该目录看起来是本项目的开发检出目录，软件不会把它当作工作区；请改用应用数据目录下的独立工作目录",
                )
                .with_detail(target.to_string_lossy().to_string()));
            }
        } else {
            if target.exists() {
                // 目录非空且不是 Git 仓库时，不擅自写入。
                let non_empty = std::fs::read_dir(&target)
                    .map(|mut d| d.next().is_some())
                    .unwrap_or(false);
                if non_empty {
                    return Err(WriterError::new(
                        ErrorCode::InvalidArgument,
                        "目标目录非空且不是 Git 仓库，请选择一个空目录",
                    )
                    .with_detail(target.to_string_lossy().to_string()));
                }
            }
            self.clone_repository(&config.repo_url, &target)?;
        }

        config.workspace_dir = target.to_string_lossy().to_string();
        config.connected = true;
        self.store.save_config(&config)?;
        Ok(self.status())
    }

    /// 在指定目录 clone 公开仓库的 `main`。
    fn clone_repository(&self, url: &str, target: &Path) -> Result<()> {
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                WriterError::new(ErrorCode::IoFailed, format!("创建父目录失败：{e}"))
            })?;
        }
        // 只取 main；`--single-branch` 避免顺带拉取开发分支。
        let target_str = target.to_string_lossy().to_string();
        git::git(
            target.parent().unwrap_or(Path::new(".")),
            &[
                "clone",
                "--branch",
                "main",
                "--single-branch",
                url,
                &target_str,
            ],
        )?;
        Ok(())
    }

    /// 打开已连接的工作区。
    pub fn open_workspace(&self) -> Result<Workspace> {
        let config = self.store.load_config();
        if config.workspace_dir.is_empty() {
            return Err(WriterError::new(ErrorCode::InvalidArgument, "尚未完成首次连接"));
        }
        Workspace::open(PathBuf::from(&config.workspace_dir))
    }

    /// 读取配置（供命令层复用）。
    pub fn config(&self) -> AppConfig {
        self.store.load_config()
    }

    /// 取得应用数据目录中的预览临时根目录。
    pub fn preview_temp_root(&self) -> PathBuf {
        self.store.preview_dir()
    }
}

/// 校验并规范化用户给出的工作目录。
///
/// 只接受绝对路径；相对路径与空值被拒绝。
pub fn normalize_workspace_dir(input: &str) -> Result<PathBuf> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(WriterError::new(
            ErrorCode::InvalidArgument,
            "工作目录不能为空",
        ));
    }
    let path = PathBuf::from(trimmed);
    if !path.is_absolute() {
        return Err(WriterError::new(
            ErrorCode::InvalidArgument,
            "工作目录必须是绝对路径",
        ));
    }
    Ok(path)
}

/// 判断配置中的仓库地址是否为受支持的 Git 远端 URL。
///
/// 只接受 `https://`、`http://`、`file://` 与 `git@host:owner/repo` 形式；
/// 相对路径、盘符（`C:\...`）与裸本机路径一律拒绝，避免用户输入直接变成
/// Git 命令参数或本地路径。
pub fn is_supported_remote_url(url: &str) -> bool {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return false;
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("https://") || lower.starts_with("http://") || lower.starts_with("file://")
    {
        return true;
    }
    // `git@host:owner/repo` 形式。
    if let Some(rest) = trimmed.strip_prefix("git@") {
        return rest.contains(':') && !rest.contains('\\');
    }
    false
}

/// 默认目标仓库标签（供 UI 只读展示）。
pub fn default_repo_label() -> &'static str {
    DEFAULT_REPO_LABEL
}

/// 检查工作目录是否位于应用数据目录之内（防止误把用户 repo 当工作区）。
pub fn is_inside_app_data(store: &LocalStore, candidate: &Path) -> bool {
    let root = store.root();
    candidate.starts_with(root)
}

/// 确认一条路径不是开发者手头的 repo。
///
/// 判据是「具备本项目开发检出的特征」：站点源码目录 + 开发用目录
/// （`.superpowers` / `.zcode`）同时存在。仅凭某一项判断会误伤普通工作区。
pub fn looks_like_development_checkout(path: &Path) -> bool {
    let has_site_source = path.join("src/content.config.ts").exists()
        || path.join("astro.config.mjs").exists();
    let has_dev_marker = path.join(".superpowers").exists() || path.join(".zcode").exists();
    has_site_source && has_dev_marker
}

/// 受管路径清单常量（供 UI 展示「将影响哪些文件」）。
pub fn managed_paths() -> Vec<&'static str> {
    vec![paths::BLOG_DIR_PREFIX.trim_end_matches('/'), paths::IMAGE_DIR_PREFIX.trim_end_matches('/')]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, LocalStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalStore::open_at(dir.path().join("appdata")).unwrap();
        (dir, store)
    }

    #[test]
    fn disclosure_must_be_acknowledged_before_connecting() {
        let (_dir, store) = store();
        let connector = Connector::new(store);
        let err = connector.connect(Some(connector.default_workspace_dir())).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidArgument);
        assert!(err.message.contains("公开"));

        // 确认后即可继续（后续会因网络/仓库不可达而失败，但不再是披露错误）。
        connector.acknowledge_disclosure().unwrap();
        let status = connector.status();
        assert!(status.disclosed_public_drafts);
    }

    #[test]
    fn status_reports_missing_workspace_as_not_connected() {
        let (_dir, store) = store();
        let connector = Connector::new(store);
        let status = connector.status();
        assert!(!status.connected);
        assert!(!status.workspace_ready);
        assert_eq!(status.repo_label, DEFAULT_REPO_LABEL);
        assert!(status.public_disclosure.contains("可见"));
    }

    #[test]
    fn disclosure_text_states_drafts_are_public_but_not_on_site() {
        assert!(PUBLIC_DISCLOSURE.contains("草稿"));
        assert!(PUBLIC_DISCLOSURE.contains("可见"));
        assert!(PUBLIC_DISCLOSURE.contains("不会展示"));
    }

    #[test]
    fn workspace_dir_must_be_absolute() {
        assert!(normalize_workspace_dir("").is_err());
        assert!(normalize_workspace_dir("relative/dir").is_err());
        let ok = normalize_workspace_dir("C:/Users/example/ws").unwrap();
        assert!(ok.is_absolute());
    }

    #[test]
    fn refuses_to_reuse_checkout_with_wrong_origin() {
        let (_dir, store) = store();
        let connector = Connector::new(store);
        connector.acknowledge_disclosure().unwrap();

        // 造一个 origin 指向别处的「工作目录」。
        let other = tempfile::tempdir().unwrap();
        git::git(other.path(), &["init", "--initial-branch=main"]).unwrap();
        git::git(other.path(), &["remote", "add", "origin", "https://github.com/someone/other.git"])
            .unwrap();

        let err = connector.connect(Some(other.path().to_path_buf())).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidArgument);
        assert!(err.message.contains("origin"));
    }

    #[test]
    fn refuses_non_empty_non_repo_directory() {
        let (_dir, store) = store();
        let connector = Connector::new(store);
        connector.acknowledge_disclosure().unwrap();

        let target = tempfile::tempdir().unwrap();
        std::fs::write(target.path().join("some-file.txt"), "x").unwrap();
        let err = connector.connect(Some(target.path().to_path_buf())).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidArgument);
        assert!(err.message.contains("非空"));
    }

    #[test]
    fn managed_paths_are_the_two_documented_dirs() {
        let paths = managed_paths();
        assert_eq!(paths, vec!["src/content/blog", "public/blog"]);
    }

    /// P1 回归：拒绝把**开发者手头的检出目录**当成工作区。
    ///
    /// 该目录的 origin 恰好等于默认公开仓库，因此仅凭 origin 校验会放行，
    /// 之后同步/发布/删除会直接作用在这个真实目录上。应用数据目录内的
    /// 自己的 clone 不受此限制。
    #[test]
    fn refuses_development_checkout_outside_app_data() {
        let (dir, store) = store();
        let connector = Connector::new(store.clone_handle());
        connector.acknowledge_disclosure().unwrap();

        // 造一个「开发检出」：origin 指向配置仓库 + 站点源码 + 开发目录标记。
        let checkout = dir.path().join("dev-checkout");
        std::fs::create_dir_all(checkout.join("src")).unwrap();
        std::fs::create_dir_all(checkout.join(".zcode")).unwrap();
        std::fs::write(checkout.join("astro.config.mjs"), "x\n").unwrap();
        git::git(&checkout, &["init", "--initial-branch=main"]).unwrap();
        let config = store.load_config();
        git::git(&checkout, &["remote", "add", "origin", &config.repo_url]).unwrap();

        assert!(looks_like_development_checkout(&checkout));
        let err = connector.connect(Some(checkout.clone())).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidArgument);
        assert!(err.message.contains("开发检出"), "{}", err.message);

        // 应用数据目录内的目录不受此限制。
        assert!(is_inside_app_data(&store, &store.root().join("workspace")));
    }

    /// P1 回归：仓库地址必须是受支持的 Git 远端，而不是任意本机路径。
    #[test]
    fn rejects_unsupported_remote_urls() {
        assert!(is_supported_remote_url("https://github.com/guanlangzg/guanlangzg.github.io.git"));
        assert!(is_supported_remote_url("http://127.0.0.1:8080/repo.git"));
        assert!(is_supported_remote_url("file:///D:/repo"));
        assert!(is_supported_remote_url("git@github.com:guanlangzg/guanlangzg.github.io.git"));
        assert!(!is_supported_remote_url(""));
        assert!(!is_supported_remote_url("D:/some/local/path"));
        assert!(!is_supported_remote_url("../relative/repo"));
        assert!(!is_supported_remote_url("github.com/owner/repo"));
    }

    /// 凭据不得随错误信息回显。
    #[test]
    fn credentials_are_redacted_from_error_details() {
        let redacted =
            git::redact_credentials("https://user:ghp_secret@github.com/owner/repo.git");
        assert_eq!(redacted, "https://***@github.com/owner/repo.git");
        assert!(!redacted.contains("ghp_secret"));
        // 无凭据的 URL 原样返回。
        assert_eq!(
            git::redact_credentials("https://github.com/owner/repo.git"),
            "https://github.com/owner/repo.git"
        );
    }
}
