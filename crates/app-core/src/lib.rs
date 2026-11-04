//! VisRecall 应用编排层：配置、库句柄与事件总线。
//!
//! 本 crate 不依赖 Tauri，便于集成测试；`src-tauri` 负责命令、托盘与窗口生命周期。

pub mod app;
pub mod events;

pub use app::AppCore;
pub use events::{
    AppEvent, AppRecoveredEvent, CaptureFailedEvent, EventSink, ItemEvent, ItemUpdatedEvent,
    NullEventSink, RecordingSink,
};
pub use visrecall_domain::AppError;
pub use visrecall_library::paths::AppPaths;
pub use visrecall_library::{AppConfig, Library};

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
