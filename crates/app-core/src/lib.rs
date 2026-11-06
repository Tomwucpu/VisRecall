//! VisRecall 应用编排层：配置、库句柄、捕获编排与事件总线。
//!
//! 本 crate 不依赖 Tauri，便于集成测试；`src-tauri` 负责命令、托盘与窗口生命周期。

pub mod app;
pub mod capture_flow;
pub mod detail;
pub mod events;
pub mod import_flow;
pub mod storage_flow;
pub mod thumbnail;

pub use app::{
    gate_message, gate_rejection, handle_shell_event, AppCore, CaptureGate, RelocateResult,
    SettingsPatchInput,
};
pub use capture_flow::{
    persist_capture, CaptureCoordinator, NoopWindowVisibility, OverlayPresenter, WindowVisibility,
    MIN_FREE_BYTES,
};
pub use detail::{load_detail, DetailExtras, ItemDetail, BODY_TEXT_LIMIT};
pub use events::{
    AppEvent, AppRecoveredEvent, CaptureFailedEvent, EventSink, HotkeyConflictEvent, ItemEvent,
    ItemUpdatedEvent, NullEventSink, RecordingSink,
};
pub use import_flow::ImportFlow;
pub use storage_flow::{check_quota, prune_expired};
pub use thumbnail::{generate_thumbnail, load_thumb_bytes};
pub use visrecall_domain::AppError;
pub use visrecall_library::paths::AppPaths;
pub use visrecall_library::{AppConfig, Library};

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
