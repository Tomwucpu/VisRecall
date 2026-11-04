//! 打印当前可见窗口与鼠标命中链，用于人工核对 Chrome / 资源管理器等目标窗口。
//!
//! 运行：`cargo run -p visrecall-capture --example list_windows`
//! 把鼠标移到目标窗口上再运行，末尾的 `hit_chain` 应给出该窗口（及桌面/任务栏的整屏回退）。

use visrecall_capture::window::{enumerate_windows, foreground_info, hit_chain};
use visrecall_domain::geometry::PhysicalPoint;
use windows::Win32::Foundation::POINT;
use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 先声明 Per-Monitor V2，保证窗口边界与鼠标坐标都是物理像素。
    visrecall_capture::set_per_monitor_v2();

    match foreground_info() {
        Ok(fg) => println!(
            "foreground: hwnd=0x{:x} pid={} process={} display_name={} title={:?} bounds=({}, {}) {}x{}",
            fg.hwnd,
            fg.pid,
            fg.process_name,
            fg.display_name,
            fg.title,
            fg.bounds.x,
            fg.bounds.y,
            fg.bounds.width,
            fg.bounds.height,
        ),
        Err(err) => println!("foreground: unavailable ({err})"),
    }

    let windows = enumerate_windows()?;
    println!("candidate windows: {}", windows.len());
    for (index, w) in windows.iter().enumerate() {
        println!(
            "[{:>2}] hwnd=0x{:x} process={:<28} class={:<32} tool={} cloaked={} bounds=({}, {}) {}x{} title={:?}",
            index + 1,
            w.hwnd,
            if w.process_name.is_empty() {
                "<unknown>"
            } else {
                &w.process_name
            },
            w.class_name,
            w.tool_window,
            w.cloaked,
            w.bounds.x,
            w.bounds.y,
            w.bounds.width,
            w.bounds.height,
            w.title,
        );
    }

    let mut cursor = POINT::default();
    // SAFETY: `cursor` 指向栈上有效 `POINT`。
    unsafe { GetCursorPos(&mut cursor) }?;
    let point = PhysicalPoint {
        x: cursor.x,
        y: cursor.y,
    };
    println!("cursor=({}, {})", point.x, point.y);

    match hit_chain(point) {
        Ok(chain) => {
            println!("hit_chain depth={}", chain.len());
            for (depth, w) in chain.iter().enumerate() {
                println!(
                    "  [{}] hwnd=0x{:x} process={:<28} class={:<32} bounds=({}, {}) {}x{} title={:?}",
                    depth,
                    w.hwnd,
                    if w.process_name.is_empty() {
                        "<unknown>"
                    } else {
                        &w.process_name
                    },
                    w.class_name,
                    w.bounds.x,
                    w.bounds.y,
                    w.bounds.width,
                    w.bounds.height,
                    w.title,
                );
            }
        }
        Err(err) => println!("hit_chain: unavailable ({err})"),
    }

    Ok(())
}
