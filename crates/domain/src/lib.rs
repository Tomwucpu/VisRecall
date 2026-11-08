//! VisRecall 领域层：纯数据与几何，不依赖窗口、抓帧、存储或 UI。
//!
//! 具体实现（`FrameGrabber`/`WindowProbe` 等 trait 及其 Win32 实现）位于 `crates/capture`。
//! 阶段 03 起本 crate 还持有 AI/检索所需的全部 Provider trait，使 `search` 不依赖 `ai`。

pub mod capture;
pub mod drop;
pub mod error;
pub mod geometry;
pub mod ids;
pub mod insight;
pub mod item;
pub mod providers;
pub mod search;
pub mod storage;
pub mod tag;
pub mod time;

pub use capture::{CapturedImage, DisplayInfo, ForegroundInfo, RawFrame, WindowInfo};
pub use drop::DropContent;
pub use error::AppError;
pub use geometry::{PhysicalPoint, PhysicalRect};
pub use ids::new_item_id;
pub use insight::{ChartType, ContentType, Insight, OcrBox, PrimaryColor, SummarySource, UiType};
pub use item::{Item, ItemKind, ItemStatus, NewItem};
pub use providers::{
    EmbeddingProvider, OcrOutput, OcrProvider, TextInsight, TextProvider, VectorIndex,
    VisionInsight, VisionProvider,
};
pub use storage::{
    OrphanStats, PruneReport, RelocateReport, RetentionPolicy, RetentionPreview, StorageUsage,
    DEFAULT_CAPACITY_GB, MAX_CAPACITY_GB, MAX_RETENTION_DAYS, MIN_CAPACITY_GB, MIN_RETENTION_DAYS,
};
pub use tag::{hue_for, normalize_tag_name, ItemTag, Tag, TagSource, MAX_TAG_CHARS, TAG_HUES};
pub use time::now_millis;

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
