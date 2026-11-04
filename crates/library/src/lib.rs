//! VisRecall 本地存储层：配置、库路径与 `library.json` 元数据。
//!
//! 阶段 01 只包含路径/配置/元数据；items、jobs 等 SQLite 表由后续任务加入。

pub mod config;
pub mod files;
pub mod paths;

pub use config::{AppConfig, BallConfig, CaptureConfig, HotkeyConfig, PrivacyConfig};
pub use paths::{ensure_library_root, AppPaths, LibraryMeta};

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
