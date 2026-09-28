//! 观澜志本地写作软件的后端入口。
//!
//! 信任边界：webview 只能调用此处显式注册的文章业务命令，不开放任意 shell、
//! 任意文件系统读写或任意 Git 命令。

pub mod app_commands;
pub mod article_io;
pub mod commands;
pub mod connection;
pub mod crash_session;
pub mod git;
pub mod images;
pub mod ipc;
pub mod local_store;
pub mod model;
pub mod paths;
pub mod preview;
pub mod publish;
pub mod selftest;
pub mod sync;
#[cfg(test)]
pub mod testkit;
pub mod trash;
pub mod util;
pub mod workspace;

/// 命令行入口。
///
/// 除正常启动界面外，支持一个用于诊断的子命令：
///
/// ```text
/// 观澜志写作.exe --self-test [--json]
/// ```
///
/// 它会在系统临时目录中自建一个隔离的本地 Git 远端，完整走一遍
/// 「写作 → 同步到写作分支 → 按篇发布 → 预览流水线」，用于确认本机环境
/// 与安装包的核心功能可用。自检**不接触**用户配置的真实仓库，也不访问网络。
pub fn run() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // 诊断子命令需要可见的 stdout/stderr：release 是 GUI 子系统，没有控制台，
    // 必须先接回父终端（且必须早于任何输出）。无参数即正常启动界面，不需要。
    if !args.is_empty() {
        crate::util::attach_parent_console();
    }
    if args.iter().any(|arg| arg == "--self-test") {
        let report = selftest::run();
        if args.iter().any(|arg| arg == "--json") {
            match serde_json::to_string_pretty(&report) {
                Ok(json) => println!("{json}"),
                Err(err) => eprintln!("无法序列化自检报告：{err}"),
            }
        } else {
            print!("{}", selftest::render_human(&report));
        }
        // 接回终端后 stdout 可能仍是行缓冲；显式冲刷避免退出时丢最后一段输出。
        use std::io::Write;
        let _ = std::io::stdout().flush();
        let _ = std::io::stderr().flush();
        std::process::exit(if report.ok { 0 } else { 1 });
    }
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!("观澜志写作");
        println!("  --self-test [--json]                    在本机隔离环境中自检核心工作流");
        println!("  --crash-session <数据目录>              建立崩溃测试环境并挂起（供外部强杀）");
        println!("  --inspect-recovery <数据目录> [--json]  只读检查恢复副本与远端分支头");
        println!("  --restore-recovery <数据目录> <文章ID>  执行恢复并报告结果");
        println!("  --help                                  显示本帮助");
        return;
    }

    // 崩溃恢复验证用的子命令（由 scripts/test/crash-recovery.mjs 驱动）。
    if let Some(index) = args.iter().position(|arg| arg == "--crash-session") {
        let Some(dir) = args.get(index + 1) else {
            eprintln!("--crash-session 需要一个数据目录参数");
            std::process::exit(2);
        };
        match crash_session::run_crash_session(std::path::Path::new(dir)) {
            Ok(()) => {}
            Err(err) => {
                eprintln!("崩溃会话失败：{}（{}）", err.message, err.code);
                std::process::exit(1);
            }
        }
        return;
    }
    if let Some(index) = args.iter().position(|arg| arg == "--inspect-recovery") {
        let Some(dir) = args.get(index + 1) else {
            eprintln!("--inspect-recovery 需要一个数据目录参数");
            std::process::exit(2);
        };
        let json = args.iter().any(|arg| arg == "--json");
        match crash_session::inspect_recovery(std::path::Path::new(dir)) {
            Ok(report) => {
                if json {
                    println!("{}", serde_json::to_string_pretty(&report).unwrap_or_default());
                } else {
                    println!("恢复副本：{:?}", report.pending_article_ids);
                    println!("副本含未保存正文：{}", report.pending_contains_unsaved_body);
                    println!("磁盘不含未保存正文：{}", report.disk_lacks_unsaved_body);
                    println!("磁盘标题：{:?}", report.disk_title);
                    println!("writing 头：{:?}", report.writing_head);
                    println!("main 头：{:?}", report.main_head);
                }
            }
            Err(err) => {
                eprintln!("检查失败：{}（{}）", err.message, err.code);
                std::process::exit(1);
            }
        }
        return;
    }
    if let Some(index) = args.iter().position(|arg| arg == "--restore-recovery") {
        let (Some(dir), Some(article_id)) = (args.get(index + 1), args.get(index + 2)) else {
            eprintln!("--restore-recovery 需要 <数据目录> <文章ID>");
            std::process::exit(2);
        };
        match crash_session::run_restore_recovery(std::path::Path::new(dir), article_id) {
            Ok(result) => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&result).unwrap_or_default()
                );
            }
            Err(err) => {
                eprintln!("恢复失败：{}（{}）", err.message, err.code);
                std::process::exit(1);
            }
        }
        return;
    }

    let state = app_commands::AppState::new().unwrap_or_else(|err| {
        eprintln!("无法启动：{}", err.message);
        std::process::exit(1);
    });

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            ipc::connection_status,
            ipc::acknowledge_disclosure,
            ipc::connect,
            ipc::toolchain_report,
            ipc::list_articles,
            ipc::read_article,
            ipc::check_article_remote,
            ipc::create_article,
            ipc::save_article,
            ipc::import_article,
            ipc::export_article,
            ipc::import_article_image,
            ipc::import_article_image_bytes,
            ipc::list_article_images,
            ipc::assess_sync,
            ipc::sync_article,
            ipc::adopt_remote,
            ipc::resolve_conflict_manually,
            ipc::publish_precheck,
            ipc::publish_article,
            ipc::withdraw_article,
            ipc::assess_delete,
            ipc::delete_article,
            ipc::retry_delete,
            ipc::restore_article,
            ipc::list_trash,
            ipc::purge_trash,
            ipc::pending_recovery,
            ipc::discard_recovery,
            ipc::snapshot_recovery,
            ipc::read_recovery,
            ipc::restore_recovery,
            ipc::assess_rename_url,
            ipc::rename_article_url,
            ipc::get_preferences,
            ipc::set_preferences,
            ipc::start_site_preview,
            ipc::stop_site_preview,
            ipc::preview_dependency_status,
            ipc::prepare_preview_dependencies,
            ipc::deployment_status,
            ipc::status_overview,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
