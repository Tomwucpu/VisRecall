//! 手工验证：抓一帧冻结画面并按显示器铺满覆盖层，然后走完整的框选/提交回读（Task 13）。
//!
//! 流程与 design §5 一致：`set_per_monitor_v2` → 枚举显示器 → 记录前台窗口 →
//! [`GdiFrameGrabber`] 抓帧 → 启动覆盖层线程（预创建隐藏窗口/交换链）→
//! [`visrecall_shell::overlay::OverlayHandle::show`] → 对覆盖窗口 `InvalidateRect`
//! 验证 `WM_PAINT` 重绘路径 → 等待用户框选（`Enter` 提交 / `Esc`、右键取消）→
//! 打印收到的覆盖层事件。
//!
//! 用法：
//! ```text
//! cargo run -p visrecall-shell --example overlay_frozen -- --duration 15
//! ```
//!
//! `--duration` 是等待用户操作的兜底超时（秒）：超时后自动 `cancel`。示例本身不写任何
//! 抓屏文件，Committed 事件只打印尺寸与全局矩形（**不打印像素**）。
//!
//! 手工验证要点：
//! - 拖拽框选后按 `Enter` → 日志出现 `EVENT committed width=… height=… rect=…`，
//!   尺寸应等于所选矩形的物理像素；
//! - `Esc` / 右键 → `EVENT cancelled`，没有 `Committed`；
//! - 拖拽时按住 `Ctrl` 关闭窗口吸附；滚轮在指针下的窗口链中切换高亮；
//! - 方向键 1px 微调、`Shift`+方向键 10px；
//! - 单击（不拖拽）采用悬停窗口边界；选区小于 10×10 时显示「选区过小」并在约 1s 后取消。

use std::process::ExitCode;
use std::time::{Duration, Instant};

use crossbeam_channel::bounded;
use tracing::Level;
use visrecall_capture::{
    enumerate_displays, foreground_info, set_per_monitor_v2, FrameGrabber, GdiFrameGrabber,
};
use visrecall_domain::capture::ForegroundInfo;
use visrecall_domain::AppError;
use visrecall_shell::overlay::{spawn_overlay_thread, OverlayEvent, OVERLAY_CLASS_NAME};
use windows::core::PCWSTR;
use windows::Win32::Graphics::Gdi::InvalidateRect;
use windows::Win32::UI::WindowsAndMessaging::FindWindowW;

/// 默认等待时长（秒）。
const DEFAULT_DURATION_SECS: u64 = 15;
/// show 之后多长时间触发一次重绘验证（留出首帧 Present 的时间）。
const REDRAW_CHECK_DELAY: Duration = Duration::from_millis(500);

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
    // 覆盖层线程的 tracing 日志（窗口/纹理尺寸、首帧耗时、输入失败）输出到 stderr。
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

    // 验证 WM_PAINT 重绘路径：对覆盖窗口 InvalidateRect，覆盖层线程应重新绘制完整场景
    // （冻结帧 + 选区图层）。渲染失败会以 `warn` 出现在 stderr。
    std::thread::sleep(REDRAW_CHECK_DELAY);
    match invalidate_overlay_window() {
        Ok(hwnd) => println!("REDRAW invalidate_sent hwnd=0x{hwnd:X}"),
        Err(err) => println!("REDRAW invalidate_skipped: {err}"),
    }

    println!("WAITING 请在覆盖层上框选（Enter 提交 / Esc、右键取消），最长 {duration}s");

    let deadline = Instant::now() + Duration::from_secs(duration);
    let mut committed = false;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            println!("EVENT missing: {duration}s 内没有提交或取消");
            handle.cancel()?;
            break;
        }
        match events_rx.recv_timeout(remaining) {
            Ok(OverlayEvent::Committed {
                image,
                global_rect,
                foreground,
            }) => {
                // 只打印尺寸/矩形：图像数据可能有几十 MB。
                println!(
                    "EVENT committed width={} height={} rect={}x{}+{}+{} source_process={} source_title={:?}",
                    image.width,
                    image.height,
                    global_rect.width,
                    global_rect.height,
                    global_rect.x,
                    global_rect.y,
                    foreground.process_name,
                    foreground.title
                );
                committed = true;
                break;
            }
            Ok(OverlayEvent::Cancelled) => {
                println!("EVENT cancelled");
                break;
            }
            Err(err) => {
                println!("EVENT recv timeout branch: {err}");
            }
        }
    }

    // 非激活态再次 cancel 也必须回一次 Cancelled，保证调用方不会挂起；
    // 重复 Cancelled 幂等，Task 14 的编排依赖这个契约。
    handle.cancel()?;
    match events_rx.recv_timeout(Duration::from_secs(3)) {
        Ok(OverlayEvent::Cancelled) => {
            println!("EVENT cancelled again (inactive cancel acknowledged)");
        }
        Ok(OverlayEvent::Committed { image, .. }) => {
            println!(
                "EVENT unexpected second commit width={} height={}",
                image.width, image.height
            );
        }
        Err(err) => println!("EVENT missing on inactive cancel: {err}"),
    }

    // 让覆盖层线程处理通道断开（隐藏窗口/恢复焦点/释放 D3D 资源）。
    drop(handle);
    std::thread::sleep(Duration::from_millis(300));
    println!("DONE committed={committed}");
    Ok(())
}

/// 找到覆盖窗口并让整个客户区失效，触发 `WM_PAINT`；返回窗口句柄用于打印。
fn invalidate_overlay_window() -> Result<usize, AppError> {
    let class_name = wide(OVERLAY_CLASS_NAME);
    // SAFETY: 类名以 NUL 结尾且在调用期间存活；只查找窗口，不修改目标进程状态。
    let hwnd =
        unsafe { FindWindowW(PCWSTR(class_name.as_ptr()), PCWSTR::null()) }.map_err(|err| {
            AppError::Internal(format!("FindWindowW({OVERLAY_CLASS_NAME}) failed: {err}"))
        })?;
    // SAFETY: hwnd 属于本进程的覆盖层线程；`InvalidateRect(None, false)` 只标记整块
    // 客户区待重绘，不擦除背景（背景由 D2D 负责）。
    let ok = unsafe { InvalidateRect(Some(hwnd), None, false) };
    if !ok.as_bool() {
        return Err(AppError::Internal(format!(
            "InvalidateRect failed on hwnd=0x{:X}",
            hwnd.0 as usize
        )));
    }
    Ok(hwnd.0 as usize)
}

/// 解析 `--duration <sec>`（等待用户操作的兜底超时，默认 15 秒）。
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

/// UTF-16（NUL 结尾）缓冲区。
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}
