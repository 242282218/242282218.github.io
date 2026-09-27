//! Tauri IPC 包装层。
//!
//! 这一层只做参数转发与错误序列化，不含业务判断；真正的逻辑在
//! [`crate::app_commands`] 中，因此两者都能被单元测试覆盖。
//!
//! 所有命令都返回 `Result<T, WriterError>`；`WriterError` 结构化序列化后由
//! 前端按错误码给出操作性提示。

use crate::app_commands::{AppState, DeploymentStatus, PreviewSessionInfo, RenameAssessment};
use crate::local_store::{RecoveryDraft, TrashEntry, WritingPreferences};
use crate::model::{
    ArticleContent, ArticleMeta, ArticleStatus, ArticleSummary, ErrorCode, WriterError,
};
use crate::preview::ToolchainReport;
use crate::publish::{PublishOutcome, PublishPrecheck};
use crate::sync::{SyncAssessment, SyncOutcome};
use crate::trash::{DeleteAssessment, DeleteOutcome, WithdrawOutcome};
use tauri::State;

use crate::app_commands as core;

/// 首次连接状态。
#[tauri::command]
pub fn connection_status(state: State<'_, AppState>) -> Result<crate::connection::ConnectionStatus, WriterError> {
    core::connection_status(&state)
}

/// 确认已知悉公开仓库会公开远程草稿。
#[tauri::command]
pub fn acknowledge_disclosure(
    state: State<'_, AppState>,
) -> Result<crate::connection::ConnectionStatus, WriterError> {
    core::acknowledge_disclosure(&state)
}

/// 建立独立工作目录（clone 公开仓库的 main）。
#[tauri::command]
pub fn connect(
    state: State<'_, AppState>,
    workspace_dir: Option<String>,
) -> Result<crate::connection::ConnectionStatus, WriterError> {
    core::connect(&state, workspace_dir)
}

/// 探测 Git / Node / pnpm 前置条件。
#[tauri::command]
pub fn toolchain_report(state: State<'_, AppState>) -> ToolchainReport {
    core::toolchain_report(&state)
}

/// 文章列表。
#[tauri::command]
pub fn list_articles(state: State<'_, AppState>) -> Result<Vec<ArticleSummary>, WriterError> {
    core::list_articles(&state)
}

/// 读取单篇文章。
#[tauri::command]
pub fn read_article(state: State<'_, AppState>, article_id: String) -> Result<ArticleContent, WriterError> {
    core::read_article(&state, article_id)
}

/// 新建文章。
#[tauri::command]
pub fn create_article(
    state: State<'_, AppState>,
    article_id: String,
    meta: ArticleMeta,
    body: String,
) -> Result<ArticleContent, WriterError> {
    core::create_article(&state, article_id, meta, body)
}

/// 本地保存。
#[tauri::command]
pub fn save_article(
    state: State<'_, AppState>,
    article_id: String,
    meta: ArticleMeta,
    body: String,
    updated_date_action: Option<crate::model::UpdatedDateAction>,
) -> Result<ArticleContent, WriterError> {
    core::save_article(&state, article_id, meta, body, updated_date_action)
}

/// 显式导入本地 Markdown。
#[tauri::command]
pub fn import_article(
    state: State<'_, AppState>,
    source_path: String,
    article_id: String,
) -> Result<ArticleContent, WriterError> {
    core::import_article(&state, source_path, article_id)
}

/// 插入图片：校验后归档到 `public/blog/<article-id>/`。
#[tauri::command]
pub fn import_article_image(
    state: State<'_, AppState>,
    article_id: String,
    source_path: String,
) -> Result<core::ImportedImage, WriterError> {
    core::import_article_image(&state, article_id, source_path)
}

/// 插入**已在内存中的**图片字节（剪贴板粘贴、页面内拖入等没有磁盘路径的来源）。
///
/// 用原始请求体承载字节，避免把图片序列化成 JSON 数字数组。文章标识与文件名
/// 通过请求头传递，并按 URL 编码规则解码，从而支持中文文件名。
#[tauri::command]
pub fn import_article_image_bytes(
    state: State<'_, AppState>,
    request: tauri::ipc::Request<'_>,
) -> Result<core::ImportedImage, WriterError> {
    handle_image_bytes_invoke(&state, request.body(), request.headers())
}

/// 处理一次「原始字节插图」请求。
///
/// 与 Tauri 命令分开，使这段解析逻辑（原始体判定、请求头解码、参数校验）
/// 可以被直接单元测试——`tauri::ipc::Request` 无法在测试中构造。
pub fn handle_image_bytes_invoke(
    state: &AppState,
    body: &tauri::ipc::InvokeBody,
    headers: &tauri::http::HeaderMap,
) -> Result<core::ImportedImage, WriterError> {
    let tauri::ipc::InvokeBody::Raw(bytes) = body else {
        return Err(WriterError::new(
            ErrorCode::InvalidArgument,
            "插入图片的请求格式不正确（需要原始字节）",
        ));
    };
    if bytes.is_empty() {
        return Err(WriterError::new(ErrorCode::InvalidArgument, "粘贴或拖入的图片内容为空"));
    }

    let article_id = decode_header(headers, ARTICLE_ID_HEADER)?;
    let file_name = decode_header(headers, FILE_NAME_HEADER)?;
    core::import_article_image_data(state, article_id, &file_name, bytes)
}

/// 承载文章标识的请求头（值经 URL 编码）。
const ARTICLE_ID_HEADER: &str = "x-guanlanzhi-article-id";
/// 承载原始文件名的请求头（值经 URL 编码，支持中文）。
const FILE_NAME_HEADER: &str = "x-guanlanzhi-file-name";

/// 读取并解码请求头。
///
/// HTTP 头只能是 ASCII，因此前端用 `encodeURIComponent` 编码，这里用
/// percent-decoding 还原；解码失败时退回原值，并交由后端的路径校验拦截。
fn decode_header(
    headers: &tauri::http::HeaderMap,
    name: &str,
) -> Result<String, WriterError> {
    let raw = headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| {
            WriterError::new(ErrorCode::InvalidArgument, "插入图片的请求缺少必要信息")
        })?;
    let decoded = percent_encoding::percent_decode_str(raw)
        .decode_utf8()
        .map(|value| value.into_owned())
        .unwrap_or_else(|_| raw.to_string());
    // 头里出现换行等内容属于异常输入，直接拒绝。
    let cleaned = decoded.replace(['\r', '\n'], "").trim().to_string();
    if cleaned.is_empty() {
        return Err(WriterError::new(ErrorCode::InvalidArgument, "插入图片的请求参数为空"));
    }
    Ok(cleaned)
}

/// 列出某篇文章已归档的图片。
#[tauri::command]
pub fn list_article_images(
    state: State<'_, AppState>,
    article_id: String,
) -> Result<Vec<core::ImportedImage>, WriterError> {
    core::list_article_images(&state, article_id)
}

/// 同步前评估（差异界面数据源）。
#[tauri::command]
pub fn assess_sync(state: State<'_, AppState>, article_id: String) -> Result<SyncAssessment, WriterError> {
    core::assess_sync(&state, article_id)
}

/// 同步到写作分支。
#[tauri::command]
pub fn sync_article(
    state: State<'_, AppState>,
    article_id: String,
    adopt_local: bool,
) -> Result<SyncOutcome, WriterError> {
    core::sync_article(&state, article_id, adopt_local)
}

/// 采用远端版本（本地改动先存入恢复副本）。
#[tauri::command]
pub fn adopt_remote(state: State<'_, AppState>, article_id: String) -> Result<ArticleContent, WriterError> {
    core::adopt_remote(&state, article_id)
}

/// 手工合并后保存。
#[tauri::command]
pub fn resolve_conflict_manually(
    state: State<'_, AppState>,
    article_id: String,
    meta: ArticleMeta,
    body: String,
) -> Result<ArticleContent, WriterError> {
    core::resolve_conflict_manually(&state, article_id, meta, body)
}

/// 发布预检。
#[tauri::command]
pub fn publish_precheck(
    state: State<'_, AppState>,
    article_id: String,
) -> Result<PublishPrecheck, WriterError> {
    core::publish_precheck(&state, article_id)
}

/// 发布到网站（main）。
#[tauri::command]
pub fn publish_article(
    state: State<'_, AppState>,
    article_id: String,
    removed_image_paths: Vec<String>,
    remove_old_markdown_path: Option<String>,
) -> Result<PublishOutcome, WriterError> {
    core::publish_article(&state, article_id, removed_image_paths, remove_old_markdown_path)
}

/// 从网站撤下。
#[tauri::command]
pub fn withdraw_article(
    state: State<'_, AppState>,
    article_id: String,
) -> Result<WithdrawOutcome, WriterError> {
    core::withdraw_article(&state, article_id)
}

/// 删除影响评估。
#[tauri::command]
pub fn assess_delete(
    state: State<'_, AppState>,
    article_id: String,
) -> Result<DeleteAssessment, WriterError> {
    core::assess_delete(&state, article_id)
}

/// 删除文件。
#[tauri::command]
pub fn delete_article(
    state: State<'_, AppState>,
    article_id: String,
) -> Result<DeleteOutcome, WriterError> {
    core::delete_article(&state, article_id)
}

/// 重试未完成的多分支删除。
#[tauri::command]
pub fn retry_delete(state: State<'_, AppState>, op_id: String) -> Result<DeleteOutcome, WriterError> {
    core::retry_delete(&state, op_id)
}

/// 从回收区恢复为未发布草稿。
#[tauri::command]
pub fn restore_article(state: State<'_, AppState>, op_id: String) -> Result<ArticleContent, WriterError> {
    core::restore_article(&state, op_id)
}

/// 回收区列表。
#[tauri::command]
pub fn list_trash(state: State<'_, AppState>) -> Result<Vec<TrashEntry>, WriterError> {
    core::list_trash(&state)
}

/// 清空一条回收记录。
#[tauri::command]
pub fn purge_trash(state: State<'_, AppState>, op_id: String) -> Result<(), WriterError> {
    core::purge_trash(&state, op_id)
}

/// 待处理的崩溃恢复副本（只含与磁盘不同的条目）。
#[tauri::command]
pub fn pending_recovery(state: State<'_, AppState>) -> Result<Vec<RecoveryDraft>, WriterError> {
    core::pending_recovery(&state)
}

/// 丢弃一份崩溃恢复副本。
#[tauri::command]
pub fn discard_recovery(state: State<'_, AppState>, article_id: String) -> Result<(), WriterError> {
    core::discard_recovery(&state, article_id)
}

/// 记录一份崩溃恢复副本（编辑过程中轻量防抖调用）。
#[tauri::command]
pub fn snapshot_recovery(
    state: State<'_, AppState>,
    article_id: String,
    meta: ArticleMeta,
    body: String,
) -> Result<(), WriterError> {
    core::snapshot_recovery(&state, article_id, meta, body)
}

/// 读取某篇文章的恢复副本。
#[tauri::command]
pub fn read_recovery(
    state: State<'_, AppState>,
    article_id: String,
) -> Result<Option<RecoveryDraft>, WriterError> {
    core::read_recovery(&state, article_id)
}

/// 用恢复副本覆盖本地文章文件（用户显式确认）。
#[tauri::command]
pub fn restore_recovery(
    state: State<'_, AppState>,
    article_id: String,
) -> Result<ArticleContent, WriterError> {
    core::restore_recovery(&state, article_id)
}

/// URL 改名影响评估。
#[tauri::command]
pub fn assess_rename_url(
    state: State<'_, AppState>,
    old_id: String,
    new_id: String,
) -> Result<RenameAssessment, WriterError> {
    core::assess_rename_url(&state, old_id, new_id)
}

/// 执行 URL 改名。
#[tauri::command]
pub fn rename_article_url(
    state: State<'_, AppState>,
    old_id: String,
    new_id: String,
    confirm_published: bool,
) -> Result<ArticleContent, WriterError> {
    core::rename_article_url(&state, old_id, new_id, confirm_published)
}

/// 读取写作外观偏好。
#[tauri::command]
pub fn get_preferences(state: State<'_, AppState>) -> WritingPreferences {
    core::get_preferences(&state)
}

/// 写入写作外观偏好。
#[tauri::command]
pub fn set_preferences(
    state: State<'_, AppState>,
    preferences: WritingPreferences,
) -> Result<WritingPreferences, WriterError> {
    core::set_preferences(&state, preferences)
}

/// 启动网站预览。
#[tauri::command]
pub fn start_site_preview(
    state: State<'_, AppState>,
    article_id: String,
    simulate_public: bool,
) -> Result<PreviewSessionInfo, WriterError> {
    core::start_site_preview(&state, article_id, simulate_public)
}

/// 关闭网站预览并清理临时目录。
#[tauri::command]
pub fn stop_site_preview(state: State<'_, AppState>) -> Result<(), WriterError> {
    core::stop_site_preview(&state)
}

/// 查询部署状态（只读，无凭据）。
#[tauri::command]
pub fn deployment_status(
    state: State<'_, AppState>,
    article_id: String,
) -> Result<DeploymentStatus, WriterError> {
    core::deployment_status(&state, article_id)
}

/// 文章状态汇总。
#[tauri::command]
pub fn status_overview(state: State<'_, AppState>) -> Result<Vec<ArticleStatus>, WriterError> {
    core::status_overview(&state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tauri::http::HeaderMap;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(
                tauri::http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                tauri::http::HeaderValue::from_str(value).unwrap(),
            );
        }
        map
    }

    #[test]
    fn decodes_percent_encoded_chinese_file_name() {
        // 前端用 encodeURIComponent 编码中文文件名。
        let encoded = percent_encoding::utf8_percent_encode("截图 01.png", percent_encoding::NON_ALPHANUMERIC)
            .to_string();
        let map = headers(&[(FILE_NAME_HEADER, &encoded)]);
        assert_eq!(decode_header(&map, FILE_NAME_HEADER).unwrap(), "截图 01.png");
    }

    #[test]
    fn decodes_ascii_article_id_unchanged() {
        let map = headers(&[(ARTICLE_ID_HEADER, "read-code")]);
        assert_eq!(decode_header(&map, ARTICLE_ID_HEADER).unwrap(), "read-code");
    }

    #[test]
    fn decodes_percent_encoded_article_id_with_slash() {
        let encoded =
            percent_encoding::utf8_percent_encode("notes/first", percent_encoding::NON_ALPHANUMERIC)
                .to_string();
        let map = headers(&[(ARTICLE_ID_HEADER, &encoded)]);
        assert_eq!(decode_header(&map, ARTICLE_ID_HEADER).unwrap(), "notes/first");
    }

    #[test]
    fn missing_or_empty_header_is_rejected() {
        let empty = headers(&[]);
        let err = decode_header(&empty, ARTICLE_ID_HEADER).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidArgument);

        let blank = headers(&[(ARTICLE_ID_HEADER, "%20%20")]);
        let err = decode_header(&blank, ARTICLE_ID_HEADER).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidArgument, "解码后只剩空白应被拒绝");
    }

    #[test]
    fn invalid_percent_encoding_falls_back_to_raw_without_panicking() {
        // 非法的百分号序列：不应 panic，退回原值继续走路径校验。
        // 注意 `%zz%` 本身是合法的 Windows 文件名，因此这里断言的是
        // 「不 panic 且原样保留」，而不是「一定被拒绝」——真正的拒绝
        // 由路径校验按语义完成（见下面的穿越用例）。
        let map = headers(&[(ARTICLE_ID_HEADER, "%zz%")]);
        let decoded = decode_header(&map, ARTICLE_ID_HEADER).unwrap();
        assert_eq!(decoded, "%zz%");
        assert!(!decoded.contains(".."), "解码结果不得引入相对路径片段");
    }

    #[test]
    fn header_values_cannot_smuggle_newlines_into_paths() {
        // 头部本身就禁止换行，这里验证解码后的清理逻辑兜住异常输入。
        let map = headers(&[(FILE_NAME_HEADER, "%0a%0dname.png")]);
        let decoded = decode_header(&map, FILE_NAME_HEADER).unwrap();
        assert!(!decoded.contains('\n'));
        assert!(!decoded.contains('\r'));
    }

    #[test]
    fn traversal_attempts_are_decoded_then_rejected_by_path_validation() {
        let encoded = percent_encoding::utf8_percent_encode(
            "../../secret",
            percent_encoding::NON_ALPHANUMERIC,
        )
        .to_string();
        let map = headers(&[(ARTICLE_ID_HEADER, &encoded)]);
        let decoded = decode_header(&map, ARTICLE_ID_HEADER).unwrap();
        assert_eq!(decoded, "../../secret");
        assert!(
            crate::paths::validate_existing_article_id(&decoded).is_err(),
            "穿越路径必须在解码后被拒绝"
        );
    }

    /// 端到端：走一次完整的「原始字节插图」请求处理。
    ///
    /// 这是剪贴板粘贴与拖入共用的后端入口，覆盖原始体判定、头部解码、
    /// 文件头校验与归档落盘。
    mod invoke {
        use super::*;
        use crate::testkit::{tiny_png, TestEnv};

        /// 与 app_commands 测试一致地构造指向隔离远端的应用状态。
        fn state_for(env: &TestEnv) -> AppState {
            let mut config = env.store.load_config();
            config.workspace_dir = env.path().to_string_lossy().to_string();
            config.connected = true;
            config.repo_url = format!("file://{}", env.remote.to_string_lossy().replace('\\', "/"));
            config.repo_label = "fixture/site".to_string();
            config.disclosed_public_drafts = true;
            env.store.save_config(&config).unwrap();
            AppState::with_store(env.store.clone_handle())
        }

        fn meta(title: &str) -> crate::model::ArticleMeta {
            crate::model::ArticleMeta {
                title: title.to_string(),
                description: format!("{title} 摘要"),
                pub_date: "2026-09-23".to_string(),
                updated_date: None,
                tags: vec!["测试样稿".to_string()],
                draft: true,
            }
        }

        /// 构造一次粘贴请求：原始体 + 编码后的请求头。
        fn paste_request(
            article_id: &str,
            file_name: &str,
            bytes: &[u8],
        ) -> (tauri::ipc::InvokeBody, tauri::http::HeaderMap) {
            let body = tauri::ipc::InvokeBody::Raw(bytes.to_vec());
            let encoded_id =
                percent_encoding::utf8_percent_encode(article_id, percent_encoding::NON_ALPHANUMERIC)
                    .to_string();
            let encoded_name =
                percent_encoding::utf8_percent_encode(file_name, percent_encoding::NON_ALPHANUMERIC)
                    .to_string();
            let map = headers(&[
                (ARTICLE_ID_HEADER, &encoded_id),
                (FILE_NAME_HEADER, &encoded_name),
            ]);
            (body, map)
        }

        #[test]
        fn accepts_raw_bytes_and_archives_with_chinese_name() {
            let env = TestEnv::new();
            let state = state_for(&env);
            crate::app_commands::create_article(
                &state,
                "paste-ipc".to_string(),
                meta("粘贴"),
                String::new(),
            )
            .unwrap();

            let bytes = tiny_png();
            let (body, map) = paste_request("paste-ipc", "截图 01.png", &bytes);
            let image = handle_image_bytes_invoke(&state, &body, &map).unwrap();

            assert!(image.rel_path.starts_with("public/blog/paste-ipc/"), "{}", image.rel_path);
            assert_eq!(image.mime, "image/png");
            // 中文文件名被解码并保留在归档文件名中。
            assert!(image.file_name.contains("截图"), "{}", image.file_name);
            // 字节逐位落盘。
            assert_eq!(std::fs::read(env.path().join(&image.rel_path)).unwrap(), bytes);
        }

        #[test]
        fn rejects_json_body_instead_of_raw_bytes() {
            let env = TestEnv::new();
            let state = state_for(&env);
            crate::app_commands::create_article(
                &state,
                "paste-json".to_string(),
                meta("格式错误"),
                String::new(),
            )
            .unwrap();

            // 前端若误用普通 JSON 调用，应得到明确错误而不是把数字数组当图片。
            let body = tauri::ipc::InvokeBody::Json(serde_json::json!({ "a": 1 }));
            let map = headers(&[
                (ARTICLE_ID_HEADER, "paste-json"),
                (FILE_NAME_HEADER, "x.png"),
            ]);
            let err = handle_image_bytes_invoke(&state, &body, &map).unwrap_err();
            assert_eq!(err.code, ErrorCode::InvalidArgument);
            assert!(err.message.contains("原始字节"));
        }

        #[test]
        fn rejects_empty_body_and_missing_headers() {
            let env = TestEnv::new();
            let state = state_for(&env);
            crate::app_commands::create_article(
                &state,
                "paste-empty".to_string(),
                meta("空内容"),
                String::new(),
            )
            .unwrap();

            // 空字节。
            let empty = tauri::ipc::InvokeBody::Raw(Vec::new());
            let map = headers(&[
                (ARTICLE_ID_HEADER, "paste-empty"),
                (FILE_NAME_HEADER, "x.png"),
            ]);
            let err = handle_image_bytes_invoke(&state, &empty, &map).unwrap_err();
            assert_eq!(err.code, ErrorCode::InvalidArgument);

            // 缺少文章标识头。
            let body = tauri::ipc::InvokeBody::Raw(tiny_png());
            let no_id = headers(&[(FILE_NAME_HEADER, "x.png")]);
            let err = handle_image_bytes_invoke(&state, &body, &no_id).unwrap_err();
            assert_eq!(err.code, ErrorCode::InvalidArgument);

            // 缺少文件名头。
            let no_name = headers(&[(ARTICLE_ID_HEADER, "paste-empty")]);
            let err = handle_image_bytes_invoke(&state, &body, &no_name).unwrap_err();
            assert_eq!(err.code, ErrorCode::InvalidArgument);
        }

        #[test]
        fn enforces_the_same_format_check_as_file_import() {
            let env = TestEnv::new();
            let state = state_for(&env);
            crate::app_commands::create_article(
                &state,
                "paste-fmt".to_string(),
                meta("格式校验"),
                String::new(),
            )
            .unwrap();

            // 伪装成 png 的文本：文件名合法但文件头不对，必须被拒绝且不落盘。
            let (body, map) = paste_request("paste-fmt", "fake.png", b"<html>not an image</html>");
            let err = handle_image_bytes_invoke(&state, &body, &map).unwrap_err();
            assert_eq!(err.code, ErrorCode::ImageUnsupported);
            assert!(
                crate::app_commands::list_article_images(&state, "paste-fmt".to_string())
                    .unwrap()
                    .is_empty(),
                "被拒绝的内容不得落盘"
            );
        }

        #[test]
        fn rejects_traversal_in_article_id_header() {
            let env = TestEnv::new();
            let state = state_for(&env);
            let (body, map) = paste_request("../../escape", "x.png", &tiny_png());
            let err = handle_image_bytes_invoke(&state, &body, &map).unwrap_err();
            assert_eq!(err.code, ErrorCode::ArticleIdInvalid);
        }
    }
}
