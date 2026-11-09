//! VisRecall 本地存储层：配置、库路径与 `library.json` 元数据，以及 SQLite 素材库
//! （`items`/`jobs`/`settings` 表，阶段 03 起含 `insights`/`items_fts`/向量表）。
//!
//! 文件落盘（图片/文本/缩略图）由上层编排调用 [`files`] 完成；本 crate 只负责元数据
//! 与索引原语（FTS/向量/insights 的读写在各自模块里）。

pub mod cleanup;
pub mod config;
pub mod db;
pub mod files;
pub mod fts;
pub mod hooks;
pub mod insights;
pub mod items;
pub mod jobs;
pub mod migrations;
pub mod paths;
pub mod purge;
pub mod queries_log;
pub mod relocate;
pub mod settings_kv;
pub mod storage;
pub mod tags;
pub mod vector_index;
pub mod vectors;

pub use cleanup::StartupCleanup;
pub use config::{
    AppConfig, BallConfig, CaptureConfig, HotkeyConfig, ModelConfig, PrivacyConfig, ProviderEntry,
};
pub use db::{IndexCounts, Library};
pub use fts::{fts_delete_tx, fts_upsert_tx, FtsColumns, FtsHit};
pub use hooks::MetadataHook;
pub use insights::insight_upsert_tx;
pub use jobs::{Job, JobState};
pub use paths::{ensure_library_root, AppPaths, LibraryMeta};
pub use purge::{DeletedItem, PurgeReport};
pub use queries_log::QueryLog;
pub use vector_index::BruteForceInt8;
pub use vectors::{vector_delete_tx, vector_upsert_tx};
pub use vectors::{VectorRow, DEFAULT_VECTOR_TABLE};

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
