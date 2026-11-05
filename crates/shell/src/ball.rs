//! 阶段 01 最小悬浮球：分层弹出窗口 + GDI+ 自绘 + 单击捕获 / 拖动记忆位置。
//!
//! 线程模型见 design §3、§9.1：悬浮球独占一个 STA 线程（Task 16 的 OLE 拖放复用该
//! 线程），窗口、DIB 与绘制资源都只在该线程创建和释放；交互结果通过 [`ShellEvent`]
//! 投递给 app-core，悬浮球线程本身**不写配置**（位置落盘由 app-core 完成）。
//!
//! 消息循环空闲阻塞在 `GetMessageW`，不做轮询；[`BallHandle::set_visible`] 用
//! `SendMessageW` 同步切换可见性，保证抓帧开始前球已经消失（抓帧不含悬浮球）。
//!
//! 窗口是 `WS_POPUP` + `WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW |
//! WS_EX_NOACTIVATE`：不进任务栏/Alt-Tab、不抢焦点；内容通过 32 位 ARGB DIB 与
//! `UpdateLayeredWindow(ULW_ALPHA)` 呈现。
//!
//! 拖动不依赖 `WM_LBUTTONUP` 收尾：鼠标捕获被其他窗口抢走时系统只发
//! `WM_CAPTURECHANGED`（`WM_LBUTTONUP` 可能送到抢占方），所以捕获一丢失就结束拖动，
//! 之后不按键移动鼠标不会再移动球、也不会写位置。

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::marker::PhantomData;
use std::mem::size_of;
use std::ptr::{addr_of_mut, null_mut};
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crossbeam_channel::{bounded, Sender};
use tracing::{debug, error, info, trace, warn};
use visrecall_capture::display::{enumerate_displays, normalize_position};
use visrecall_domain::capture::DisplayInfo;
use visrecall_domain::geometry::{PhysicalPoint, PhysicalRect};
use visrecall_domain::AppError;
use windows::core::{Error as WinError, PCWSTR};
use windows::Win32::Foundation::{
    COLORREF, ERROR_CLASS_ALREADY_EXISTS, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, SIZE,
    WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC, SelectObject,
    AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, DIB_RGB_COLORS,
    HDC, HGDIOBJ,
};
use windows::Win32::Graphics::GdiPlus::{
    GdipCreateBitmapFromScan0, GdipCreateFont, GdipCreateFontFamilyFromName, GdipCreatePen1,
    GdipCreateSolidFill, GdipCreateStringFormat, GdipDeleteBrush, GdipDeleteFont,
    GdipDeleteFontFamily, GdipDeleteGraphics, GdipDeletePen, GdipDeleteStringFormat,
    GdipDisposeImage, GdipDrawEllipse, GdipDrawString, GdipFillEllipse,
    GdipGetImageGraphicsContext, GdipGraphicsClear, GdipSetSmoothingMode, GdipSetStringFormatAlign,
    GdipSetStringFormatLineAlign, GdiplusShutdown, GdiplusStartup, GdiplusStartupInput, GpBitmap,
    GpBrush, GpFont, GpFontFamily, GpGraphics, GpImage, GpPen, GpSolidFill, GpStringFormat, RectF,
    SmoothingModeAntiAlias, StringAlignmentCenter, UnitPixel,
};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Ole::{OleInitialize, OleUninitialize};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, VK_LBUTTON,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW,
    GetWindowLongPtrW, LoadCursorW, PostMessageW, PostQuitMessage, RegisterClassW,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, SystemParametersInfoW, TranslateMessage,
    UnregisterClassW, UpdateLayeredWindow, CS_HREDRAW, CS_VREDRAW, GWLP_USERDATA, IDC_ARROW, MSG,
    SPI_GETWORKAREA, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SW_HIDE, SW_SHOWNOACTIVATE,
    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, ULW_ALPHA, WM_APP, WM_CAPTURECHANGED, WM_CLOSE,
    WM_DESTROY, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WNDCLASSW, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use crate::drop_target::{DropRegistration, DropVisual, WM_BALL_DROP_VISUAL};
use crate::{CaptureTrigger, ShellEvent};

/// 悬浮球窗口类名（`FindWindowW("VisRecallBall")` 可用于排查）。
pub const BALL_CLASS_NAME: &str = "VisRecallBall";
/// 窗口标题；`WS_EX_TOOLWINDOW` 下只在调试工具里可见。
const BALL_WINDOW_TITLE: &str = "VisRecall Ball";

/// 默认尺寸（与 `BallConfig::default().size` 一致）。
pub const DEFAULT_BALL_SIZE: u32 = 48;
/// 尺寸下限：配置被手工改坏时不至于画出不可见的球。
pub const MIN_BALL_SIZE: u32 = 16;
/// 尺寸上限：配置被手工改坏时不至于画出占满屏幕的球。
pub const MAX_BALL_SIZE: u32 = 256;
/// 悬浮球线程回报「窗口已就绪」的最长等待时间。
///
/// 超时只影响首帧：`spawn_ball_thread` 不阻塞启动（M-8），线程仍会继续创建窗口，
/// 之后 `BallHandle::set_visible` 照常生效。
const BALL_READY_TIMEOUT: Duration = Duration::from_secs(2);
/// 首次出现时距工作区右下角的边距（物理像素）。
const DEFAULT_MARGIN: i32 = 24;
/// 按下后移动超过该距离（物理像素）视为拖动，否则算单击。
const DRAG_THRESHOLD: i32 = 4;
/// 切换可见性的窗口消息（`SendMessageW` 同步处理，见模块文档）。
const WM_BALL_SET_VISIBLE: u32 = WM_APP + 1;
/// GDI+ `PixelFormat32bppPARGB`：预乘 alpha 与 `UpdateLayeredWindow` 的输入一致。
///
/// 值来自 `gdipluspixelformats.h`：
/// `11 | (32 << 8) | PixelFormatAlpha | PixelFormatPAlpha | PixelFormatGDI`。
const PIXEL_FORMAT_32BPP_PARGB: i32 = 0x000E_200B;

/// 壳层使用的悬浮球配置子集（尺寸 + 已记忆位置）。
///
/// 由 app-core 从 `visrecall_library::BallConfig` 映射而来：壳层不依赖配置/存储层，
/// 只接收本次启动需要的数据。
#[derive(Clone, Debug, PartialEq)]
pub struct BallConfig {
    /// 边长（物理像素）；线程内夹到 `[MIN_BALL_SIZE, MAX_BALL_SIZE]`。
    pub size: u32,
    /// 显示器 id → 归一化中心坐标，用于恢复上次位置。
    pub positions: HashMap<String, (f32, f32)>,
}

impl Default for BallConfig {
    fn default() -> Self {
        Self {
            size: DEFAULT_BALL_SIZE,
            positions: HashMap::new(),
        }
    }
}

/// 悬浮球线程句柄；drop 时等价于 [`BallHandle::stop`]。
#[derive(Debug)]
pub struct BallHandle {
    /// 窗口句柄（`isize`）；0 表示窗口尚未创建或线程已退出。
    hwnd: Arc<AtomicIsize>,
    /// 线程维护的可见性，`is_visible` 无锁读取。
    visible: Arc<AtomicBool>,
    stopped: AtomicBool,
}

impl BallHandle {
    /// 显示/隐藏悬浮球；同步等待悬浮球线程处理完（抓帧前隐藏必须是确定的）。
    ///
    /// 窗口不可用（线程创建失败/已退出）时只记日志，不 panic。
    pub fn set_visible(&self, visible: bool) {
        let raw = self.hwnd.load(Ordering::SeqCst);
        if raw == 0 {
            debug!(
                visible,
                "floating ball window unavailable; visibility change ignored"
            );
            return;
        }
        // SAFETY: `hwnd` 来自本进程的悬浮球线程；跨线程 `SendMessageW` 由系统串行投递。
        // 窗口若已被销毁，消息返回 0，不影响调用方。
        unsafe {
            windows::Win32::UI::WindowsAndMessaging::SendMessageW(
                HWND(raw as *mut c_void),
                WM_BALL_SET_VISIBLE,
                Some(WPARAM(usize::from(visible))),
                Some(LPARAM(0)),
            );
        }
    }

    /// 最近一次生效的可见性（由悬浮球线程在切换后写入）。
    pub fn is_visible(&self) -> bool {
        self.visible.load(Ordering::SeqCst)
    }

    /// 请求悬浮球线程退出（`WM_CLOSE` → 销毁窗口 → `WM_QUIT`）；幂等且不阻塞。
    pub fn stop(&self) {
        if self.stopped.swap(true, Ordering::SeqCst) {
            return;
        }
        let raw = self.hwnd.load(Ordering::SeqCst);
        if raw == 0 {
            return;
        }
        // SAFETY: 同上；窗口已销毁时投递失败只记日志。
        if let Err(err) = unsafe {
            PostMessageW(
                Some(HWND(raw as *mut c_void)),
                WM_CLOSE,
                WPARAM(0),
                LPARAM(0),
            )
        } {
            warn!(error = %err, "failed to post WM_CLOSE to the floating ball window");
        }
    }
}

impl Drop for BallHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// 在专用 STA 线程创建悬浮球窗口并返回句柄。
///
/// 即使 `config` 表示「球被禁用」也会创建线程（窗口初始隐藏）：托盘随时可以开启球，
/// 调用方随后用 [`BallHandle::set_visible`] 应用期望的可见性。线程创建失败只记日志，
/// 返回的句柄所有操作都是空操作，不影响热键捕获取代路径。
pub fn spawn_ball_thread(config: BallConfig, events: Sender<ShellEvent>) -> BallHandle {
    let hwnd = Arc::new(AtomicIsize::new(0));
    let visible = Arc::new(AtomicBool::new(false));
    let requested_size = config.size;
    let (ready_tx, ready_rx) = bounded::<Result<(), AppError>>(1);

    let thread_hwnd = Arc::clone(&hwnd);
    let thread_visible = Arc::clone(&visible);

    let spawned = thread::Builder::new()
        .name("visrecall-ball".into())
        .spawn(move || ball_thread_main(config, events, ready_tx, thread_hwnd, thread_visible));

    match spawned {
        Ok(_) => match ready_rx.recv_timeout(BALL_READY_TIMEOUT) {
            Ok(Ok(())) => info!(size = requested_size, "floating ball thread started"),
            Ok(Err(err)) => error!(error = %err, "floating ball unavailable"),
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => warn!(
                timeout_ms = BALL_READY_TIMEOUT.as_millis() as u64,
                "floating ball window not ready in time; continuing without waiting \
                 (visibility will be applied by the next set_visible call)"
            ),
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                error!("floating ball thread exited before reporting its window")
            }
        },
        Err(err) => error!(error = %err, "failed to spawn floating ball thread"),
    }

    BallHandle {
        hwnd,
        visible,
        stopped: AtomicBool::new(false),
    }
}

/// 把期望的左上角位置夹到（包含或最近）显示器的工作区内。
///
/// `desired` 是窗口**左上角**；结果满足
/// `x ∈ [work.x, work.right - size]`、`y ∈ [work.y, work.bottom - size]`
/// （工作区比球还小时固定到左上角）。没有任何显示器时原样返回。
pub fn clamp_ball_position(
    displays: &[DisplayInfo],
    desired: PhysicalPoint,
    size: u32,
) -> PhysicalPoint {
    let Some(display) = display_at_or_nearest(displays, desired) else {
        return desired;
    };
    let work = display.work_area;
    if work.is_empty() {
        return desired;
    }

    let size = i64::from(size);
    let (min_x, min_y) = (i64::from(work.x), i64::from(work.y));
    let (max_x, max_y) = (
        i64::from(work.right()) - size,
        i64::from(work.bottom()) - size,
    );
    PhysicalPoint {
        x: clamp_axis(i64::from(desired.x), min_x, max_x) as i32,
        y: clamp_axis(i64::from(desired.y), min_y, max_y) as i32,
    }
}

/// 夹取单轴；工作区小于球时固定到 `min`（左上角优先可见）。
fn clamp_axis(value: i64, min: i64, max: i64) -> i64 {
    value.max(min).min(max.max(min))
}

/// 包含 `point` 的显示器优先，否则取矩形距离最近的；距离相同时主显示器优先。
fn display_at_or_nearest(displays: &[DisplayInfo], point: PhysicalPoint) -> Option<&DisplayInfo> {
    displays
        .iter()
        .min_by_key(|display| (rect_distance(&display.bounds, point), !display.primary))
}

fn rect_distance(rect: &PhysicalRect, point: PhysicalPoint) -> i64 {
    let dx = axis_distance(point.x, rect.x, rect.right());
    let dy = axis_distance(point.y, rect.y, rect.bottom());
    dx * dx + dy * dy
}

/// 点到半开区间 `[start, end)` 的距离（区间内为 0）。
fn axis_distance(value: i32, start: i32, end: i32) -> i64 {
    let (value, start, end) = (i64::from(value), i64::from(start), i64::from(end));
    if value < start {
        start - value
    } else if value >= end {
        value - end + 1
    } else {
        0
    }
}

/// 恢复上次位置：优先主显示器记录，其次任意一台有记录的显示器，最后回落到默认位置。
fn restore_ball_position(
    positions: &HashMap<String, (f32, f32)>,
    displays: &[DisplayInfo],
    size: u32,
) -> PhysicalPoint {
    if displays.is_empty() {
        return fallback_default_position(size);
    }

    let primary = displays
        .iter()
        .find(|display| display.primary)
        .unwrap_or(&displays[0]);
    let restored = positions
        .get(&primary.id)
        .map(|norm| (primary, *norm))
        .or_else(|| {
            displays
                .iter()
                .find_map(|display| positions.get(&display.id).map(|norm| (display, *norm)))
        });

    match restored {
        Some((display, (nx, ny))) => {
            let center = PhysicalPoint {
                x: display.bounds.x + (nx * display.bounds.width as f32).round() as i32,
                y: display.bounds.y + (ny * display.bounds.height as f32).round() as i32,
            };
            let top_left = PhysicalPoint {
                x: center.x - size as i32 / 2,
                y: center.y - size as i32 / 2,
            };
            clamp_ball_position(displays, top_left, size)
        }
        None => default_position(displays, size),
    }
}

/// 首次出现：主显示器工作区右下角留 [`DEFAULT_MARGIN`] 边距。
fn default_position(displays: &[DisplayInfo], size: u32) -> PhysicalPoint {
    let display = displays
        .iter()
        .find(|display| display.primary)
        .unwrap_or(&displays[0]);
    let work = display.work_area;
    let desired = PhysicalPoint {
        x: work.right() - size as i32 - DEFAULT_MARGIN,
        y: work.bottom() - size as i32 - DEFAULT_MARGIN,
    };
    clamp_ball_position(displays, desired, size)
}

/// 枚举失败时的兜底：主显示器工作区右下角。
fn fallback_default_position(size: u32) -> PhysicalPoint {
    let mut rect = RECT::default();
    // SAFETY: 输出指针指向栈上 `RECT`；失败时回落到 (0, 0)。
    let query = unsafe {
        SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some(addr_of_mut!(rect).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    };
    match query {
        Ok(()) => PhysicalPoint {
            x: rect.right - size as i32 - DEFAULT_MARGIN,
            y: rect.bottom - size as i32 - DEFAULT_MARGIN,
        },
        Err(err) => {
            warn!(error = %err, "SPI_GETWORKAREA failed; floating ball placed at the origin");
            PhysicalPoint::default()
        }
    }
}

/// 悬浮球线程主体：COM/GDI+ → 注册类 → 建窗口 → 绘制 → 消息循环 → 释放资源。
fn ball_thread_main(
    config: BallConfig,
    events: Sender<ShellEvent>,
    ready: Sender<Result<(), AppError>>,
    hwnd_slot: Arc<AtomicIsize>,
    visible: Arc<AtomicBool>,
) {
    // Task 16 的 OLE 拖放复用该线程的 STA apartment。
    let _com = ComGuard::init();
    let _gdiplus = match GdiplusGuard::start() {
        Some(guard) => guard,
        None => {
            let _ = ready.send(Err(AppError::Internal(
                "GdiplusStartup failed for the floating ball".into(),
            )));
            return;
        }
    };

    let instance = match unsafe { GetModuleHandleW(None) } {
        Ok(module) => HINSTANCE(module.0),
        Err(err) => {
            let _ = ready.send(Err(AppError::Internal(format!(
                "GetModuleHandleW failed for the floating ball: {err}"
            ))));
            return;
        }
    };

    let class_registered = match register_ball_class(instance) {
        Ok(registered) => registered,
        Err(err) => {
            let _ = ready.send(Err(err));
            return;
        }
    };

    if let Err(err) = run_ball_window(
        config,
        instance,
        events,
        &ready,
        Arc::clone(&hwnd_slot),
        &visible,
    ) {
        // `run_ball_window` 在回报成功之前失败时不会发送 ready；这里补上错误。
        let _ = ready.send(Err(err));
    }

    if class_registered {
        unregister_ball_class(instance);
    }
    debug!("floating ball thread exiting");
}

/// 创建窗口、绘制并运行消息循环；`ready` 只在窗口可用后回报成功。
fn run_ball_window(
    config: BallConfig,
    instance: HINSTANCE,
    events: Sender<ShellEvent>,
    ready: &Sender<Result<(), AppError>>,
    hwnd_slot: Arc<AtomicIsize>,
    visible: &Arc<AtomicBool>,
) -> Result<(), AppError> {
    let displays = match enumerate_displays() {
        Ok(displays) => displays,
        Err(err) => {
            warn!(error = %err, "display enumeration failed; using the primary work area");
            Vec::new()
        }
    };
    let size = config.size.clamp(MIN_BALL_SIZE, MAX_BALL_SIZE);
    if size != config.size {
        warn!(
            configured = config.size,
            clamped = size,
            "floating ball size outside the supported range; using the clamped value"
        );
    }
    let position = restore_ball_position(&config.positions, &displays, size);

    // 窗口状态放在 `RefCell` 后：`SetWindowPos`/`ShowWindow` 可能同步重入 wndproc，
    // 每次一律短借用，绝不把可变引用跨越窗口 API 调用（M-7）。
    let drop_visual = Arc::new(Mutex::new(DropVisual::default()));
    let drop_events = events.clone();
    let state = Box::new(RefCell::new(BallWindowState {
        base_size: size,
        size,
        position,
        base_position: position,
        events,
        visible: Arc::clone(visible),
        drag: DragState::inactive(),
        hwnd_slot: Arc::clone(&hwnd_slot),
        drop_visual: Arc::clone(&drop_visual),
        drop_target: None,
    }));
    let state_ptr = Box::into_raw(state);

    let hwnd = match create_ball_window(instance, position, size) {
        Ok(hwnd) => hwnd,
        Err(err) => {
            // SAFETY: 状态刚创建，尚未被任何窗口引用。
            unsafe { drop(Box::from_raw(state_ptr)) };
            return Err(err);
        }
    };

    // SAFETY: 状态由本线程独占，窗口销毁（消息循环退出）之前一直有效。
    unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, state_ptr as isize) };

    if let Err(err) = render_ball(hwnd, size, position, None, true) {
        // SAFETY: 窗口与状态都由本线程管理；销毁窗口后取回状态。
        unsafe {
            let _ = DestroyWindow(hwnd);
            drop(Box::from_raw(state_ptr));
        }
        return Err(err);
    }

    // OLE 拖放目标：注册失败只降级（单击捕获仍可用），不阻断悬浮球。
    match DropRegistration::register(hwnd, drop_events, Arc::clone(&drop_visual)) {
        Ok(registration) => {
            let _ = with_state_mut(hwnd, |state| state.drop_target = Some(registration));
            info!("floating ball registered as an OLE drop target");
        }
        Err(err) => warn!(error = %err, "OLE drop target unavailable; drag and drop disabled"),
    }

    hwnd_slot.store(hwnd.0 as isize, Ordering::SeqCst);
    let _ = ready.send(Ok(()));
    info!(
        size,
        x = position.x,
        y = position.y,
        "floating ball window created"
    );

    let mut message = MSG::default();
    loop {
        // SAFETY: 消息循环只读取本线程队列，`message` 是栈上有效缓冲。
        let result = unsafe { GetMessageW(&mut message, None, 0, 0) };
        match result.0 {
            // WM_QUIT：窗口已销毁（`stop` 或用户关闭）。
            0 => break,
            // -1：GetMessageW 出错。
            -1 => {
                error!("floating ball message loop failed");
                break;
            }
            _ => {}
        }
        // SAFETY: `message` 来自上面的 GetMessageW。
        unsafe {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }

    // WM_DESTROY 已把共享句柄置 0；这里再写一次只是兜底（幂等）。
    hwnd_slot.store(0, Ordering::SeqCst);
    // SAFETY: 消息循环已退出且窗口已销毁；状态不再被任何消息处理引用。
    unsafe { drop(Box::from_raw(state_ptr)) };
    Ok(())
}

/// 窗口消息处理所需的状态；只在悬浮球线程访问，每次消息短借用（见 [`with_state`]）。
struct BallWindowState {
    /// 配置的球体边长（拖入时按 1.15× 放大，离开后恢复）。
    base_size: u32,
    /// 当前绘制边长（拖动视觉可临时放大）。
    size: u32,
    /// 当前左上角（全局物理像素）。
    position: PhysicalPoint,
    /// 未放大时的左上角；拖入视觉以它为中心放大/还原。
    base_position: PhysicalPoint,
    events: Sender<ShellEvent>,
    visible: Arc<AtomicBool>,
    /// 拖动状态机（纯逻辑，见 [`DragState`]）。
    drag: DragState,
    /// 共享窗口句柄；窗口销毁时置 0，让跨线程投递不再打到失效句柄（M-4）。
    hwnd_slot: Arc<AtomicIsize>,
    /// OLE 回调写入的拖入视觉状态（本线程的 wndproc 读取）。
    drop_visual: Arc<Mutex<DropVisual>>,
    /// OLE 拖放注册；窗口销毁前释放（`RevokeDragDrop`）。
    drop_target: Option<DropRegistration>,
}

/// 拖动进度。
#[derive(Clone, Copy, Debug, PartialEq)]
enum DragUpdate {
    /// 不在拖动中（未按下，或已结束/取消）：忽略本次移动。
    Ignore,
    /// 拖动中但尚未越过 [`DRAG_THRESHOLD`]：不移动窗口。
    Pending,
    /// 越过阈值：把窗口移动到该左上角。
    Move(PhysicalPoint),
    /// 拖动中左键已不在按下状态（捕获丢失的典型信号）：已结束拖动，不产生事件。
    Cancelled,
}

/// 拖动状态机：与 Win32 解耦，便于单测「捕获丢失后移动不再生效」（I-1）。
///
/// 生命周期：`begin`（`WM_LBUTTONDOWN`）→ `on_move`（`WM_MOUSEMOVE`）→
/// `finish`（`WM_LBUTTONUP`）或 `cancel`（`WM_CAPTURECHANGED` / 左键已松开）。
/// `cancel` 之后必须重新 `begin` 才能再次拖动，避免捕获丢失后「移一下鼠标球就跳走」。
#[derive(Clone, Debug, PartialEq)]
struct DragState {
    active: bool,
    /// 本次按下后是否已越过拖动阈值。
    moved: bool,
    /// 按下时的光标位置与窗口左上角，用于计算拖动位移。
    press_cursor: (i32, i32),
    press_origin: PhysicalPoint,
}

impl DragState {
    const fn inactive() -> Self {
        Self {
            active: false,
            moved: false,
            press_cursor: (0, 0),
            press_origin: PhysicalPoint { x: 0, y: 0 },
        }
    }

    /// 开始拖动：记录光标起点与窗口原点。
    fn begin(&mut self, cursor: (i32, i32), origin: PhysicalPoint) {
        *self = Self {
            active: true,
            moved: false,
            press_cursor: cursor,
            press_origin: origin,
        };
    }

    fn is_active(&self) -> bool {
        self.active
    }

    /// 结束拖动（捕获丢失/取消）；返回之前是否处于拖动状态。
    fn cancel(&mut self) -> bool {
        let was_active = self.active;
        *self = Self::inactive();
        was_active
    }

    /// 拖动中的一次移动；`pressed == false` 表示左键已不在按下状态 → 视为捕获丢失。
    fn on_move(&mut self, cursor: (i32, i32), pressed: bool) -> DragUpdate {
        if !self.active {
            return DragUpdate::Ignore;
        }
        if !pressed {
            self.cancel();
            return DragUpdate::Cancelled;
        }

        let dx = cursor.0 - self.press_cursor.0;
        let dy = cursor.1 - self.press_cursor.1;
        if !self.moved {
            if dx.abs() <= DRAG_THRESHOLD && dy.abs() <= DRAG_THRESHOLD {
                return DragUpdate::Pending;
            }
            self.moved = true;
        }
        DragUpdate::Move(PhysicalPoint {
            x: self.press_origin.x + dx,
            y: self.press_origin.y + dy,
        })
    }

    /// 松开：返回 `None` 表示不在拖动；`Some(false)` 是单击，`Some(true)` 是拖动。
    fn finish(&mut self) -> Option<bool> {
        if !self.active {
            return None;
        }
        let moved = self.moved;
        *self = Self::inactive();
        Some(moved)
    }
}

/// 取窗口状态抽屉；窗口尚未绑定状态（`WM_CREATE` 之前）返回 `None`。
unsafe fn ball_state(hwnd: HWND) -> Option<&'static RefCell<BallWindowState>> {
    // SAFETY: 调用方保证 `hwnd` 是悬浮球窗口；指针在消息循环退出后才释放。
    let raw = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) };
    if raw == 0 {
        return None;
    }
    // SAFETY: `raw` 由 `Box<RefCell<BallWindowState>>` 泄漏而来，且状态只在悬浮球线程访问。
    Some(unsafe { &*(raw as *const RefCell<BallWindowState>) })
}

/// 短借用窗口状态做只读操作。
///
/// 借用失败说明正在处理外层消息（`SetWindowPos`/`ShowWindow` 之类的窗口 API 可以
/// 同步重入 wndproc）；此时直接忽略本次消息，绝不等同一份状态上的第二个可变引用（M-7）。
fn with_state<R>(hwnd: HWND, f: impl FnOnce(&BallWindowState) -> R) -> Option<R> {
    // SAFETY: 见 `ball_state`；借用只在本函数内有效。
    let cell = unsafe { ball_state(hwnd) }?;
    let state = cell.try_borrow().ok()?;
    Some(f(&state))
}

/// 短借用窗口状态做可变操作（同上，不跨窗口 API 调用）。
fn with_state_mut<R>(hwnd: HWND, f: impl FnOnce(&mut BallWindowState) -> R) -> Option<R> {
    // SAFETY: 见 `ball_state`；借用只在本函数内有效。
    let cell = unsafe { ball_state(hwnd) }?;
    let mut state = cell.try_borrow_mut().ok()?;
    Some(f(&mut state))
}

unsafe extern "system" fn ball_window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: 见 `ball_state`；`None` 时走默认处理。
    if unsafe { ball_state(hwnd) }.is_none() {
        return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    }

    match message {
        WM_LBUTTONDOWN => {
            press_ball(hwnd);
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            drag_ball(hwnd);
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            release_ball(hwnd);
            LRESULT(0)
        }
        // I-1：捕获被其他窗口抢走时不会再有 `WM_LBUTTONUP`，必须在这里收尾。
        WM_CAPTURECHANGED => {
            cancel_drag(hwnd);
            LRESULT(0)
        }
        WM_BALL_SET_VISIBLE => {
            set_ball_visible(hwnd, wparam.0 != 0);
            LRESULT(0)
        }
        // OLE 回调只写共享视觉状态；实际放大/绘制由本线程在这里完成。
        WM_BALL_DROP_VISUAL => {
            refresh_drop_visual(hwnd);
            LRESULT(0)
        }
        WM_CLOSE => {
            // OLE 注册必须在窗口销毁前撤销；`take` 后借用结束再 drop。
            if let Some(registration) = with_state_mut(hwnd, |state| state.drop_target.take()) {
                drop(registration);
            }
            // SAFETY: `hwnd` 有效；WM_DESTROY 里退出消息循环。
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            // 兜底：`WM_CLOSE` 之外的销毁路径也要撤销拖放注册。
            if let Some(registration) = with_state_mut(hwnd, |state| state.drop_target.take()) {
                drop(registration);
            }
            // M-4：窗口销毁之前先让共享句柄失效，跨线程投递从此拿到 0。
            let _ = with_state(hwnd, |state| state.hwnd_slot.store(0, Ordering::SeqCst));
            // SAFETY: 结束本线程消息循环（`GetMessageW` 返回 WM_QUIT）。
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

/// 按下：记录拖动起点并捕获鼠标；尚未移动，松开即单击。
fn press_ball(hwnd: HWND) {
    let mut cursor = POINT::default();
    // SAFETY: 输出指针指向栈上 `POINT`。
    if let Err(err) = unsafe { GetCursorPos(&mut cursor) } {
        warn!(error = %err, "GetCursorPos failed on ball press; drag disabled");
        return;
    }
    // 先 `SetCapture` 再进入拖动状态：`SetCapture` 可能同步派发 `WM_CAPTURECHANGED`，
    // 那时拖动尚未开始，`cancel_drag` 是空操作，不会误杀这次拖动。
    // SAFETY: `hwnd` 属于本线程；捕获鼠标让窗口外的移动/松开仍然送达。
    unsafe { SetCapture(hwnd) };
    let _ = with_state_mut(hwnd, |state| {
        let origin = state.position;
        state.drag.begin((cursor.x, cursor.y), origin);
    });
}

/// 移动：拖动中且左键仍按下时按位移移动窗口（不改尺寸、不抢焦点）。
fn drag_ball(hwnd: HWND) {
    // 未在拖动（鼠标只是掠过球）时先返回，省掉两次系统调用。
    if !with_state(hwnd, |state| state.drag.is_active()).unwrap_or(false) {
        return;
    }
    let mut cursor = POINT::default();
    // SAFETY: 输出指针指向栈上 `POINT`。
    if let Err(err) = unsafe { GetCursorPos(&mut cursor) } {
        debug!(error = %err, "GetCursorPos failed during ball drag");
        return;
    }
    // I-1 兜底：捕获丢失时 `WM_LBUTTONUP` 可能送到抢占方，这里以左键状态为准。
    // SAFETY: 无参数查询调用线程的输入状态；高字节置位表示按下。
    let pressed = unsafe { GetKeyState(VK_LBUTTON.0 as i32) } < 0;

    // 借用只覆盖纯逻辑推进；下面的窗口 API 调用不再持有引用（M-7）。
    let Some((update, size)) = with_state_mut(hwnd, |state| {
        let update = state.drag.on_move((cursor.x, cursor.y), pressed);
        (update, state.size)
    }) else {
        return;
    };

    match update {
        DragUpdate::Move(target) => {
            if move_window(hwnd, target, size) {
                let _ = with_state_mut(hwnd, |state| state.position = target);
            }
        }
        DragUpdate::Cancelled => {
            debug!("floating ball drag cancelled: left button released without WM_LBUTTONUP");
            release_capture();
        }
        DragUpdate::Ignore | DragUpdate::Pending => {}
    }
}

/// `WM_CAPTURECHANGED`：捕获被改动时立即结束拖动，不移动窗口、不发送 `BallMoved`。
///
/// 不在捕获丢失时调用 `ReleaseCapture`：捕获已经不在本窗口，调用只会白白派发消息。
fn cancel_drag(hwnd: HWND) {
    let (cancelled, position) = with_state_mut(hwnd, |state| (state.drag.cancel(), state.position))
        .unwrap_or((false, PhysicalPoint::default()));
    if cancelled {
        debug!(
            x = position.x,
            y = position.y,
            "floating ball lost the mouse capture mid-drag; ball stays where it is"
        );
    } else {
        // 正常松开（`release_ball` 里的 ReleaseCapture）也会走到这里，此时拖动已结束。
        trace!("floating ball lost the mouse capture; no drag to cancel");
    }
}

/// 松开：未移动 → `CaptureRequest(BallClick)`；移动 → 夹回工作区并上报归一化中心。
fn release_ball(hwnd: HWND) {
    // 先结束拖动并把后续需要的字段取出来，再调用窗口/显示器 API（可能同步重入 wndproc）。
    let Some((moved, mut position, size, events)) = with_state_mut(hwnd, |state| {
        state
            .drag
            .finish()
            .map(|moved| (moved, state.position, state.size, state.events.clone()))
    })
    .flatten() else {
        return;
    };
    // SAFETY: 与 `SetCapture` 成对；拖动已结束，失败无需处理。
    release_capture();

    if !moved {
        send_event(
            &events,
            ShellEvent::CaptureRequest(CaptureTrigger::BallClick),
        );
        return;
    }

    let displays = match enumerate_displays() {
        Ok(displays) => displays,
        Err(err) => {
            warn!(error = %err, "display enumeration failed; ball position not saved");
            return;
        }
    };

    let clamped = clamp_ball_position(&displays, position, size);
    if clamped != position && move_window(hwnd, clamped, size) {
        position = clamped;
        let _ = with_state_mut(hwnd, |state| state.position = clamped);
    }

    let center = PhysicalPoint {
        x: position.x + size as i32 / 2,
        y: position.y + size as i32 / 2,
    };
    let Some(target) = display_at_or_nearest(&displays, center) else {
        warn!("no display found for the ball position; not saving");
        return;
    };
    let (nx, ny) = normalize_position(target, center);
    send_event(
        &events,
        ShellEvent::BallMoved {
            display_id: target.id.clone(),
            nx,
            ny,
        },
    );
    info!(display_id = %target.id, nx, ny, "floating ball moved");
}

/// 释放鼠标捕获（只影响本线程）；拖动已结束时调用。
fn release_capture() {
    // SAFETY: `ReleaseCapture` 只作用于调用线程；本线程只有悬浮球窗口，
    // 没有捕获时调用也无副作用。
    let _ = unsafe { ReleaseCapture() };
}

fn send_event(events: &Sender<ShellEvent>, event: ShellEvent) {
    if events.send(event).is_err() {
        debug!("shell event channel closed; floating ball event dropped");
    }
}

/// 应用可见性：`SW_SHOWNOACTIVATE` 不抢焦点，`SW_HIDE` 后球不参与抓帧。
fn set_ball_visible(hwnd: HWND, visible: bool) {
    // 先取走可见性标志，再调用 `ShowWindow`（可能同步重入 wndproc，不能持有借用）。
    let Some(flag) = with_state(hwnd, |state| Arc::clone(&state.visible)) else {
        return;
    };
    // SAFETY: `hwnd` 属于本线程；显示时不激活窗口。
    unsafe {
        let _ = ShowWindow(hwnd, if visible { SW_SHOWNOACTIVATE } else { SW_HIDE });
    }
    flag.store(visible, Ordering::SeqCst);
    debug!(visible, "floating ball visibility changed");
}

/// 拖入时球体放大倍数（design §9.1）。
const DROP_SCALE: f32 = 1.15;

/// 应用 OLE 拖入视觉：可接受时放大到 1.15× 并绘制动作文案，拒绝时显示灰色提示。
///
/// 由 `WM_BALL_DROP_VISUAL` 触发；视觉状态来自共享的 [`DropVisual`]，本函数只读快照，
/// 不持有互斥锁调用窗口 API。
fn refresh_drop_visual(hwnd: HWND) {
    let Some((visual, base_size, base_position)) = with_state(hwnd, |state| {
        let visual = state
            .drop_visual
            .lock()
            .map(|slot| slot.clone())
            .unwrap_or_default();
        (visual, state.base_size, state.base_position)
    }) else {
        return;
    };

    let target_size = if visual.active {
        ((base_size as f32 * DROP_SCALE).round() as u32).max(1)
    } else {
        base_size
    };
    // 放大/还原都以配置尺寸的中心为锚点，球不会「跳」到一边。
    let center = PhysicalPoint {
        x: base_position.x + base_size as i32 / 2,
        y: base_position.y + base_size as i32 / 2,
    };
    let target_position = PhysicalPoint {
        x: center.x - target_size as i32 / 2,
        y: center.y - target_size as i32 / 2,
    };
    let label = if visual.active && !visual.label.is_empty() {
        Some(visual.label.as_str())
    } else {
        None
    };

    if let Err(err) = render_ball(hwnd, target_size, target_position, label, visual.accepted) {
        warn!(error = %err, "failed to render the drop visual");
        return;
    }
    let _ = with_state_mut(hwnd, |state| {
        state.size = target_size;
        state.position = target_position;
    });
}

/// 移动窗口（不改变尺寸/Z 序、不激活）；返回是否成功。
fn move_window(hwnd: HWND, position: PhysicalPoint, size: u32) -> bool {
    // SAFETY: `hwnd` 属于本线程；忽略 Z 序与尺寸。
    match unsafe {
        SetWindowPos(
            hwnd,
            None,
            position.x,
            position.y,
            size as i32,
            size as i32,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        )
    } {
        Ok(()) => true,
        Err(err) => {
            warn!(error = %err, "failed to move the floating ball window");
            false
        }
    }
}

fn register_ball_class(instance: HINSTANCE) -> Result<bool, AppError> {
    let class_name = wide(BALL_CLASS_NAME);
    // SAFETY: 系统光标资源；`IDC_ARROW` 是资源 id，无需释放。
    let cursor = unsafe { LoadCursorW(None, IDC_ARROW) }.unwrap_or_default();
    let window_class = WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(ball_window_proc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: instance,
        hIcon: Default::default(),
        hCursor: cursor,
        hbrBackground: Default::default(),
        lpszMenuName: PCWSTR::null(),
        lpszClassName: PCWSTR(class_name.as_ptr()),
    };
    // SAFETY: `window_class` 与类名字符串在调用期间存活。
    let atom = unsafe { RegisterClassW(&window_class) };
    if atom == 0 {
        let err = WinError::from_thread();
        // 同一进程重复注册（上一次线程残留）不视为失败，但不由本次注销。
        if err.code() == windows::core::HRESULT::from_win32(ERROR_CLASS_ALREADY_EXISTS.0) {
            debug!("floating ball window class already registered");
            return Ok(false);
        }
        return Err(AppError::Internal(format!(
            "RegisterClassW failed for {BALL_CLASS_NAME}: {err}"
        )));
    }
    Ok(true)
}

fn unregister_ball_class(instance: HINSTANCE) {
    let class_name = wide(BALL_CLASS_NAME);
    // SAFETY: 类名以 NUL 结尾且在调用期间存活；该类所有窗口都已销毁。
    unsafe {
        let _ = UnregisterClassW(PCWSTR(class_name.as_ptr()), Some(instance));
    }
}

/// 创建 `WS_POPUP` 分层窗口；初始隐藏，由 [`BallHandle::set_visible`] 显示。
fn create_ball_window(
    instance: HINSTANCE,
    position: PhysicalPoint,
    size: u32,
) -> Result<HWND, AppError> {
    let class_name = wide(BALL_CLASS_NAME);
    let title = wide(BALL_WINDOW_TITLE);
    let ex_style = WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
    // SAFETY: 类名/标题以 NUL 结尾且在调用期间存活；父窗口/菜单为空；hinstance 来自本进程。
    unsafe {
        CreateWindowExW(
            ex_style,
            PCWSTR(class_name.as_ptr()),
            PCWSTR(title.as_ptr()),
            WS_POPUP,
            position.x,
            position.y,
            size as i32,
            size as i32,
            None,
            None,
            Some(instance),
            None,
        )
    }
    .map_err(|err| {
        AppError::Internal(format!(
            "CreateWindowExW failed for {BALL_CLASS_NAME}: {err}"
        ))
    })
}

/// 画一次球并用 `UpdateLayeredWindow` 呈现；绘制资源在返回前全部释放。
///
/// `label` 非空时在球内绘制拖放动作文案（如「存为图片」）；`accepted == false`
/// 时球体转灰，提示这次拖放不会被接受。
fn render_ball(
    hwnd: HWND,
    size: u32,
    position: PhysicalPoint,
    label: Option<&str>,
    accepted: bool,
) -> Result<(), AppError> {
    // SAFETY: `GetDC(None)` 返回屏幕 DC；失败时为空句柄。
    let screen = unsafe { GetDC(None) };
    if screen.is_invalid() {
        return Err(AppError::Internal(format!(
            "GetDC failed for the floating ball: {}",
            WinError::from_thread()
        )));
    }
    let result = render_with_screen(hwnd, size, position, screen, label, accepted);
    // SAFETY: 与 `GetDC` 成对调用。
    unsafe { ReleaseDC(None, screen) };
    result
}

fn render_with_screen(
    hwnd: HWND,
    size: u32,
    position: PhysicalPoint,
    screen: HDC,
    label: Option<&str>,
    accepted: bool,
) -> Result<(), AppError> {
    // SAFETY: `screen` 有效；失败时为空句柄。
    let memory = unsafe { CreateCompatibleDC(Some(screen)) };
    if memory.is_invalid() {
        return Err(AppError::Internal(format!(
            "CreateCompatibleDC failed: {}",
            WinError::from_thread()
        )));
    }

    let width = size as i32;
    let bitmap_info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            // 负高度 = top-down DIB，与 GDI+ 的扫描行序一致。
            biHeight: -width,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            biSizeImage: (width * width * 4) as u32,
            biXPelsPerMeter: 0,
            biYPelsPerMeter: 0,
            biClrUsed: 0,
            biClrImportant: 0,
        },
        ..Default::default()
    };
    let mut bits: *mut c_void = null_mut();
    // SAFETY: `bitmap_info` 在调用期间存活；`bits` 指向栈上有效指针。
    let bitmap = match unsafe {
        CreateDIBSection(
            Some(screen),
            &bitmap_info,
            DIB_RGB_COLORS,
            &mut bits,
            None,
            0,
        )
    } {
        Ok(bitmap) if !bits.is_null() => bitmap,
        Ok(bitmap) => {
            // SAFETY: 位图已创建但像素指针为空：删除后返回错误。
            unsafe {
                let _ = DeleteObject(HGDIOBJ(bitmap.0));
                let _ = DeleteDC(memory);
            }
            return Err(AppError::Internal(
                "CreateDIBSection returned a null pixel pointer".into(),
            ));
        }
        Err(err) => {
            // SAFETY: 只释放已创建的 DC。
            unsafe {
                let _ = DeleteDC(memory);
            }
            return Err(AppError::Internal(format!(
                "CreateDIBSection failed for the floating ball: {err}"
            )));
        }
    };

    // SAFETY: 两个句柄都有效；返回原对象用于恢复。
    let previous = unsafe { SelectObject(memory, HGDIOBJ(bitmap.0)) };
    if previous.is_invalid() {
        // SAFETY: 位图尚未选入 DC，可直接删除。
        unsafe {
            let _ = DeleteObject(HGDIOBJ(bitmap.0));
            let _ = DeleteDC(memory);
        }
        return Err(AppError::Internal(format!(
            "SelectObject failed for the floating ball: {}",
            WinError::from_thread()
        )));
    }

    let rendered = draw_ball(bits, size, label, accepted)
        .and_then(|()| present_layered(hwnd, screen, memory, size, position));

    // SAFETY: 释放顺序为「恢复原对象 → 删除位图 → 删除内存 DC」。
    unsafe {
        SelectObject(memory, previous);
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
        let _ = DeleteDC(memory);
    }
    rendered
}

/// 用 GDI+ 在 DIB 上画圆底、阴影与简单镜头图标（预乘 alpha，透明背景）。
///
/// 有 `label` 时改为绘制动作文案（拖放反馈）；`accepted == false` 用灰色球体。
fn draw_ball(
    bits: *mut c_void,
    size: u32,
    label: Option<&str>,
    accepted: bool,
) -> Result<(), AppError> {
    let width = size as i32;
    let stride = width * 4;
    let mut bitmap: *mut GpBitmap = null_mut();
    // SAFETY: `bits` 指向 `size × size × 4` 的 DIB 像素，调用期间有效；
    // PARGB 与 `UpdateLayeredWindow` 期望的预乘 alpha 一致。
    let status = unsafe {
        GdipCreateBitmapFromScan0(
            width,
            width,
            stride,
            PIXEL_FORMAT_32BPP_PARGB,
            Some(bits as *const u8),
            &mut bitmap,
        )
    };
    if status.0 != 0 || bitmap.is_null() {
        return Err(AppError::Internal(format!(
            "GdipCreateBitmapFromScan0 failed: status {}",
            status.0
        )));
    }

    let painted = paint_ball(bitmap, size, label, accepted);
    // SAFETY: 位图由上面的调用创建，图形上下文已在 `paint_ball` 内释放。
    unsafe { GdipDisposeImage(bitmap.cast::<GpImage>()) };
    painted
}

/// 实际的绘制步骤；GDI+ 对象都在本函数内创建并释放。
fn paint_ball(
    bitmap: *mut GpBitmap,
    size: u32,
    label: Option<&str>,
    accepted: bool,
) -> Result<(), AppError> {
    let mut graphics: *mut GpGraphics = null_mut();
    // SAFETY: `bitmap` 有效且未被释放。
    let status = unsafe { GdipGetImageGraphicsContext(bitmap.cast::<GpImage>(), &mut graphics) };
    if status.0 != 0 || graphics.is_null() {
        return Err(AppError::Internal(format!(
            "GdipGetImageGraphicsContext failed: status {}",
            status.0
        )));
    }

    let edge = size as f32;
    // 拖入反馈：拒绝时球体转灰；接受时保持品牌蓝（放大由窗口尺寸负责）。
    let (body, border) = if accepted {
        (argb(255, 59, 130, 246), argb(255, 37, 99, 235))
    } else {
        (argb(255, 107, 114, 128), argb(255, 75, 85, 99))
    };

    // SAFETY: `graphics` 有效；每个 GDI+ 对象都在使用后立即释放。
    unsafe {
        // 清成全透明，否则 UpdateLayeredWindow 会把未初始化像素算进 alpha。
        GdipGraphicsClear(graphics, argb(0, 0, 0, 0));
        GdipSetSmoothingMode(graphics, SmoothingModeAntiAlias);

        // 阴影：略微下移的半透明黑圆。
        fill_ellipse(
            graphics,
            argb(70, 0, 0, 0),
            0.10 * edge,
            0.16 * edge,
            0.80 * edge,
            0.80 * edge,
        );
        // 球体：品牌蓝圆底（拖入反馈可换成灰色）。
        fill_ellipse(
            graphics,
            body,
            0.08 * edge,
            0.06 * edge,
            0.84 * edge,
            0.84 * edge,
        );
        // 描边：更深的同色系，界定边界。
        stroke_ellipse(
            graphics,
            border,
            0.05 * edge,
            0.08 * edge,
            0.06 * edge,
            0.84 * edge,
            0.84 * edge,
        );

        match label {
            Some(label) => draw_label(graphics, edge, label),
            None => {
                // 图标：白色镜头环 + 中心点（阶段 01 的占位图形）。
                stroke_ellipse(
                    graphics,
                    argb(255, 255, 255, 255),
                    0.075 * edge,
                    0.30 * edge,
                    0.28 * edge,
                    0.40 * edge,
                    0.40 * edge,
                );
                fill_ellipse(
                    graphics,
                    argb(255, 255, 255, 255),
                    0.45 * edge,
                    0.43 * edge,
                    0.10 * edge,
                    0.10 * edge,
                );
            }
        }

        GdipDeleteGraphics(graphics);
    }
    Ok(())
}

/// 在球体中央绘制拖放动作文案（白色、居中、自动换行）。
///
/// 任一步 GDI+ 创建失败都静默跳过：文案只是反馈，不能影响拖放本身。
fn draw_label(graphics: *mut GpGraphics, edge: f32, label: &str) {
    let family_name = wide("Microsoft YaHei");
    let text = wide(label);
    let mut family: *mut GpFontFamily = null_mut();
    // SAFETY: 名称/文案以 NUL 结尾且在调用期间存活；每个对象使用后立即释放。
    unsafe {
        if GdipCreateFontFamilyFromName(PCWSTR(family_name.as_ptr()), null_mut(), &mut family).0
            != 0
            || family.is_null()
        {
            return;
        }

        let chars = label.chars().count() as f32;
        let em = if chars <= 5.0 {
            edge * 0.16
        } else {
            edge * 0.125
        };
        let mut font: *mut GpFont = null_mut();
        let mut format: *mut GpStringFormat = null_mut();
        if GdipCreateFont(family, em, 0, UnitPixel, &mut font).0 != 0 || font.is_null() {
            GdipDeleteFontFamily(family);
            return;
        }
        if GdipCreateStringFormat(0, 0, &mut format).0 != 0 || format.is_null() {
            GdipDeleteFont(font);
            GdipDeleteFontFamily(family);
            return;
        }

        GdipSetStringFormatAlign(format, StringAlignmentCenter);
        GdipSetStringFormatLineAlign(format, StringAlignmentCenter);
        let inset = edge * 0.12;
        let layout = RectF {
            X: inset,
            Y: inset,
            Width: edge - inset * 2.0,
            Height: edge - inset * 2.0,
        };
        let mut brush: *mut GpSolidFill = null_mut();
        if GdipCreateSolidFill(argb(255, 255, 255, 255), &mut brush).0 == 0 && !brush.is_null() {
            GdipDrawString(
                graphics,
                PCWSTR(text.as_ptr()),
                -1,
                font,
                &layout,
                format,
                brush.cast::<GpBrush>(),
            );
            GdipDeleteBrush(brush.cast::<GpBrush>());
        }

        GdipDeleteStringFormat(format);
        GdipDeleteFont(font);
        GdipDeleteFontFamily(family);
    }
}

/// 填充椭圆；创建刷子失败时跳过该图元（不阻断整体绘制）。
fn fill_ellipse(graphics: *mut GpGraphics, color: u32, x: f32, y: f32, width: f32, height: f32) {
    let mut brush: *mut GpSolidFill = null_mut();
    // SAFETY: `graphics` 有效；刷子使用后立即删除。
    unsafe {
        if GdipCreateSolidFill(color, &mut brush).0 != 0 || brush.is_null() {
            return;
        }
        GdipFillEllipse(graphics, brush.cast::<GpBrush>(), x, y, width, height);
        GdipDeleteBrush(brush.cast::<GpBrush>());
    }
}

/// 描边椭圆；创建画笔失败时跳过该图元。
fn stroke_ellipse(
    graphics: *mut GpGraphics,
    color: u32,
    stroke: f32,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
) {
    let mut pen: *mut GpPen = null_mut();
    // SAFETY: `graphics` 有效；画笔使用后立即删除。
    unsafe {
        if GdipCreatePen1(color, stroke, UnitPixel, &mut pen).0 != 0 || pen.is_null() {
            return;
        }
        GdipDrawEllipse(graphics, pen, x, y, width, height);
        GdipDeletePen(pen);
    }
}

/// ARGB（GDI+ `Color` 的 32 位表示）。
fn argb(alpha: u8, red: u8, green: u8, blue: u8) -> u32 {
    (u32::from(alpha) << 24) | (u32::from(red) << 16) | (u32::from(green) << 8) | u32::from(blue)
}

fn present_layered(
    hwnd: HWND,
    screen: HDC,
    memory: HDC,
    size: u32,
    position: PhysicalPoint,
) -> Result<(), AppError> {
    let destination = POINT {
        x: position.x,
        y: position.y,
    };
    let source = POINT { x: 0, y: 0 };
    let extent = SIZE {
        cx: size as i32,
        cy: size as i32,
    };
    let blend = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: 255,
        AlphaFormat: AC_SRC_ALPHA as u8,
    };
    // SAFETY: 句柄有效；`destination`/`source`/`extent`/`blend` 在调用期间存活。
    unsafe {
        UpdateLayeredWindow(
            hwnd,
            Some(screen),
            Some(&destination),
            Some(&extent),
            Some(memory),
            Some(&source),
            COLORREF(0),
            Some(&blend),
            ULW_ALPHA,
        )
    }
    .map_err(|err| AppError::Internal(format!("UpdateLayeredWindow failed: {err}")))
}

/// COM/OLE apartment 守卫。
///
/// 拖放需要 OLE（`OleInitialize` 同时完成 STA 初始化）；OLE 不可用时回退到普通 STA，
/// 悬浮球的单击/拖动仍可用，只是没有拖放（design §3、§9.2）。
///
/// `!Send`：apartment 绑定当前线程，守卫只能由创建它的悬浮球线程 drop。
struct ComGuard {
    mode: ComMode,
    _not_send: PhantomData<*const ()>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ComMode {
    Ole,
    Com,
}

impl ComGuard {
    fn init() -> Option<ComGuard> {
        // SAFETY: 单线程 apartment 初始化；`None` 表示默认内存分配器。
        match unsafe { OleInitialize(None) } {
            Ok(()) => Some(ComGuard {
                mode: ComMode::Ole,
                _not_send: PhantomData,
            }),
            Err(err) => {
                warn!(error = %err, "OleInitialize failed; falling back to a plain STA (no drag and drop)");
                match unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.ok() {
                    Ok(()) => Some(ComGuard {
                        mode: ComMode::Com,
                        _not_send: PhantomData,
                    }),
                    Err(err) => {
                        warn!(error = %err, "CoInitializeEx failed for the floating ball thread");
                        None
                    }
                }
            }
        }
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        // SAFETY: 与初始化成对调用，且只在悬浮球线程。
        unsafe {
            match self.mode {
                ComMode::Ole => OleUninitialize(),
                ComMode::Com => CoUninitialize(),
            }
        }
    }
}

/// GDI+ 进程级守卫：启动失败返回 `None`，离开作用域时 `GdiplusShutdown`。
struct GdiplusGuard {
    token: usize,
    _not_send: PhantomData<*const ()>,
}

impl GdiplusGuard {
    fn start() -> Option<GdiplusGuard> {
        let input = GdiplusStartupInput {
            GdiplusVersion: 1,
            DebugEventCallback: 0,
            SuppressBackgroundThread: windows::core::BOOL(0),
            SuppressExternalCodecs: windows::core::BOOL(0),
        };
        let mut token: usize = 0;
        // SAFETY: 输出指针指向栈上 `usize`；`input` 在调用期间存活。
        let status = unsafe { GdiplusStartup(&mut token, &input, null_mut()) };
        if status.0 != 0 {
            error!(status = status.0, "GdiplusStartup failed");
            return None;
        }
        Some(GdiplusGuard {
            token,
            _not_send: PhantomData,
        })
    }
}

impl Drop for GdiplusGuard {
    fn drop(&mut self) {
        // SAFETY: `token` 来自本线程成功的 `GdiplusStartup`。
        unsafe { GdiplusShutdown(self.token) };
    }
}

/// UTF-16（NUL 结尾）转换，供 Win32 的 `PCWSTR` 参数使用。
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn display(
        id: &str,
        bounds: PhysicalRect,
        work_area: PhysicalRect,
        primary: bool,
    ) -> DisplayInfo {
        DisplayInfo {
            id: id.into(),
            bounds,
            work_area,
            dpi: 96,
            primary,
        }
    }

    fn point(x: i32, y: i32) -> PhysicalPoint {
        PhysicalPoint { x, y }
    }

    #[test]
    fn clamp_keeps_position_inside_the_work_area() {
        let displays = [display(
            "DISPLAY1",
            PhysicalRect::new(0, 0, 1920, 1080),
            PhysicalRect::new(0, 0, 1920, 1040),
            true,
        )];

        assert_eq!(
            clamp_ball_position(&displays, point(500, 400), 48),
            point(500, 400)
        );
        // 右下越界：右/下边界减去球宽。
        assert_eq!(
            clamp_ball_position(&displays, point(5000, 5000), 48),
            point(1920 - 48, 1040 - 48)
        );
        // 左上越界。
        assert_eq!(
            clamp_ball_position(&displays, point(-100, -100), 48),
            point(0, 0)
        );
    }

    #[test]
    fn clamp_handles_negative_virtual_coordinates() {
        let displays = [
            display(
                "DISPLAY2",
                PhysicalRect::new(-1920, 0, 1920, 1080),
                PhysicalRect::new(-1920, 0, 1920, 1080),
                false,
            ),
            display(
                "DISPLAY1",
                PhysicalRect::new(0, 0, 2560, 1440),
                PhysicalRect::new(0, 0, 2560, 1400),
                true,
            ),
        ];

        assert_eq!(
            clamp_ball_position(&displays, point(-1900, 20), 48),
            point(-1900, 20)
        );
        // 副屏左边界左侧越界。
        assert_eq!(
            clamp_ball_position(&displays, point(-3000, 20), 48),
            point(-1920, 20)
        );
    }

    #[test]
    fn clamp_respects_the_taskbar_work_area() {
        let displays = [display(
            "DISPLAY1",
            PhysicalRect::new(0, 0, 1920, 1080),
            // 底部 40px 任务栏。
            PhysicalRect::new(0, 0, 1920, 1040),
            true,
        )];

        let clamped = clamp_ball_position(&displays, point(1000, 1030), 48);
        assert_eq!(clamped, point(1000, 1040 - 48));
        assert!(clamped.y + 48 <= 1040);
    }

    #[test]
    fn clamp_uses_the_nearest_display() {
        // 两台显示器之间留 1000px 空档：落在空档里的位置按最近的左屏夹取。
        let displays = [
            display(
                "LEFT",
                PhysicalRect::new(-2000, 0, 500, 1080),
                PhysicalRect::new(-2000, 0, 500, 1080),
                false,
            ),
            display(
                "RIGHT",
                PhysicalRect::new(0, 0, 1920, 1080),
                PhysicalRect::new(0, 0, 1920, 1080),
                true,
            ),
        ];

        // (-1400, 500) 距左屏 100px、距右屏 1400px：左屏工作区右边界 = -2000 + 500 - 48。
        assert_eq!(
            clamp_ball_position(&displays, point(-1400, 500), 48),
            point(-2000 + 500 - 48, 500)
        );
    }

    #[test]
    fn clamp_without_displays_returns_the_desired_position() {
        assert_eq!(clamp_ball_position(&[], point(-5, 7), 48), point(-5, 7));
    }

    #[test]
    fn clamp_keeps_top_left_when_the_ball_is_larger_than_the_work_area() {
        let displays = [display(
            "TINY",
            PhysicalRect::new(10, 20, 30, 30),
            PhysicalRect::new(10, 20, 30, 30),
            true,
        )];

        assert_eq!(
            clamp_ball_position(&displays, point(100, 100), 48),
            point(10, 20)
        );
    }

    #[test]
    fn restore_uses_saved_positions() {
        let displays = [
            display(
                "LEFT",
                PhysicalRect::new(-1000, 0, 1000, 1000),
                PhysicalRect::new(-1000, 0, 1000, 1000),
                false,
            ),
            display(
                "MAIN",
                PhysicalRect::new(0, 0, 1000, 1000),
                PhysicalRect::new(0, 0, 1000, 1000),
                true,
            ),
        ];

        // 只有副屏记录：恢复到副屏（归一化中心 (-100, 900) → 左上角 (-124, 876)）。
        let mut positions = HashMap::new();
        positions.insert("LEFT".to_string(), (0.9, 0.9));
        assert_eq!(
            restore_ball_position(&positions, &displays, 48),
            point(-124, 876)
        );

        // 主屏有记录时优先主屏：(0.5, 0.5) → 左上角 (476, 476)。
        positions.insert("MAIN".to_string(), (0.5, 0.5));
        assert_eq!(
            restore_ball_position(&positions, &displays, 48),
            point(476, 476)
        );

        // 没有任何记录：主屏工作区右下角留边距。
        assert_eq!(
            restore_ball_position(&HashMap::new(), &displays, 48),
            point(1000 - 48 - DEFAULT_MARGIN, 1000 - 48 - DEFAULT_MARGIN)
        );
    }

    /// I-1：捕获丢失（`WM_CAPTURECHANGED`）后，再收到移动消息也不能移动球。
    #[test]
    fn capture_lost_ignores_later_moves() {
        let mut drag = DragState::inactive();
        drag.begin((100, 100), point(10, 20));
        // 拖动过一段：窗口移动到 (60, 70)。
        assert_eq!(
            drag.on_move((150, 150), true),
            DragUpdate::Move(point(60, 70))
        );

        // 捕获被抢走：立即结束拖动（不依赖 WM_LBUTTONUP）。
        assert!(drag.cancel());
        assert!(!drag.is_active());
        assert!(!drag.cancel(), "cancel is idempotent");

        // 之后即使仍收到 WM_MOUSEMOVE，也不会再产生移动。
        assert_eq!(drag.on_move((900, 900), true), DragUpdate::Ignore);
        assert_eq!(drag.on_move((1100, 800), true), DragUpdate::Ignore);
        // 迟到的 WM_LBUTTONUP 也不产生单击/位置上报。
        assert_eq!(drag.finish(), None);
    }

    /// I-1 兜底：左键已不在按下状态时，移动消息直接结束拖动。
    #[test]
    fn move_with_the_left_button_up_cancels_the_drag() {
        let mut drag = DragState::inactive();
        drag.begin((10, 10), point(0, 0));

        assert_eq!(drag.on_move((40, 40), false), DragUpdate::Cancelled);
        assert!(!drag.is_active());
        assert_eq!(drag.on_move((40, 40), true), DragUpdate::Ignore);
    }

    /// 未越阈值不移动（单击），越阈值后按光标位移移动。
    #[test]
    fn drag_moves_only_after_the_threshold() {
        let mut drag = DragState::inactive();
        drag.begin((100, 100), point(10, 20));

        assert_eq!(drag.on_move((102, 103), true), DragUpdate::Pending);
        assert_eq!(
            drag.on_move((140, 130), true),
            DragUpdate::Move(point(50, 50))
        );
        // 越过阈值后即使回到阈值内也继续跟随光标（保持拖动）。
        assert_eq!(
            drag.on_move((101, 101), true),
            DragUpdate::Move(point(11, 21))
        );
        assert_eq!(drag.finish(), Some(true));
        assert_eq!(drag.finish(), None);
    }

    /// 未越阈值就松开 = 单击。
    #[test]
    fn click_below_the_threshold_reports_no_movement() {
        let mut drag = DragState::inactive();
        drag.begin((5, 5), point(7, 8));
        assert_eq!(drag.on_move((8, 8), true), DragUpdate::Pending);
        assert_eq!(drag.finish(), Some(false));
        assert!(!drag.is_active());
    }
}
