//! VisRecall 领域层：纯数据与几何，不依赖窗口、抓帧、存储或 UI。
//!
//! 具体实现（`FrameGrabber`/`WindowProbe` 等 trait 及其 Win32 实现）位于 `crates/capture`。

pub mod capture;
pub mod drop;
pub mod error;
pub mod geometry;
pub mod ids;
pub mod item;
pub mod time;

pub use capture::{CapturedImage, DisplayInfo, ForegroundInfo, RawFrame, WindowInfo};
pub use drop::DropContent;
pub use error::AppError;
pub use geometry::{PhysicalPoint, PhysicalRect};
pub use ids::new_item_id;
pub use item::{Item, ItemKind, ItemStatus, NewItem};
pub use time::now_millis;

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
