//! 手工验证：抓一帧冻结画面并按显示器铺满覆盖层（Task 11）。
//!
//! 流程与 design §5 一致：`set_per_monitor_v2` → 枚举显示器 → 记录前台窗口 →
//! [`GdiFrameGrabber`] 抓帧 → 启动覆盖层线程（预创建隐藏窗口/交换链）→
//! [`visrecall_shell::overlay::OverlayHandle::show`] → 等待 `--duration` 秒 → `cancel`
//! → 打印收到的覆盖层事件。
//!
//! 用法：
//! ```text
//! cargo run -p visrecall-shell --example overlay_frozen -- --duration 5
//! ```
//!
//! 示例不写任何抓屏文件；每秒日志由覆盖层线程通过 `tracing` 输出（窗口客户区尺寸、
//! 纹理尺寸、`show_to_first_present_ms`）。

use std::process::ExitCode;
use std::time::{Duration, Instant};

use crossbeam_channel::bounded;
use tracing::Level;
use visrecall_capture::{
    enumerate_displays, foreground_info, set_per_monitor_v2, FrameGrabber, GdiFrameGrabber,
};
use visrecall_domain::capture::ForegroundInfo;
use visrecall_domain::AppError;
use visrecall_shell::overlay::{spawn_overlay_thread, OverlayEvent};

/// 默认显示时长（秒）。
const DEFAULT_DURATION_SECS: u64 = 5;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("overlay frozen example failed: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), AppError> {
    let duration = parse_duration(std::env::args().skip(1))?;
    // 覆盖层线程的 tracing 日志（窗口/纹理尺寸、首帧耗时）输出到 stderr。
    let _ = tracing_subscriber::fmt()
        .with_max_level(Level::INFO)
        .try_init();

    // 必须在创建任何窗口前声明 Per-Monitor V2（design §6）。
    set_per_monitor_v2();
    let displays = enumerate_displays()?;
    println!("DISPLAYS count={}", displays.len());
    for (index, display) in displays.iter().enumerate() {
        let bounds = display.bounds;
        println!(
            "DISPLAY index={index} id={} bounds={}x{}+{}+{} dpi={} primary={}",
            display.id,
            bounds.width,
            bounds.height,
            bounds.x,
            bounds.y,
            display.dpi,
            display.primary
        );
    }

    // 覆盖层出现前记录来源窗口；失败时仍允许捕获（design §5 失败分支）。
    let foreground = foreground_info().unwrap_or_else(|err| {
        eprintln!("foreground unavailable: {err}");
        ForegroundInfo::default()
    });
    println!(
        "FOREGROUND hwnd=0x{:X} process={} title={:?}",
        foreground.hwnd, foreground.process_name, foreground.title
    );

    let grab_start = Instant::now();
    let frames = GdiFrameGrabber::new().grab(&displays)?;
    let grab_ms = grab_start.elapsed().as_secs_f64() * 1000.0;
    println!("GRAB frames={} grab_ms={grab_ms:.2}", frames.len());
    for frame in &frames {
        println!(
            "FRAME display={} size={}x{} stride={} bytes={}",
            frame.display_id,
            frame.width,
            frame.height,
            frame.stride,
            frame.bgra.len()
        );
    }

    let (events_tx, events_rx) = bounded(4);
    let spawn_start = Instant::now();
    let handle = spawn_overlay_thread(events_tx)?;
    println!(
        "OVERLAY_READY spawn_init_ms={:.2}",
        spawn_start.elapsed().as_secs_f64() * 1000.0
    );

    let show_start = Instant::now();
    handle.show(frames, displays, foreground)?;
    println!(
        "SHOW_SENT send_ms={:.2}（上传/首帧耗时见覆盖层线程日志）",
        show_start.elapsed().as_secs_f64() * 1000.0
    );

    std::thread::sleep(Duration::from_secs(duration));
    handle.cancel()?;
    match events_rx.recv_timeout(Duration::from_secs(3)) {
        Ok(OverlayEvent::Cancelled) => println!("EVENT cancelled"),
        Ok(OverlayEvent::Committed { .. }) => {
            println!("EVENT committed (阶段 01 的 Task 11 不应发生)")
        }
        Err(err) => println!("EVENT missing: {err}"),
    }

    // 让覆盖层线程处理通道断开（隐藏窗口/恢复焦点/释放 D3D 资源）。
    drop(handle);
    std::thread::sleep(Duration::from_millis(300));
    println!("DONE");
    Ok(())
}

/// 解析 `--duration <sec>`（默认 5 秒）。
fn parse_duration(mut argv: impl Iterator<Item = String>) -> Result<u64, AppError> {
    let mut duration = DEFAULT_DURATION_SECS;
    while let Some(flag) = argv.next() {
        match flag.as_str() {
            "--duration" => {
                let value = argv
                    .next()
                    .ok_or_else(|| AppError::InvalidInput("--duration requires a value".into()))?;
                duration = value.parse::<u64>().map_err(|_| {
                    AppError::InvalidInput(format!(
                        "--duration expects a positive integer, got {value:?}"
                    ))
                })?;
            }
            other => {
                return Err(AppError::InvalidInput(format!(
                    "unknown argument {other:?}; usage: --duration <sec>"
                )))
            }
        }
    }
    if duration == 0 {
        return Err(AppError::InvalidInput(
            "--duration must be greater than zero".into(),
        ));
    }
    Ok(duration)
}
