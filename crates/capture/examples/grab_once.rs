//! 抓取一台显示器并导出 PNG，用于人工验证 GDI 抓帧。
//!
//! 运行：`cargo run -p visrecall-capture --example grab_once -- out.png`
//! 参数为输出路径，缺省 `out.png`；优先抓主显示器，没有主屏时抓第一台。
//! 期望：PNG 尺寸与显示器物理分辨率一致，内容不是全黑。

use std::fs;
use std::path::PathBuf;

use visrecall_capture::display::enumerate_displays;
use visrecall_capture::grab_gdi::{is_blank_frame, GdiFrameGrabber};
use visrecall_capture::trait_defs::{FrameGrabber, RawFrameExt};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 先声明 Per-Monitor V2，保证 bounds 是物理像素（与 design §6 一致）。
    visrecall_capture::set_per_monitor_v2();

    let displays = enumerate_displays()?;
    let display = displays
        .iter()
        .find(|d| d.primary)
        .or_else(|| displays.first())
        .ok_or("no displays found")?;

    let path = PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| "out.png".into()));

    let frames = GdiFrameGrabber::new().grab(std::slice::from_ref(display))?;
    let frame = frames
        .into_iter()
        .next()
        .ok_or("grabber returned no frame")?;

    let png = frame.to_png()?;
    fs::write(&path, &png)?;

    let blank = is_blank_frame(&frame);
    println!(
        "display={} bounds=({}, {}) {}x{} stride={} frame_bytes={} png_bytes={} blank={} path={}",
        frame.display_id,
        display.bounds.x,
        display.bounds.y,
        frame.width,
        frame.height,
        frame.stride,
        frame.bgra.len(),
        png.len(),
        blank,
        path.display(),
    );

    if blank {
        return Err("captured frame is all black (suspected DRM or capture failure)".into());
    }
    Ok(())
}
