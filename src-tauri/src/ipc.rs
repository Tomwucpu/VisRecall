//! Tauri 命令、IPC DTO 与事件发送端。
//!
//! DTO 定义在这里而不是 domain，避免领域层依赖 specta/Tauri。

use serde::{Deserialize, Serialize};
use tauri::Emitter;
use visrecall_app_core::{AppCore, AppError, EventSink};

use visrecall_app_core::AppEvent;

/// IPC 错误 DTO；`code` 为稳定错误码，前端据此分支，`message` 面向日志/提示。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct ApiError {
    pub code: String,
    pub message: String,
}

impl ApiError {
    pub(crate) fn new(code: &str, message: impl Into<String>) -> ApiError {
        ApiError {
            code: code.to_string(),
            message: message.into(),
        }
    }
}

/// [`AppError`] → 稳定错误码。用引用实现是因为 `AppError` 存在
/// `AppCore::library` 的 `Result` 里，不能 clone。
impl From<&AppError> for ApiError {
    fn from(err: &AppError) -> ApiError {
        let (code, message) = match err {
            AppError::ConfigMissing => ("CONFIG_MISSING", err.to_string()),
            AppError::InvalidInput(detail) => ("INVALID_INPUT", detail.clone()),
            AppError::NotFound => ("NOT_FOUND", err.to_string()),
            AppError::StorageFull => ("STORAGE_FULL", err.to_string()),
            AppError::CaptureFailed(detail) => ("CAPTURE_FAILED", detail.clone()),
            AppError::CaptureCancelled => ("CAPTURE_CANCELLED", err.to_string()),
            AppError::DrmProtected => ("DRM_PROTECTED", err.to_string()),
            AppError::Io(detail) => ("IO", detail.clone()),
            AppError::Database(detail) => ("DATABASE", detail.clone()),
            AppError::Internal(detail) => ("INTERNAL", detail.clone()),
        };
        ApiError::new(code, message)
    }
}

/// 列表/详情共用的素材 DTO；`thumb_url` 只在存在缩略图时由后端拼接。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct ItemDto {
    pub id: String,
    pub kind: String,
    pub status: String,
    pub captured_at: i64,
    pub source_app: Option<String>,
    pub source_title: Option<String>,
    pub source_url: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub char_count: Option<u32>,
    pub note: String,
    pub thumb_url: Option<String>,
}

impl From<visrecall_domain::Item> for ItemDto {
    fn from(item: visrecall_domain::Item) -> ItemDto {
        let thumb_url = item
            .thumb_path
            .as_ref()
            .map(|_| format!("visrecall://thumb/{}", item.id));
        ItemDto {
            id: item.id,
            kind: kind_str(item.kind).to_string(),
            status: status_str(item.status).to_string(),
            captured_at: item.captured_at,
            source_app: item.source_app,
            source_title: item.source_title,
            source_url: item.source_url,
            width: item.width,
            height: item.height,
            char_count: item.char_count,
            note: item.note,
            thumb_url,
        }
    }
}

fn kind_str(kind: visrecall_domain::ItemKind) -> &'static str {
    match kind {
        visrecall_domain::ItemKind::Image => "image",
        visrecall_domain::ItemKind::Text => "text",
        visrecall_domain::ItemKind::Link => "link",
    }
}

fn status_str(status: visrecall_domain::ItemStatus) -> &'static str {
    match status {
        visrecall_domain::ItemStatus::Parsing => "parsing",
        visrecall_domain::ItemStatus::Ready => "ready",
        visrecall_domain::ItemStatus::Pending => "pending",
        visrecall_domain::ItemStatus::Failed => "failed",
    }
}

/// `get_app_status` 的返回值；库打不开时 `library_ok = false`，路径仍返回给界面对账。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct AppStatus {
    pub version: String,
    pub library_path: String,
    pub library_ok: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct SettingsDto {
    pub library_path: String,
    pub capture_hotkey: String,
    pub ball_enabled: bool,
    pub ball_size: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct SettingsPatch {
    pub ball_enabled: Option<bool>,
    pub ball_size: Option<u32>,
    pub capture_hotkey: Option<String>,
}

impl SettingsDto {
    fn from_config(config: &visrecall_app_core::AppConfig) -> SettingsDto {
        SettingsDto {
            library_path: config.library_path.display().to_string(),
            capture_hotkey: config.hotkeys.capture.clone(),
            ball_enabled: config.ball.enabled,
            ball_size: config.ball.size,
        }
    }
}

/// 把 `app-core` 的事件转成 Tauri 事件；前端只订阅，不从 Rust 侧读取状态。
pub struct TauriEventSink {
    app: tauri::AppHandle,
}

impl TauriEventSink {
    pub fn new(app: tauri::AppHandle) -> TauriEventSink {
        TauriEventSink { app }
    }
}

impl EventSink for TauriEventSink {
    fn emit(&self, event: AppEvent) {
        let name = event.name();
        if let Err(err) = self.app.emit(name, event.payload()) {
            tracing::warn!(event = name, error = %err, "failed to emit event");
        }
    }
}

/// 所有阶段 01 命令的注册点；命令同时带 `#[tauri::command]` 与 `#[specta::specta]`。
pub fn specta_builder() -> tauri_specta::Builder<tauri::Wry> {
    tauri_specta::Builder::<tauri::Wry>::new()
        .commands(tauri_specta::collect_commands![
            get_app_status,
            open_library_folder,
            list_items,
            get_item,
            get_settings,
            update_settings
        ])
        // 前端用 try/catch 处理 `ApiError`，而不是每处解包 `{ status }`。
        .error_handling(tauri_specta::ErrorHandlingMode::Throw)
        // `captured_at` 是 Unix 毫秒，远小于 2^53，用 number 比 BigInt 更适合前端。
        .dangerously_cast_bigints_to_number()
}

/// 导出 TypeScript 绑定；路径按 `CARGO_MANIFEST_DIR` 解析，不依赖进程工作目录。
#[cfg(debug_assertions)]
pub fn export_bindings(builder: &tauri_specta::Builder<tauri::Wry>) -> Result<(), String> {    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../src/lib/bindings.ts");
    builder
        .export(specta_typescript::Typescript::default(), &path)
        .map_err(|err| format!("export {}: {err}", path.display()))
}

#[tauri::command]
#[specta::specta]
pub fn get_app_status(core: tauri::State<'_, AppCore>) -> AppStatus {
    let config = core.config();
    AppStatus {
        version: env!("CARGO_PKG_VERSION").to_string(),
        library_path: config.library_path.display().to_string(),
        library_ok: core.library_ok(),
    }
}

/// 打开库目录；路径始终来自 `AppCore`，前端不能传路径。
#[tauri::command]
#[specta::specta]
pub fn open_library_folder(
    app: tauri::AppHandle,
    core: tauri::State<'_, AppCore>,
) -> Result<(), ApiError> {
    use tauri_plugin_opener::OpenerExt;

    let root = core.library_root();
    let path = root.display().to_string();
    app.opener()
        .open_path(path.clone(), None::<&str>)
        .map_err(|err| ApiError::new("INTERNAL", format!("open {path}: {err}")))
}

#[tauri::command]
#[specta::specta]
pub fn list_items(
    core: tauri::State<'_, AppCore>,
    cursor: Option<i64>,
    limit: u32,
) -> Result<Vec<ItemDto>, ApiError> {
    let limit = limit.clamp(1, 100);
    let library = core.library().map_err(ApiError::from)?;
    let items = library
        .list_items(cursor, limit)
        .map_err(|err| ApiError::from(&err))?;
    Ok(items.into_iter().map(ItemDto::from).collect())
}

#[tauri::command]
#[specta::specta]
pub fn get_item(core: tauri::State<'_, AppCore>, id: String) -> Result<ItemDto, ApiError> {
    let library = core.library().map_err(ApiError::from)?;
    let item = library
        .get_item(&id)
        .map_err(|err| ApiError::from(&err))?
        .ok_or_else(|| ApiError::new("NOT_FOUND", format!("item {id} not found")))?;
    Ok(ItemDto::from(item))
}

#[tauri::command]
#[specta::specta]
pub fn get_settings(core: tauri::State<'_, AppCore>) -> SettingsDto {
    SettingsDto::from_config(&core.config())
}

#[tauri::command]
#[specta::specta]
pub fn update_settings(
    core: tauri::State<'_, AppCore>,
    patch: SettingsPatch,
) -> Result<SettingsDto, ApiError> {
    let config = core
        .update_settings(patch.ball_enabled, patch.ball_size, patch.capture_hotkey)
        .map_err(|err| ApiError::from(&err))?;
    Ok(SettingsDto::from_config(&config))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_error_maps_stable_codes() {
        let cases = [
            (AppError::ConfigMissing, "CONFIG_MISSING"),
            (AppError::InvalidInput("bad".into()), "INVALID_INPUT"),
            (AppError::NotFound, "NOT_FOUND"),
            (AppError::StorageFull, "STORAGE_FULL"),
            (AppError::CaptureFailed("no display".into()), "CAPTURE_FAILED"),
            (AppError::CaptureCancelled, "CAPTURE_CANCELLED"),
            (AppError::DrmProtected, "DRM_PROTECTED"),
            (AppError::Io("disk".into()), "IO"),
            (AppError::Database("locked".into()), "DATABASE"),
            (AppError::Internal("oops".into()), "INTERNAL"),
        ];
        for (err, code) in cases {
            assert_eq!(ApiError::from(&err).code, code);
        }

        let restart = ApiError::from(&AppError::InvalidInput("restart required".into()));
        assert_eq!(restart.code, "INVALID_INPUT");
        assert_eq!(restart.message, "restart required");
    }

    #[test]
    fn item_dto_builds_thumb_url_only_with_thumb() {
        let base = visrecall_domain::Item {
            id: "id-1".into(),
            kind: visrecall_domain::ItemKind::Image,
            status: visrecall_domain::ItemStatus::Ready,
            captured_at: 42,
            source_app: None,
            source_title: None,
            source_url: None,
            width: Some(10),
            height: Some(20),
            char_count: None,
            original_path: "originals/images/2026/01/a.png".into(),
            thumb_path: None,
            note: String::new(),
            content_hash: "hash".into(),
        };

        let dto = ItemDto::from(base.clone());
        assert_eq!(dto.kind, "image");
        assert_eq!(dto.status, "ready");
        assert_eq!(dto.thumb_url, None);

        let with_thumb = visrecall_domain::Item {
            thumb_path: Some("thumbs/id-1.png".into()),
            ..base
        };
        assert_eq!(
            ItemDto::from(with_thumb).thumb_url.as_deref(),
            Some("visrecall://thumb/id-1")
        );
    }
}
