//! 自检：在本机隔离环境中完整走一遍核心工作流。
//!
//! 目的：让**打包后的可执行文件**能证明「写作 → 同步到写作分支 → 按篇发布 →
//! 预览流水线」这条主路径在本机真实可用，而不只是「进程能启动」。
//!
//! 安全约定（重要）：
//! - 自检**完全不接触**用户配置的真实仓库。它在系统临时目录里自建一个 bare
//!   远端与工作副本，全部操作都发生在那里，结束后整棵删除。
//! - 自检不访问网络，因此**不覆盖** GitHub 认证、GCM 登录与真实推送。
//!   这部分只能由用户在真实仓库上验收。
//! - 为保证提交能在任何机器上完成，自检会在**这个临时仓库**内写一个一次性
//!   提交身份；它不会改动用户的全局 Git 配置。

use crate::app_commands as core;
use crate::local_store::{AppConfig, LocalStore};
use crate::model::{ArticleMeta, ErrorCode, Result, WriterError};
use crate::preview::{PreviewEngine, PreviewOverlay};
use crate::util;
use crate::workspace::Workspace;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// 一个自检步骤的结果。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Step {
    pub name: String,
    pub ok: bool,
    pub detail: String,
}

/// 自检报告。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    /// 所有步骤是否全部通过。
    pub ok: bool,
    pub steps: Vec<Step>,
    /// 本次自检使用的临时目录（结束后已删除）。
    pub temp_root: String,
    /// 明确说明自检覆盖与**未**覆盖的范围。
    pub coverage: Coverage,
}

/// 自检覆盖边界说明。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Coverage {
    pub covered: Vec<String>,
    pub not_covered: Vec<String>,
}

/// 一枚最小合法 PNG（1×1），用于自检中的图片插入步骤。
const SAMPLE_PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, // 签名
    0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52, // IHDR
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
    0x89, //
    0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, // IDAT
    0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, //
    0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82, // IEND
];

/// 运行自检。
pub fn run() -> Report {
    let mut steps: Vec<Step> = Vec::new();
    let root = crate::git::temp_path("self-test");

    // 每个步骤都记录结果；任何失败都不中断，以便一次看到全部问题。
    match run_inner(&root, &mut steps) {
        Ok(()) => {}
        Err(err) => steps.push(Step {
            name: "自检过程".to_string(),
            ok: false,
            detail: format!("{}（{}）", err.message, err.code),
        }),
    }

    // 无论成功失败都清理临时目录，不跟随目录联接。
    let _ = util::remove_dir_all_no_follow(&root);

    Report {
        ok: steps.iter().all(|step| step.ok),
        steps,
        temp_root: root.to_string_lossy().to_string(),
        coverage: coverage(),
    }
}

/// 自检明确覆盖与未覆盖的范围。
fn coverage() -> Coverage {
    Coverage {
        covered: vec![
            "在本机隔离目录中建立 bare 远端与工作副本（全程离线）".to_string(),
            "新建文章、插入图片、本地保存".to_string(),
            "同步到写作分支（隔离索引构造单篇提交）".to_string(),
            "按篇发布到 main（draft 置为 false，仅带出本篇及其图片）".to_string(),
            "发布前的隔离构建闸门（在临时副本中运行站点 test/check/build，失败即停止发布；自检夹具使用无副作用的占位脚本）".to_string(),
            "预览流水线（从 main 导出隔离副本并覆盖当前文章，正式工作区不变）".to_string(),
            "同步不触碰 main、发布不带出其它文章".to_string(),
            "异常关闭后的恢复副本：留下未落盘编辑、能被提示并完整恢复".to_string(),
        ],
        not_covered: vec![
            "GitHub 认证、GCM 登录与真实网络推送（自检使用本地 bare 仓库且不联网）".to_string(),
            "真实站点的构建产物（自检夹具的 test/check/build 是无副作用占位脚本，不代表 Astro 真实构建通过）".to_string(),
            "网站预览服务的真实启动（需要站点依赖；由集成测试与软件界面验收）".to_string(),
            "Pages 部署结论查询（需要真实仓库与网络）".to_string(),
            "界面交互、中文输入法与外观（需要人工验收）".to_string(),
            "进程被外部强制结束（由 scripts/test/crash-recovery.mjs 驱动真实进程验收）".to_string(),
        ],
    }
}

/// 记录一步结果；失败时继续上报，从而停止后续依赖该步骤的操作。
fn step(
    steps: &mut Vec<Step>,
    name: &str,
    outcome: std::result::Result<String, WriterError>,
) -> Result<()> {
    match outcome {
        Ok(detail) => {
            steps.push(Step { name: name.to_string(), ok: true, detail });
            Ok(())
        }
        Err(err) => {
            let message = format!("{}（{}）", err.message, err.code);
            steps.push(Step { name: name.to_string(), ok: false, detail: message.clone() });
            Err(WriterError::new(err.code, format!("步骤「{name}」失败：{message}")))
        }
    }
}

fn run_inner(root: &Path, steps: &mut Vec<Step>) -> Result<()> {
    // ---- 1. 环境检查 ----
    let toolchain = crate::preview::check_toolchain();
    steps.push(Step {
        name: "环境检查".to_string(),
        ok: toolchain.git.available,
        detail: format!(
            "git={}；node={}；pnpm={}{}",
            toolchain.git.version.as_deref().unwrap_or("未找到"),
            toolchain.node.version.as_deref().unwrap_or("未找到"),
            toolchain.pnpm.version.as_deref().unwrap_or("未找到"),
            if toolchain.missing.is_empty() {
                String::new()
            } else {
                format!("；缺少：{}", toolchain.missing.join("、"))
            }
        ),
    });
    if !toolchain.git.available {
        return Err(WriterError::new(
            ErrorCode::ToolchainMissing,
            "未找到 git，无法进行自检",
        ));
    }

    // ---- 2. 建立隔离的本地测试仓库 ----
    let remote = root.join("remote.git");
    let workspace_dir = root.join("workspace");
    let data_dir = root.join("appdata");
    std::fs::create_dir_all(root)
        .map_err(|e| WriterError::new(ErrorCode::IoFailed, format!("创建自检目录失败：{e}")))?;

    let seeded = seed_remote(root, &remote, &workspace_dir);
    step(steps, "建立隔离测试仓库", seeded.map(|()| {
        format!("远端 {}；工作副本 {}", remote.display(), workspace_dir.display())
    }))?;

    // ---- 3. 写入连接配置 ----
    let store = LocalStore::open_at(data_dir.clone())?;
    let mut config = AppConfig::default();
    config.workspace_dir = workspace_dir.to_string_lossy().to_string();
    config.repo_url = format!("file://{}", remote.to_string_lossy().replace('\\', "/"));
    config.connected = true;
    config.disclosed_public_drafts = true;
    store.save_config(&config)?;
    steps.push(Step {
        name: "写入连接配置".to_string(),
        ok: true,
        detail: format!("目标仓库 {}", config.repo_label),
    });

    let state = core::AppState::with_store(store.clone_handle());

    // ---- 4. 写作：新建 + 插图 + 保存 ----
    let article_id = "self-test-article";
    let meta = ArticleMeta {
        title: "自检样稿".to_string(),
        description: "由软件自检生成的样稿，不对应真实写作内容。".to_string(),
        pub_date: "2026-01-01".to_string(),
        updated_date: None,
        tags: vec!["自检".to_string()],
        draft: true,
    };
    let writing = (|| -> Result<String> {
        core::create_article(&state, article_id.to_string(), meta.clone(), String::new())?;
        let image = core::import_article_image_data(
            &state,
            article_id.to_string(),
            "自检图片.png",
            SAMPLE_PNG,
        )?;
        let body = format!("自检正文。\n\n![自检图片]({})\n", image.url);
        let saved = core::save_article(&state, article_id.to_string(), meta.clone(), body, None)?;
        if !saved.meta.draft {
            return Err(WriterError::new(
                ErrorCode::InvalidArgument,
                "新建文章应为未发布草稿",
            ));
        }
        Ok(format!(
            "已新建、插入图片（{} 字节）并保存，正文 {} 字",
            image.size,
            saved.body.chars().count()
        ))
    })();
    step(steps, "写作：新建、插图、本地保存", writing)?;

    // ---- 5. 同步到写作分支 ----
    let synced = (|| -> Result<String> {
        let outcome = core::sync_article(&state, article_id.to_string(), false)?;
        let commit = outcome.pushed_commit.clone().ok_or_else(|| {
            WriterError::new(ErrorCode::GitFailed, "同步未产生提交")
        })?;
        // 核实写作分支确实有这篇与其图片。
        let workspace = Workspace::open(workspace_dir.clone())?;
        let engine = crate::sync::SyncEngine::new(&workspace, &store, &config.repo_url);
        let head = engine
            .fetch(crate::sync::WRITING_BRANCH)?
            .ok_or_else(|| WriterError::new(ErrorCode::GitFailed, "写作分支不存在"))?;
        let markdown = engine.markdown_at(&head, article_id)?.ok_or_else(|| {
            WriterError::new(ErrorCode::GitFailed, "写作分支上没有该文章")
        })?;
        let images = engine.image_hashes_at(&head, article_id)?;
        if images.is_empty() {
            return Err(WriterError::new(ErrorCode::GitFailed, "写作分支上没有该文章的图片"));
        }
        if !markdown.contains("draft: true") {
            return Err(WriterError::new(
                ErrorCode::GitFailed,
                "写作分支应保留工作稿的 draft: true",
            ));
        }
        Ok(format!(
            "已推送到写作分支（{}），含 {} 张图片",
            &commit[..8.min(commit.len())],
            images.len()
        ))
    })();
    step(steps, "同步到写作分支", synced)?;

    // 同步不得改动 main。
    let main_untouched = (|| -> Result<String> {
        let workspace = Workspace::open(workspace_dir.clone())?;
        let engine = crate::sync::SyncEngine::new(&workspace, &store, &config.repo_url);
        let main_head = engine
            .fetch(crate::sync::MAIN_BRANCH)?
            .ok_or_else(|| WriterError::new(ErrorCode::GitFailed, "main 分支不存在"))?;
        if engine.markdown_at(&main_head, article_id)?.is_some() {
            return Err(WriterError::new(
                ErrorCode::GitFailed,
                "同步不得让文章出现在 main 上",
            ));
        }
        Ok("main 上没有该文章（同步只改写作分支）".to_string())
    })();
    step(steps, "校验：同步不触碰 main", main_untouched)?;

    // ---- 6. 按篇发布 ----
    let published = (|| -> Result<String> {
        let precheck = core::publish_precheck(&state, article_id.to_string())?;
        let outcome = core::publish_article(&state, article_id.to_string(), Vec::new(), None)?;
        // 变更清单必须只涉及本篇。
        let markdown_rel = format!("{}{}.md", crate::paths::BLOG_DIR_PREFIX, article_id);
        if outcome.changed_paths.is_empty() {
            return Err(WriterError::new(ErrorCode::GitFailed, "发布未产生任何变更"));
        }
        for path in &outcome.changed_paths {
            let owned = path == &markdown_rel
                || path.starts_with(&format!("{}{}/", crate::paths::IMAGE_DIR_PREFIX, article_id));
            if !owned {
                return Err(WriterError::new(
                    ErrorCode::GitFailed,
                    format!("发布带出了不属于本篇的路径：{path}"),
                ));
            }
        }
        // 核实 main 上该文已公开。
        let workspace = Workspace::open(workspace_dir.clone())?;
        let engine = crate::sync::SyncEngine::new(&workspace, &store, &config.repo_url);
        let main_head = engine
            .fetch(crate::sync::MAIN_BRANCH)?
            .ok_or_else(|| WriterError::new(ErrorCode::GitFailed, "main 分支不存在"))?;
        let text = engine
            .markdown_at(&main_head, article_id)?
            .ok_or_else(|| WriterError::new(ErrorCode::GitFailed, "main 上没有该文章"))?;
        if !text.contains("draft: false") {
            return Err(WriterError::new(
                ErrorCode::GitFailed,
                "发布后 main 上的文章应为 draft: false",
            ));
        }
        let images = engine.image_hashes_at(&main_head, article_id)?;
        Ok(format!(
            "已发布 {} 个路径（{}），main 上为 draft: false，含 {} 张图片{}",
            outcome.changed_paths.len(),
            &outcome.commit[..8.min(outcome.commit.len())],
            images.len(),
            if precheck.differs_from_online { "" } else { "（内容与在线一致）" }
        ))
    })();
    step(steps, "按篇发布到 main", published)?;

    // ---- 7. 预览流水线 ----
    let preview = (|| -> Result<String> {
        let workspace = Workspace::open(workspace_dir.clone())?;
        let engine = PreviewEngine::new(
            &workspace,
            root.join("preview"),
            &config.repo_url,
        );

        // 先记录正式工作区中文章与图片的内容，预览后必须**逐字节不变**。
        let article_path = workspace_dir.join("src/content/blog/self-test-article.md");
        let before_article = std::fs::read(&article_path).unwrap_or_default();
        let before_images = workspace_dir.join("public/blog/self-test-article");

        let worktree = engine.prepare_worktree("self-test")?;
        if !worktree.join("SITE.md").exists() {
            return Err(WriterError::new(
                ErrorCode::PreviewFailed,
                "预览副本未包含仓库中的已提交文件",
            ));
        }
        // 工作区里放一个未提交文件，验证它不会进入预览副本。
        std::fs::write(workspace_dir.join("UNCOMMITTED.txt"), "未提交").ok();

        let overlay = PreviewOverlay {
            article_id: article_id.to_string(),
            markdown: "---\ntitle: x\ndescription: y\npubDate: \"2026-01-01\"\ndraft: true\n---\n\n正文\n"
                .to_string(),
            images: BTreeMap::new(),
            simulate_public: true,
        };
        engine.apply_overlay(&worktree, &overlay)?;

        // 覆盖只发生在副本里：副本中已模拟公开。
        let written =
            std::fs::read_to_string(worktree.join("src/content/blog/self-test-article.md"))
                .map_err(|e| {
                    WriterError::new(
                        ErrorCode::PreviewFailed,
                        format!("无法读取预览副本中的文章：{e}"),
                    )
                })?;
        if !written.contains("draft: false") {
            return Err(WriterError::new(
                ErrorCode::PreviewFailed,
                "临时副本中的模拟公开未生效",
            ));
        }
        if worktree.join("UNCOMMITTED.txt").exists() {
            return Err(WriterError::new(
                ErrorCode::PreviewFailed,
                "未提交文件不应进入预览副本",
            ));
        }

        // 正式工作区逐字节未被预览改动（文章仍是工作稿 draft: true）。
        let after_article = std::fs::read(&article_path).unwrap_or_default();
        if after_article != before_article {
            return Err(WriterError::new(
                ErrorCode::PreviewFailed,
                "预览不得改动正式工作区中的文章",
            ));
        }
        let after_text = String::from_utf8_lossy(&after_article);
        if !after_text.contains("draft: true") {
            return Err(WriterError::new(
                ErrorCode::PreviewFailed,
                "正式工作区中的文章应仍是工作稿（draft: true）",
            ));
        }
        // 正式工作区中的专属图片目录也未被预览删除或改写。
        if workspace_dir.join("public/blog/self-test-article").exists() != before_images.exists() {
            return Err(WriterError::new(
                ErrorCode::PreviewFailed,
                "预览不得改动正式工作区中的图片目录",
            ));
        }
        // 清理自检写入的未提交文件。
        let _ = std::fs::remove_file(workspace_dir.join("UNCOMMITTED.txt"));

        // 依赖可用性只作**说明**，不计入通过条件：自检刻意不访问网络，
        // 因此这里只检查工作区是否已有可复用的依赖，不主动安装。
        let deps = if engine.link_dependencies(&worktree).unwrap_or(false) {
            "已复用工作区的 node_modules"
        } else {
            "工作区尚无 node_modules；启动真实预览前需要先安装依赖"
        };
        util::remove_dir_all_no_follow(&worktree).ok();
        Ok(format!(
            "已从 main 导出隔离副本、覆盖当前文章并在副本中模拟公开，正式工作区未改动；{deps}"
        ))
    })();
    step(steps, "预览流水线（隔离副本与覆盖）", preview)?;

    // ---- 8. 异常关闭后的恢复副本 ----
    let recovery = (|| -> Result<String> {
        let crash_id = "self-test-crash";
        let saved = ArticleMeta {
            title: "自检崩溃样稿".to_string(),
            description: "自检用的崩溃恢复样稿。".to_string(),
            pub_date: "2026-01-01".to_string(),
            updated_date: None,
            tags: vec![],
            draft: true,
        };
        // 已存在时（重复运行）继续即可。
        match core::create_article(
            &state,
            crash_id.to_string(),
            saved.clone(),
            "已保存的正文\n".to_string(),
        ) {
            Ok(_) => {}
            Err(err) if err.code == ErrorCode::ArticleExists => {}
            Err(err) => return Err(err),
        }

        // 记录远端头，用于确认恢复过程不触发任何远端提交。
        let workspace = Workspace::open(workspace_dir.clone())?;
        let engine = crate::sync::SyncEngine::new(&workspace, &store, &config.repo_url);
        let writing_before = engine.fetch(crate::sync::WRITING_BRANCH)?;
        let main_before = engine.fetch(crate::sync::MAIN_BRANCH)?;

        // 模拟「未落盘的编辑」：只写恢复副本，不改文章文件。
        let unsaved_title = "自检崩溃样稿（未保存的标题）";
        core::snapshot_recovery(
            &state,
            crash_id.to_string(),
            ArticleMeta { title: unsaved_title.to_string(), ..saved },
            "未保存的正文\n".to_string(),
        )?;

        // 应被提示。
        let pending = core::pending_recovery(&state)?;
        if !pending.iter().any(|draft| draft.article_id == crash_id) {
            return Err(WriterError::new(ErrorCode::IoFailed, "未提示未保存的恢复副本"));
        }
        // 磁盘仍是旧标题。
        let disk = Workspace::open(workspace_dir.clone())?
            .read(crash_id, &BTreeMap::new())?;
        if disk.meta.title != "自检崩溃样稿" {
            return Err(WriterError::new(ErrorCode::IoFailed, "崩溃前内容不应已落盘"));
        }

        // 恢复：标题与正文都回来。
        let restored = core::restore_recovery(&state, crash_id.to_string())?;
        if restored.meta.title != unsaved_title {
            return Err(WriterError::new(ErrorCode::IoFailed, "恢复后标题不正确"));
        }
        if !restored.body.contains("未保存的正文") {
            return Err(WriterError::new(ErrorCode::IoFailed, "恢复后正文不正确"));
        }
        // 恢复后不再提示。
        if core::pending_recovery(&state)?
            .iter()
            .any(|draft| draft.article_id == crash_id)
        {
            return Err(WriterError::new(ErrorCode::IoFailed, "恢复后仍提示未保存内容"));
        }
        // 恢复不产生远端提交。
        let engine = crate::sync::SyncEngine::new(&workspace, &store, &config.repo_url);
        if engine.fetch(crate::sync::WRITING_BRANCH)? != writing_before {
            return Err(WriterError::new(ErrorCode::GitFailed, "恢复不应改动写作分支"));
        }
        if engine.fetch(crate::sync::MAIN_BRANCH)? != main_before {
            return Err(WriterError::new(ErrorCode::GitFailed, "恢复不应改动 main"));
        }
        Ok("未落盘的编辑被提示、可完整恢复，且两分支均无新提交".to_string())
    })();
    step(steps, "异常关闭后的恢复副本", recovery)?;

    Ok(())
}

/// 在 `root` 下建立一个 bare 远端与一个从它克隆的工作副本。
///
/// 使用一次性的提交身份，确保在任何机器上都能完成提交；用户的全局 Git
/// 配置不会被改动。
fn seed_remote(root: &Path, remote: &Path, workspace: &Path) -> Result<()> {
    let io = |e: std::io::Error| WriterError::new(ErrorCode::IoFailed, format!("创建自检仓库失败：{e}"));

    // bare 远端
    std::fs::create_dir_all(remote).map_err(io)?;
    crate::git::git(remote, &["init", "--bare", "--initial-branch=main"])?;

    // 种子工作副本
    let seed = root.join("seed");
    std::fs::create_dir_all(seed.join("src/content/blog")).map_err(io)?;
    std::fs::create_dir_all(seed.join("public/blog")).map_err(io)?;
    std::fs::write(
        seed.join("package.json"),
        // 自检夹具需要满足「发布前隔离构建闸门」：test/check/build 三个脚本
        // 都要能跑通。这里用无副作用的占位脚本，既不联网也不产生真实构建产物。
        "{\n  \"name\": \"self-test-site\",\n  \"private\": true,\n  \"scripts\": {\n    \"test\": \"node -e \\\"process.exit(0)\\\"\",\n    \"check\": \"node -e \\\"process.exit(0)\\\"\",\n    \"build\": \"node -e \\\"process.exit(0)\\\"\"\n  }\n}\n",
    )
    .map_err(io)?;
    std::fs::write(seed.join("SITE.md"), "自检站点骨架\n").map_err(io)?;
    std::fs::write(seed.join("src/content/blog/.gitkeep"), "").map_err(io)?;

    crate::git::git(&seed, &["init", "--initial-branch=main"])?;
    set_local_identity(&seed)?;
    crate::git::git(&seed, &["add", "-A"])?;
    crate::git::git(&seed, &["commit", "-m", "自检站点骨架"])?;
    crate::git::git(
        &seed,
        &["remote", "add", "origin", &format!("file://{}", remote.to_string_lossy().replace('\\', "/"))],
    )?;
    crate::git::git(&seed, &["push", "origin", "main"])?;

    // 工作副本：从远端克隆，模拟软件首次连接建立独立工作目录。
    let remote_url = format!("file://{}", remote.to_string_lossy().replace('\\', "/"));
    crate::git::git(
        root,
        &[
            "clone",
            "--branch",
            "main",
            "--single-branch",
            &remote_url,
            &workspace.to_string_lossy(),
        ],
    )?;
    set_local_identity(workspace)?;
    Ok(())
}

/// 在指定仓库内写入一次性提交身份（不改动全局配置）。
fn set_local_identity(repo: &Path) -> Result<()> {
    crate::git::git(repo, &["config", "user.name", "观澜志自检"])?;
    crate::git::git(repo, &["config", "user.email", "self-test@example.invalid"])?;
    crate::git::git(repo, &["config", "commit.gpgsign", "false"])?;
    Ok(())
}

/// 把报告渲染为面向人的文本。
pub fn render_human(report: &Report) -> String {
    let mut out = String::new();
    out.push_str("观澜志写作 · 自检\n");
    out.push_str("==================\n\n");
    for (index, item) in report.steps.iter().enumerate() {
        out.push_str(&format!(
            "{:>2}. [{}] {}\n      {}\n",
            index + 1,
            if item.ok { "通过" } else { "失败" },
            item.name,
            item.detail
        ));
    }
    out.push_str(&format!(
        "\n结果：{}\n\n本次自检覆盖：\n",
        if report.ok { "全部通过" } else { "存在失败项" }
    ));
    for item in &report.coverage.covered {
        out.push_str(&format!("  · {item}\n"));
    }
    out.push_str("\n本次自检**未**覆盖：\n");
    for item in &report.coverage.not_covered {
        out.push_str(&format!("  · {item}\n"));
    }
    out
}

/// 定位自检用的临时根目录（便于外部脚本核对清理）。
pub fn temp_root_for(name: &str) -> PathBuf {
    crate::git::temp_path(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn self_test_passes_on_this_machine() {
        // 自检本身就是端到端验证：真实 git、真实本地远端、真实提交与推送。
        let report = run();
        let rendered = render_human(&report);
        assert!(
            report.ok,
            "自检应全部通过；实际输出：\n{rendered}"
        );
        // 关键步骤都必须出现且通过。
        for expected in [
            "环境检查",
            "建立隔离测试仓库",
            "写作：新建、插图、本地保存",
            "同步到写作分支",
            "校验：同步不触碰 main",
            "按篇发布到 main",
            "预览流水线（隔离副本与覆盖）",
        ] {
            let found = report.steps.iter().find(|s| s.name == expected);
            let step = found.unwrap_or_else(|| panic!("缺少步骤：{expected}"));
            assert!(step.ok, "步骤「{expected}」应通过，实际：{}", step.detail);
        }
    }

    #[test]
    fn self_test_cleans_up_its_temp_root() {
        let report = run();
        assert!(
            !Path::new(&report.temp_root).exists(),
            "自检结束后应删除临时目录：{}",
            report.temp_root
        );
    }

    #[test]
    fn coverage_states_what_is_not_verified() {
        let report = run();
        // 必须诚实列出未覆盖项，尤其是真实 GitHub 通路。
        let joined = report.coverage.not_covered.join("\n");
        assert!(joined.contains("GitHub 认证"), "{joined}");
        assert!(joined.contains("网络推送"), "{joined}");
        assert!(joined.contains("预览服务的真实启动"), "{joined}");
    }
}
