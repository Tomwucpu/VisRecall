//! 捕获相关的纯数据类型（显示器、帧、来源窗口、成图）。
//!
//! 抓帧与窗口探针的 trait 实现位于 `crates/capture`，domain 只定义数据形状。

use serde::{Deserialize, Serialize};

use crate::geometry::PhysicalRect;

/// 显示器信息。`bounds`/`work_area` 均为物理像素，副屏可为负坐标。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayInfo {
    /// 设备名，如 `\\.\DISPLAY1`。
    pub id: String,
    pub bounds: PhysicalRect,
    pub work_area: PhysicalRect,
    /// 96/120/144/192 等。
    pub dpi: u32,
    pub primary: bool,
}

/// 单显示器原始帧，top-down BGRA8。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawFrame {
    pub display_id: String,
    pub width: u32,
    pub height: u32,
    /// 每行字节数。
    pub stride: usize,
    pub bgra: Vec<u8>,
}

/// 捕获瞬间的前台窗口信息。`display_name` 为进程显示名，所在显示器由 `bounds` 交集推断。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForegroundInfo {
    pub hwnd: isize,
    pub pid: u32,
    pub process_name: String,
    pub display_name: String,
    pub title: String,
    pub bounds: PhysicalRect,
}

/// 已拼合完成的选区图像，top-down BGRA8。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapturedImage {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

/// 窗口枚举结果，用于窗口吸附与命中链。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowInfo {
    pub hwnd: isize,
    pub title: String,
    pub class_name: String,
    pub process_name: String,
    pub bounds: PhysicalRect,
    pub tool_window: bool,
    pub cloaked: bool,
}
