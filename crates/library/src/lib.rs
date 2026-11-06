//! VisRecall 本地存储层：配置、库路径与 `library.json` 元数据，以及 SQLite 素材库
//! （`items`/`jobs`/`settings` 表）。
//!
//! 文件落盘（图片/文本/缩略图）由上层编排调用 [`files`] 完成；本 crate 只负责元数据。

pub mod cleanup;
pub mod config;
pub mod db;
pub mod files;
pub mod items;
pub mod migrations;
pub mod paths;
pub mod settings_kv;

pub use cleanup::StartupCleanup;
pub use config::{AppConfig, BallConfig, CaptureConfig, HotkeyConfig, PrivacyConfig};
pub use db::Library;
pub use paths::{ensure_library_root, AppPaths, LibraryMeta};

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
