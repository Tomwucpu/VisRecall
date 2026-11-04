//! 打印本机显示器列表，用于人工核对 Windows 显示设置。
//!
//! 运行：`cargo run -p visrecall-capture --example list_displays`
//! 期望：150% 缩放的屏幕 `dpi = 144`，`bounds` 为物理像素，副屏可为负坐标。

use visrecall_capture::display::{enumerate_displays, virtual_bounds};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 先声明 Per-Monitor V2，避免进程在 150% 屏上拿到虚拟化后的坐标。
    visrecall_capture::set_per_monitor_v2();

    let displays = enumerate_displays()?;
    println!("displays: {}", displays.len());
    for (index, d) in displays.iter().enumerate() {
        let b = d.bounds;
        let w = d.work_area;
        println!(
            "[{}] id={} primary={} dpi={} scale={}% bounds=({}, {}) {}x{} work=({}, {}) {}x{}",
            index + 1,
            d.id,
            d.primary,
            d.dpi,
            d.dpi * 100 / 96,
            b.x,
            b.y,
            b.width,
            b.height,
            w.x,
            w.y,
            w.width,
            w.height,
        );
    }

    let v = virtual_bounds(&displays);
    println!("virtual_bounds=({}, {}) {}x{}", v.x, v.y, v.width, v.height);
    Ok(())
}
