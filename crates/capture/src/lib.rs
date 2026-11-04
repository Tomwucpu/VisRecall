//! 捕获层：显示器枚举、GDI 抓帧、窗口探针与选区拼合。
//!
//! 数据形状（[`visrecall_domain::capture`]）在 domain 层定义，本 crate 只放 Win32 实现。

pub mod display;
pub mod grab_gdi;
pub mod trait_defs;
pub mod window;

pub use display::{
    display_for_point, enumerate_displays, normalize_position, set_per_monitor_v2, virtual_bounds,
};
pub use grab_gdi::GdiFrameGrabber;
pub use trait_defs::{FrameGrabber, RawFrame};
pub use window::{
    enumerate_windows, filter_candidates, foreground_info, hit_chain, is_desktop_class,
    Win32WindowProbe, WindowInfo, WindowProbe,
};

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
