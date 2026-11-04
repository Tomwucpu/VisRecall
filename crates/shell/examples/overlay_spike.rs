//! D3D11 覆盖层门禁 spike（Task 10）。
//!
//! 为每台显示器（或每个合成窗口）创建一个 `WS_POPUP` 置顶窗口 + FLIP_SEQUENTIAL
//! 交换链，用 Direct2D 画深色背景、白色中文标签和一个每帧移动的矩形，循环
//! `Present(1, 0)`。全程在单线程创建/使用 D3D11、D2D 与 COM（design §3）。
//!
//! 用法：
//! ```text
//! cargo run --release -p visrecall-shell --example overlay_spike -- \
//!     [--duration <sec>] [--width <px>] [--height <px>] [--windows <n>]
//! ```
//!
//! 无参数时窗口取第一台显示器的原生物理分辨率；`Esc` 退出。
//! 输出一行机器可读的 `SPIKE_RESULT ...`，另有每窗口的 `SPIKE_WINDOW ...` 明细。
//! 帧时间用进程内 `Instant`（QPC）测量，跳过前 [`WARMUP_FRAMES`] 帧。
//! 首帧时间 = `main` 开始 → 第一次成功 `Present`。

use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use visrecall_capture::{enumerate_displays, set_per_monitor_v2};
use visrecall_domain::AppError;
use visrecall_shell::overlay::{D3dContext, Renderer};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{SetFocus, VK_ESCAPE};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect, LoadCursorW,
    PeekMessageW, PostQuitMessage, RegisterClassW, SetForegroundWindow, ShowWindow,
    TranslateMessage, CS_HREDRAW, CS_VREDRAW, HICON, IDC_ARROW, MSG, PM_REMOVE, SW_SHOW,
    WINDOW_EX_STYLE, WM_CLOSE, WM_DESTROY, WM_ERASEBKGND, WM_KEYDOWN, WM_QUIT, WNDCLASSW,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
};

/// 进程内起点；`main` 第一条语句写入，用于计算首帧时间。
static PROCESS_START: OnceLock<Instant> = OnceLock::new();

/// 窗口过程把退出请求广播给主循环。
static QUIT: AtomicBool = AtomicBool::new(false);

/// 窗口类名（design §7.1 的生产类名是 `VisRecallOverlay`，spike 用独立类名）。
const CLASS_NAME: &str = "VisRecallOverlaySpike";
const WINDOW_TITLE: &str = "VisRecall Overlay Spike";
const LABEL: &str = "VisRecall Overlay Spike · 已冻结";
/// 前 60 帧用于预热（着色器/字体/合成器缓存），不计入统计。
const WARMUP_FRAMES: u64 = 60;
/// 帧时间超过该值即记为丢帧。
const DROP_THRESHOLD_MS: f64 = 20.0;
/// 多窗口时每多一个窗口向右下偏移的物理像素。
const WINDOW_STAGGER_PX: i32 = 48;

fn main() -> ExitCode {
    let start = *PROCESS_START.get_or_init(Instant::now);

    let args = match Args::parse(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };

    match run(&args, start) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("overlay spike failed: {err}");
            ExitCode::FAILURE
        }
    }
}

/// 命令行参数；未给出的尺寸回退到第一台显示器的原生分辨率。
#[derive(Debug)]
struct Args {
    duration: u64,
    width: Option<u32>,
    height: Option<u32>,
    windows: u32,
}

impl Args {
    fn parse(mut argv: impl Iterator<Item = String>) -> Result<Args, String> {
        let mut args = Args {
            duration: 30,
            width: None,
            height: None,
            windows: 1,
        };

        while let Some(flag) = argv.next() {
            let mut value = || {
                argv.next()
                    .ok_or_else(|| format!("{flag} requires a value"))
            };
            let number = |raw: String, flag: &str| -> Result<u32, String> {
                raw.parse::<u32>()
                    .map_err(|_| format!("{flag} expects a positive integer, got {raw:?}"))
            };
            match flag.as_str() {
                "--duration" => args.duration = u64::from(number(value()?, "--duration")?),
                "--width" => args.width = Some(number(value()?, "--width")?),
                "--height" => args.height = Some(number(value()?, "--height")?),
                "--windows" => args.windows = number(value()?, "--windows")?,
                other => {
                    return Err(format!(
                        "unknown argument {other:?}; usage: --duration <sec> --width <px> \
                         --height <px> --windows <n>"
                    ))
                }
            }
        }

        if args.duration == 0 {
            return Err("--duration must be greater than zero".into());
        }
        if args.windows == 0 {
            return Err("--windows must be greater than zero".into());
        }
        Ok(args)
    }
}

/// COM apartment 守卫：离开作用域时 `CoUninitialize`。
struct ComGuard;

impl ComGuard {
    fn init() -> Result<ComGuard, AppError> {
        // SAFETY: 单线程 apartment 初始化；`None` 表示默认内存分配器。
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
            .ok()
            .map_err(|err| AppError::Internal(format!("CoInitializeEx failed: {err}")))?;
        Ok(ComGuard)
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        // SAFETY: 与 `CoInitializeEx` 成对调用，只在同一线程。
        unsafe { CoUninitialize() };
    }
}

fn run(args: &Args, start: Instant) -> Result<(), AppError> {
    // 必须在创建任何窗口前声明 Per-Monitor V2（design §6）。
    set_per_monitor_v2();
    let _com = ComGuard::init()?;

    let displays = enumerate_displays()?;
    let display = &displays[0];
    let bounds = display.bounds;
    let width = args.width.unwrap_or(bounds.width);
    let height = args.height.unwrap_or(bounds.height);
    println!(
        "SPIKE_ENV display={} bounds={}x{}+{}+{} dpi={} width={} height={} windows={} duration_s={}",
        display.id, bounds.width, bounds.height, bounds.x, bounds.y, display.dpi, width, height,
        args.windows, args.duration
    );

    let module = unsafe { GetModuleHandleW(None) }
        .map_err(|err| AppError::Internal(format!("GetModuleHandleW failed: {err}")))?;
    let hinstance = HINSTANCE(module.0);
    register_window_class(hinstance)?;

    let mut windows = Vec::with_capacity(args.windows as usize);
    for index in 0..args.windows {
        let offset = index as i32 * WINDOW_STAGGER_PX;
        let x = bounds.x + offset;
        let y = bounds.y + offset;
        let hwnd = create_overlay_window(hinstance, x, y, width, height)?;
        windows.push((hwnd, x, y));
    }

    // 记录实际客户区尺寸：合成 4K 场景下窗口大于屏幕，合成路径仍按该尺寸渲染。
    for (index, (hwnd, x, y)) in windows.iter().enumerate() {
        let mut client = RECT::default();
        // SAFETY: hwnd 刚创建，`client` 是栈上有效指针。
        unsafe { GetClientRect(*hwnd, &mut client) }
            .map_err(|err| AppError::Internal(format!("GetClientRect failed: {err}")))?;
        println!(
            "SPIKE_WINDOW_GEOMETRY index={index} x={x} y={y} client={}x{}",
            client.right - client.left,
            client.bottom - client.top
        );
    }

    // 覆盖层需要键盘焦点（design §7.1），焦点给第一个窗口。
    if let Some(&(first, _, _)) = windows.first() {
        unsafe {
            let _ = SetForegroundWindow(first);
            let _ = SetFocus(Some(first));
        }
    }

    let device = D3dContext::new()?;
    println!(
        "SPIKE_DEVICE driver={} adapter={}",
        device.driver_name(),
        device.adapter_name()
    );
    let mut renderers = Vec::with_capacity(windows.len());
    for &(hwnd, _, _) in &windows {
        let swap = device.create_swap_chain(hwnd, width, height)?;
        renderers.push(Renderer::new(&device, swap)?);
    }

    let measurement = measure(&mut renderers, args.duration, start)?;

    report(&measurement, width, height, args, device.driver_name());

    // 先释放渲染资源/交换链，再销毁窗口。
    drop(renderers);
    drop(device);
    for (hwnd, _, _) in windows {
        // SAFETY: hwnd 由本线程创建且尚未销毁。
        let _ = unsafe { DestroyWindow(hwnd) };
    }
    Ok(())
}

/// 单窗口或单线程多窗口的测量结果。
struct Measurement {
    presents: Vec<u64>,
    /// 相邻 `Present` 返回时刻的间隔（含 vsync 等待）。
    samples: Vec<Vec<f64>>,
    /// `draw_test_frame` 的耗时：CPU 侧绘制提交成本（不含 vsync 等待）。
    work_ms: Vec<Vec<f64>>,
    /// `Present(1, 0)` 的阻塞耗时。
    present_ms: Vec<Vec<f64>>,
    first_frame_ms: Option<f64>,
}

/// 注册窗口类；重复注册返回 0，视为失败。
fn register_window_class(hinstance: HINSTANCE) -> Result<(), AppError> {
    let class_name = wide(CLASS_NAME);
    // SAFETY: 系统光标句柄；`IDC_ARROW` 是资源 id，无需释放。
    let cursor = unsafe { LoadCursorW(None, IDC_ARROW) }.unwrap_or_default();
    let wc = WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(overlay_wnd_proc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: hinstance,
        hIcon: HICON::default(),
        hCursor: cursor,
        hbrBackground: Default::default(),
        lpszMenuName: PCWSTR::null(),
        lpszClassName: PCWSTR(class_name.as_ptr()),
    };
    // SAFETY: `wc` 与类名字符串在调用期间存活。
    let atom = unsafe { RegisterClassW(&wc) };
    if atom == 0 {
        return Err(AppError::Internal(format!(
            "RegisterClassW failed for {CLASS_NAME}: {}",
            windows::core::Error::from_thread()
        )));
    }
    Ok(())
}

/// 创建一个置顶弹出窗口；位置/尺寸都是物理像素。
fn create_overlay_window(
    hinstance: HINSTANCE,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
) -> Result<HWND, AppError> {
    let class_name = wide(CLASS_NAME);
    let title = wide(WINDOW_TITLE);
    let style = WS_POPUP | WS_VISIBLE;
    // 不用 WS_EX_NOACTIVATE：覆盖层需要键盘焦点（design §7.1）。
    let ex_style = WS_EX_TOPMOST | WS_EX_TOOLWINDOW;
    // SAFETY: 类名/标题字符串在调用期间存活；父窗口与菜单为空。
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(ex_style.0),
            PCWSTR(class_name.as_ptr()),
            PCWSTR(title.as_ptr()),
            style,
            x,
            y,
            width as i32,
            height as i32,
            None,
            None,
            Some(hinstance),
            None,
        )
    }
    .map_err(|err| AppError::Internal(format!("CreateWindowExW failed: {err}")))?;

    // SAFETY: hwnd 刚创建。
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
    }
    Ok(hwnd)
}

/// 主渲染循环：泵消息 → 每个窗口画一帧并 `Present(1, 0)` → 记录帧间隔。
fn measure(
    renderers: &mut [Renderer],
    duration_secs: u64,
    start: Instant,
) -> Result<Measurement, AppError> {
    let window_count = renderers.len();
    let mut last_present: Vec<Option<Instant>> = vec![None; window_count];
    let mut presents: Vec<u64> = vec![0; window_count];
    let mut samples: Vec<Vec<f64>> = vec![Vec::new(); window_count];
    let mut work_ms: Vec<Vec<f64>> = vec![Vec::new(); window_count];
    let mut present_ms: Vec<Vec<f64>> = vec![Vec::new(); window_count];
    let mut first_frame_ms: Option<f64> = None;

    let deadline = start + Duration::from_secs(duration_secs);
    while Instant::now() < deadline {
        if !pump_messages() {
            break;
        }

        for (index, renderer) in renderers.iter_mut().enumerate() {
            let draw_start = Instant::now();
            renderer.draw_test_frame(LABEL)?;
            let draw_done = Instant::now();
            renderer.present()?;
            let now = Instant::now();

            if index == 0 && first_frame_ms.is_none() {
                first_frame_ms = Some(now.duration_since(start).as_secs_f64() * 1000.0);
            }

            presents[index] += 1;
            if presents[index] > WARMUP_FRAMES {
                if let Some(previous) = last_present[index] {
                    samples[index].push(now.duration_since(previous).as_secs_f64() * 1000.0);
                }
                work_ms[index].push(draw_done.duration_since(draw_start).as_secs_f64() * 1000.0);
                present_ms[index].push(now.duration_since(draw_done).as_secs_f64() * 1000.0);
            }
            last_present[index] = Some(now);
        }
    }

    Ok(Measurement {
        presents,
        samples,
        work_ms,
        present_ms,
        first_frame_ms,
    })
}

/// 处理当前线程队列里的全部消息；收到 `WM_QUIT`（Esc/关窗）返回 `false`。
fn pump_messages() -> bool {
    let mut msg = MSG::default();
    loop {
        // SAFETY: `msg` 指向栈上有效变量；`PM_REMOVE` 取走消息。
        let has_message = unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) };
        if !has_message.as_bool() {
            break;
        }
        if msg.message == WM_QUIT {
            QUIT.store(true, Ordering::SeqCst);
            return false;
        }
        // SAFETY: 消息来自本线程队列，转发给窗口过程。
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    !QUIT.load(Ordering::SeqCst)
}

/// 打印每窗口明细与一行机器可读 summary。
fn report(measurement: &Measurement, width: u32, height: u32, args: &Args, driver: &str) {
    let mut window_stats = Vec::with_capacity(measurement.samples.len());
    for (index, samples) in measurement.samples.iter().enumerate() {
        let stats = Stats::compute(samples);
        let work_avg = mean(&measurement.work_ms[index]);
        let present_avg = mean(&measurement.present_ms[index]);
        println!(
            "SPIKE_WINDOW index={index} width={width} height={height} presents={} samples={} avg_ms={:.3} p50_ms={:.3} p95_ms={:.3} p99_ms={:.3} max_ms={:.3} drops={} fps={:.2} work_avg_ms={work_avg:.3} present_avg_ms={present_avg:.3}",
            measurement.presents[index],
            stats.samples,
            stats.avg_ms,
            stats.p50_ms,
            stats.p95_ms,
            stats.p99_ms,
            stats.max_ms,
            stats.drops,
            stats.fps,
        );
        window_stats.push(stats);
    }

    let primary = window_stats.first().copied().unwrap_or_default();
    let work_avg = mean(measurement.work_ms.first().map_or(&[], Vec::as_slice));
    println!(
        "SPIKE_RESULT width={width} height={height} windows={} avg_ms={:.3} p50_ms={:.3} p95_ms={:.3} p99_ms={:.3} max_ms={:.3} drops={} fps={:.2} first_frame_ms={:.2} work_avg_ms={work_avg:.3} presents={} samples={} duration_s={} driver={driver}",
        args.windows,
        primary.avg_ms,
        primary.p50_ms,
        primary.p95_ms,
        primary.p99_ms,
        primary.max_ms,
        primary.drops,
        primary.fps,
        measurement.first_frame_ms.unwrap_or(-1.0),
        measurement.presents.first().copied().unwrap_or(0),
        primary.samples,
        args.duration,
    );
}

/// 样本算术平均；空集合返回 0。
fn mean(samples: &[f64]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    samples.iter().sum::<f64>() / samples.len() as f64
}

/// 帧间隔统计（毫秒）。
#[derive(Clone, Copy, Debug, Default)]
struct Stats {
    samples: usize,
    avg_ms: f64,
    p50_ms: f64,
    p95_ms: f64,
    p99_ms: f64,
    max_ms: f64,
    drops: usize,
    fps: f64,
}

impl Stats {
    fn compute(samples: &[f64]) -> Stats {
        if samples.is_empty() {
            return Stats::default();
        }
        let mut sorted = samples.to_vec();
        sorted.sort_by(|a, b| a.total_cmp(b));
        let sum: f64 = sorted.iter().sum();
        let avg = sum / sorted.len() as f64;
        Stats {
            samples: sorted.len(),
            avg_ms: avg,
            p50_ms: percentile(&sorted, 0.50),
            p95_ms: percentile(&sorted, 0.95),
            p99_ms: percentile(&sorted, 0.99),
            max_ms: sorted[sorted.len() - 1],
            drops: sorted.iter().filter(|&&ms| ms > DROP_THRESHOLD_MS).count(),
            fps: if avg > 0.0 { 1000.0 / avg } else { 0.0 },
        }
    }
}

/// 最近秩法分位数；`sorted` 必须升序且非空。
fn percentile(sorted: &[f64], fraction: f64) -> f64 {
    let index = ((sorted.len() - 1) as f64 * fraction).round() as usize;
    sorted[index.min(sorted.len() - 1)]
}

/// 覆盖层窗口过程：Esc/关窗退出，`WM_ERASEBKGND` 交给 D3D 而不是 GDI。
unsafe extern "system" fn overlay_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_KEYDOWN if wparam.0 as u16 == VK_ESCAPE.0 => {
            QUIT.store(true, Ordering::SeqCst);
            // SAFETY: 向本线程消息队列投递 WM_QUIT。
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        WM_CLOSE => {
            // SAFETY: hwnd 有效；Destroy 后收到 WM_DESTROY。
            let _ = unsafe { DestroyWindow(hwnd) };
            LRESULT(0)
        }
        WM_DESTROY => {
            QUIT.store(true, Ordering::SeqCst);
            // SAFETY: 向本线程消息队列投递 WM_QUIT。
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        _ => {
            // SAFETY: 默认处理；参数原样转发。
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
    }
}

/// UTF-16（NUL 结尾）缓冲区。
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}
