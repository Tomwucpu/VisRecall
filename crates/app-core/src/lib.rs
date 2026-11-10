//! VisRecall 应用编排层：配置、库句柄、捕获编排、索引管线与事件总线。
//!
//! 本 crate 不依赖 Tauri，便于集成测试；`src-tauri` 负责命令、托盘与窗口生命周期。

pub mod app;
pub mod capture_flow;
pub mod detail;
pub mod directory_import;
pub mod events;
pub mod import_flow;
pub mod import_service;
pub mod pipeline_host;
pub mod search_flow;
pub mod storage_flow;
pub mod thumbnail;

pub use app::{
    gate_message, gate_rejection, handle_shell_event, AppCore, CaptureGate, ModelConfigPatch,
    ModelEntryInput, RelocateResult, SettingsPatchInput,
};
pub use capture_flow::{
    persist_capture, CaptureCoordinator, NoopWindowVisibility, OverlayPresenter, WindowVisibility,
    MIN_FREE_BYTES,
};
pub use detail::{
    load_detail, read_preview, DetailExtras, ItemDetail, BODY_TEXT_LIMIT, PREVIEW_CHARS,
};
pub use directory_import::{
    finish_import_task, DirectoryImportService, DirectoryScanOptions, ALREADY_IMPORTED,
    MAX_DIRECTORY_DEPTH, MAX_DIRECTORY_FILES,
};
pub use events::{
    AppEvent, AppRecoveredEvent, CaptureFailedEvent, EventSink, HotkeyConflictEvent,
    ImportTaskEvent, ImportTaskFinishedEvent, ItemEvent, ItemUpdatedEvent, NullEventSink,
    PipelineStatusEvent, RecordingSink, SemanticStatusEvent, MAX_FINISHED_FAILURES,
};
pub use import_flow::ImportFlow;
pub use import_service::{FileImportReport, ImportFileSummary, ImportService};
pub use pipeline_host::{
    start_pipeline, ProviderRegistry, RoutedText, RoutedVision, StartedPipeline,
};
pub use search_flow::SearchFlow;
pub use storage_flow::{check_quota, prune_expired};
pub use thumbnail::{generate_thumbnail, load_thumb_bytes};
pub use visrecall_domain::AppError;
pub use visrecall_library::paths::AppPaths;
pub use visrecall_library::{
    AppConfig, Library, ModelConfig, ModelEntry, ProviderEntry, MODEL_PROTOCOLS,
};

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
