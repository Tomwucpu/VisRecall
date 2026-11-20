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
use std::time::{Duration, Instant};

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
    DashCapFlat, FillModeAlternate, GdipAddPathArc, GdipAddPathLine, GdipClosePathFigure,
    GdipCreateBitmapFromScan0, GdipCreateFont, GdipCreateFontFamilyFromName, GdipCreatePath,
    GdipCreatePen1, GdipCreateSolidFill, GdipCreateStringFormat, GdipDeleteBrush, GdipDeleteFont,
    GdipDeleteFontFamily, GdipDeleteGraphics, GdipDeletePath, GdipDeletePen,
    GdipDeleteStringFormat, GdipDisposeImage, GdipDrawArc, GdipDrawLine, GdipDrawPath,
    GdipDrawString, GdipFillPath, GdipGetImageGraphicsContext, GdipGraphicsClear,
    GdipSetPenLineCap197819, GdipSetSmoothingMode, GdipSetStringFormatAlign,
    GdipSetStringFormatLineAlign, GdipSetTextRenderingHint, GdiplusShutdown, GdiplusStartup,
    GdiplusStartupInput, GpBitmap, GpBrush, GpFont, GpFontFamily, GpGraphics, GpImage, GpPath,
    GpPen, GpSolidFill, GpStringFormat, LineCapRound, RectF, SmoothingModeAntiAlias,
    StringAlignmentCenter, TextRenderingHintAntiAlias, UnitPixel,
};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Ole::{OleInitialize, OleUninitialize};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
    VK_LBUTTON,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW,
    GetWindowLongPtrW, KillTimer, LoadCursorW, PostMessageW, PostQuitMessage, RegisterClassW,
    SetTimer, SetWindowLongPtrW, ShowWindow, SystemParametersInfoW, TranslateMessage,
    UnregisterClassW, UpdateLayeredWindow, CS_HREDRAW, CS_VREDRAW, GWLP_USERDATA, HTCLIENT,
    HTTRANSPARENT, IDC_ARROW, MSG, SPI_GETWORKAREA, SW_HIDE, SW_SHOWNOACTIVATE,
    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, ULW_ALPHA, WM_APP, WM_CAPTURECHANGED, WM_CLOSE,
    WM_DESTROY, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_NCHITTEST, WM_SETTINGCHANGE,
    WM_TIMER, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_POPUP,
};

use crate::ball::anim::{
    hits_visual, tooltip_rect, tooltip_width, window_rect, BallMetrics, CornerRadii, Curve,
    Transition, VisualFrame,
};
use crate::drop_target::{DropRegistration, DropVisual, WM_BALL_DROP_VISUAL};

pub mod anim;
pub mod state;

use crate::{CaptureTrigger, ShellEvent};
pub use state::{
    clamp_size, BallAction, BallEdge, BallEvent, BallSettings, BallStateMachine, BallVisual,
    BALL_MAX_SIZE, BALL_MIN_SIZE, COLLAPSE_DELAY_MS, EDGE_SNAP_PX, FULLSCREEN_TICKS,
    RETRACT_DELAY_MS,
};

/// 悬浮球窗口类名（`FindWindowW("VisRecallBall")` 可用于排查）。
pub const BALL_CLASS_NAME: &str = "VisRecallBall";
/// 窗口标题；`WS_EX_TOOLWINDOW` 下只在调试工具里可见。
const BALL_WINDOW_TITLE: &str = "VisRecall Ball";

/// 默认尺寸（与 `BallConfig::default().size` 一致）。
pub const DEFAULT_BALL_SIZE: u32 = 56;
/// 尺寸下限：与设置页滑动条一致。
pub const MIN_BALL_SIZE: u32 = 50;
/// 尺寸上限：与设置页滑动条一致。
pub const MAX_BALL_SIZE: u32 = 150;
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
/// 尺寸热更新：`wparam` = 新边长。
///
/// 取值避开 `WM_APP + 1`（可见性）与 `WM_APP + 2`（OLE 拖放视觉，见 `drop_target`）。
const WM_BALL_SET_SIZE: u32 = WM_APP + 10;
/// 设置补丁：`wparam` 打包 size / edge_hide / hide_in_fullscreen。
const WM_BALL_APPLY_SETTINGS: u32 = WM_APP + 11;
/// app-core 回推进度/结果：`wparam` 由 [`BallFeedback::pack`] 打包。
const WM_BALL_FEEDBACK: u32 = WM_APP + 12;

/// `WM_MOUSELEAVE`：`windows` crate 未导出该常量，按 `winuser.h` 定义。
const WM_MOUSELEAVE: u32 = 0x02A3;

/// 状态机截止时间（收起/展开/回缩）的定时器。
const TIMER_STATE: usize = 1;
/// 全屏检测的 1s 采样定时器。
const TIMER_FULLSCREEN: usize = 2;
/// 形变/缩放/光环的 60fps 驱动。
const TIMER_ANIM: usize = 3;
/// 进度结果（对勾/失败）复位。
const TIMER_FEEDBACK: usize = 4;

/// 球体常态不透明度（原型 0.76）。
const BALL_OPACITY: f32 = 0.76;
/// 拖入放大倍数。
const DROP_SCALE: f32 = 1.14;
/// 拖动放大倍数。
const DRAG_SCALE: f32 = 1.08;
/// 形变/移动动画时长。
const MORPH_MS: u64 = 420;
/// 缩放动画时长。
const SCALE_MS: u64 = 340;
/// 对勾保持时长。
const SUCCESS_MS: u64 = 720;
/// 失败反馈保持时长。
const FAILURE_MS: u64 = 600;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Theme {
    Light,
    Dark,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IconKind {
    Default,
    Drop,
    Check,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FeedbackState {
    Idle,
    Busy,
    Success,
    Failure,
}

/// 每帧传给绘制的离散状态（不参与插值）。
struct PaintPayload {
    icon: IconKind,
    /// 0..=1 的确定进度；`None` 且 `indeterminate` 时画旋转弧。
    progress: Option<f32>,
    indeterminate: bool,
    failed: bool,
    accepted: bool,
    halo_rotation: f32,
    halo_breath: f32,
    ring_rotation: f32,
    tooltip: Option<(String, bool)>,
}

/// 打包 `BallSettings` 到 `wparam`：低 16 位尺寸，第 16/17 位两个开关。
fn pack_settings(settings: BallSettings) -> usize {
    (settings.size as usize & 0xFFFF)
        | ((settings.edge_hide as usize) << 16)
        | ((settings.hide_in_fullscreen as usize) << 17)
}

fn unpack_settings(packed: usize) -> BallSettings {
    BallSettings {
        size: (packed & 0xFFFF) as u32,
        edge_hide: packed & (1 << 16) != 0,
        hide_in_fullscreen: packed & (1 << 17) != 0,
    }
}
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
    /// 贴边收起（P2-R10）。
    pub edge_hide: bool,
    /// 全屏时自动隐藏（P2-R10）。
    pub hide_in_fullscreen: bool,
}

impl Default for BallConfig {
    fn default() -> Self {
        Self {
            size: DEFAULT_BALL_SIZE,
            positions: HashMap::new(),
            edge_hide: true,
            hide_in_fullscreen: true,
        }
    }
}

/// app-core 回推给悬浮球的导入反馈。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BallFeedback {
    /// 导入进行中；`progress` 为 0..=99，`None` 表示不确定进度。
    Busy {
        progress: Option<u8>,
    },
    Success,
    Failure,
}

impl BallFeedback {
    /// 打包到 `WM_BALL_FEEDBACK` 的 `wparam`。
    ///
    /// 进度加一存储，用 0 表示「不确定」，从而能区分 `Some(0)`。
    pub fn pack(self) -> usize {
        match self {
            BallFeedback::Busy { progress: None } => 0,
            BallFeedback::Busy {
                progress: Some(progress),
            } => (usize::from(progress.min(99)) + 1) << 8,
            BallFeedback::Success => 1,
            BallFeedback::Failure => 2,
        }
    }

    pub fn unpack(payload: usize) -> Self {
        let raw = (payload >> 8) & 0xFF;
        match payload & 0x3 {
            1 => BallFeedback::Success,
            2 => BallFeedback::Failure,
            _ => BallFeedback::Busy {
                progress: if raw > 0 { Some((raw - 1) as u8) } else { None },
            },
        }
    }
}

/// 可跨线程克隆的反馈句柄；窗口已销毁时发送为空操作。
#[derive(Clone, Debug)]
pub struct BallFeedbackHandle {
    hwnd: Arc<AtomicIsize>,
}

impl BallFeedbackHandle {
    /// 测试或未启动运行时使用的空句柄。
    pub fn detached() -> Self {
        Self {
            hwnd: Arc::new(AtomicIsize::new(0)),
        }
    }

    pub fn send(&self, feedback: BallFeedback) {
        let raw = self.hwnd.load(Ordering::SeqCst);
        if raw == 0 {
            return;
        }
        // SAFETY: `hwnd` 来自本进程的悬浮球线程；窗口销毁后消息返回失败，只记日志。
        if let Err(err) = unsafe {
            PostMessageW(
                Some(HWND(raw as *mut c_void)),
                WM_BALL_FEEDBACK,
                WPARAM(feedback.pack()),
                LPARAM(0),
            )
        } {
            debug!(error = %err, "failed to post feedback to the floating ball");
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

    /// 供 app-core 跨线程回推进度/结果；句柄可克隆，窗口销毁后自动失效。
    pub fn feedback_handle(&self) -> BallFeedbackHandle {
        BallFeedbackHandle {
            hwnd: Arc::clone(&self.hwnd),
        }
    }

    /// 热更新直径：保持中心点并夹回工作区（P2-R10）。
    pub fn set_size(&self, size: u32) {
        self.post(WM_BALL_SET_SIZE, size as usize);
    }

    /// 应用悬浮球设置（尺寸 + 贴边收起 + 全屏隐藏）；同值不会造成多余重绘。
    pub fn apply_settings(&self, settings: BallSettings) {
        self.post(WM_BALL_APPLY_SETTINGS, pack_settings(settings));
    }

    fn post(&self, message: u32, payload: usize) {
        let raw = self.hwnd.load(Ordering::SeqCst);
        if raw == 0 {
            debug!(
                message,
                "floating ball window unavailable; settings change ignored"
            );
            return;
        }
        // SAFETY: `hwnd` 属于本进程的悬浮球线程；窗口已销毁时投递失败只记日志。
        if let Err(err) = unsafe {
            PostMessageW(
                Some(HWND(raw as *mut c_void)),
                message,
                WPARAM(payload),
                LPARAM(0),
            )
        } {
            warn!(message, error = %err, "failed to post message to the floating ball");
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
    let (ready_tx, ready_rx) = bounded::<Result<(), AppError>>(1);

    let thread_hwnd = Arc::clone(&hwnd);
    let thread_visible = Arc::clone(&visible);

    let spawned = thread::Builder::new()
        .name("visrecall-ball".into())
        .spawn(move || ball_thread_main(config, events, ready_tx, thread_hwnd, thread_visible));

    match spawned {
        Ok(_) => match ready_rx.recv_timeout(BALL_READY_TIMEOUT) {
            // 成功路径由线程内的「floating ball window created」记录（含尺寸与位置）。
            Ok(Ok(())) => {}
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
    let machine = BallStateMachine::new(
        BallSettings {
            size,
            edge_hide: config.edge_hide,
            hide_in_fullscreen: config.hide_in_fullscreen,
        },
        PhysicalPoint {
            x: position.x + size as i32 / 2,
            y: position.y + size as i32 / 2,
        },
        work_area_for(&displays, position, size),
        0,
    );
    let drop_visual = Arc::new(Mutex::new(DropVisual::default()));
    let drop_events = events.clone();
    let metrics = BallMetrics::new(size);
    let initial_frame = ball_frame(
        PhysicalRect::new(position.x, position.y, size, size),
        metrics,
    );
    let theme = system_theme();
    let state = Box::new(RefCell::new(BallWindowState {
        base_size: size,
        size,
        position,
        events,
        visible: Arc::clone(visible),
        drag: DragState::inactive(),
        hwnd_slot: Arc::clone(&hwnd_slot),
        drop_visual: Arc::clone(&drop_visual),
        drop_target: None,
        machine,
        started: Instant::now(),
        fullscreen: false,
        tracking_leave: false,
        theme,
        frame: initial_frame,
        transition: None,
        feedback: FeedbackState::Idle,
        feedback_progress: None,
        feedback_until: None,
        drop_active: false,
        tooltip: None,
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

    let initial_window = window_rect(initial_frame.rect, None, false, metrics);
    if let Err(err) = render_ball(
        hwnd,
        initial_window,
        &initial_frame,
        &PaintPayload {
            icon: IconKind::Default,
            progress: None,
            indeterminate: false,
            failed: false,
            accepted: true,
            halo_rotation: 0.0,
            halo_breath: 1.0,
            ring_rotation: 0.0,
            tooltip: None,
        },
        theme,
        metrics,
    ) {
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

/// 球所在显示器的工作区；枚举不到显示器时退回系统的工作区查询。
fn work_area_for(displays: &[DisplayInfo], position: PhysicalPoint, size: u32) -> PhysicalRect {
    let center = PhysicalPoint {
        x: position.x + size as i32 / 2,
        y: position.y + size as i32 / 2,
    };
    if let Some(display) = display_at_or_nearest(displays, center) {
        return display.work_area;
    }
    let mut rect = RECT::default();
    // SAFETY: 输出指针指向栈上 `RECT`。
    if unsafe {
        SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some(&mut rect as *mut RECT as *mut c_void),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    }
    .is_ok()
    {
        return PhysicalRect::new(
            rect.left,
            rect.top,
            (rect.right - rect.left).max(1) as u32,
            (rect.bottom - rect.top).max(1) as u32,
        );
    }
    PhysicalRect::new(0, 0, 1920, 1080)
}

/// 全屏/演示/独占全屏判定；查询失败按「非全屏」处理（design §6.10）。
fn is_fullscreen() -> bool {
    use windows::Win32::UI::Shell::{
        SHQueryUserNotificationState, QUNS_BUSY, QUNS_PRESENTATION_MODE,
        QUNS_RUNNING_D3D_FULL_SCREEN,
    };

    // SAFETY: 无参数查询，不持有任何指针。
    match unsafe { SHQueryUserNotificationState() } {
        Ok(state) => matches!(
            state,
            QUNS_BUSY | QUNS_PRESENTATION_MODE | QUNS_RUNNING_D3D_FULL_SCREEN
        ),
        Err(err) => {
            trace!(error = %err, "SHQueryUserNotificationState failed; assuming not fullscreen");
            false
        }
    }
}

/// 系统「应用主题」：`AppsUseLightTheme == 0` 视为深色。
fn system_theme() -> Theme {
    let subkey = wide(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
    let value = wide("AppsUseLightTheme");
    let mut data: u32 = 1;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: 子键/值名以 NUL 结尾且在调用期间存活；输出缓冲为栈上 u32。
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(subkey.as_ptr()),
            PCWSTR(value.as_ptr()),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut data as *mut u32 as *mut c_void),
            Some(&mut size),
        )
    };
    if status.0 == 0 && data == 0 {
        Theme::Dark
    } else {
        Theme::Light
    }
}

struct Palette {
    body: (u8, u8, u8),
    border: u32,
    icon: u32,
    tooltip_bg: u32,
    tooltip_text: u32,
    ring_track: u32,
    highlight: u32,
}

fn palette(theme: Theme, accepted: bool) -> Palette {
    if !accepted {
        return Palette {
            body: (107, 114, 128),
            border: argb(255, 75, 85, 99),
            icon: argb(255, 255, 255, 255),
            tooltip_bg: argb(255, 75, 85, 99),
            tooltip_text: argb(255, 255, 255, 255),
            ring_track: argb(32, 255, 255, 255),
            highlight: argb(60, 255, 255, 255),
        };
    }
    match theme {
        Theme::Light => Palette {
            body: (255, 255, 255),
            border: argb(36, 0, 0, 0),
            icon: argb(255, 17, 17, 17),
            tooltip_bg: argb(255, 17, 17, 17),
            tooltip_text: argb(255, 255, 255, 255),
            ring_track: argb(32, 17, 17, 17),
            highlight: argb(120, 255, 255, 255),
        },
        Theme::Dark => Palette {
            body: (26, 26, 28),
            border: argb(46, 255, 255, 255),
            icon: argb(255, 245, 245, 247),
            tooltip_bg: argb(255, 245, 245, 247),
            tooltip_text: argb(255, 17, 17, 17),
            ring_track: argb(32, 245, 245, 247),
            highlight: argb(36, 255, 255, 255),
        },
    }
}

fn ball_frame(rect: PhysicalRect, _metrics: BallMetrics) -> VisualFrame {
    VisualFrame {
        rect,
        radii: CornerRadii::capsule(rect.width as f32 / 2.0),
        scale: 1.0,
        opacity: BALL_OPACITY,
        icon_alpha: 1.0,
        icon_scale: 1.0,
        grip_alpha: 0.0,
        halo_alpha: 0.0,
        tooltip_alpha: 0.0,
    }
}

fn strip_frame(rect: PhysicalRect, edge: BallEdge, metrics: BallMetrics) -> VisualFrame {
    VisualFrame {
        rect,
        radii: CornerRadii::docked_strip(edge, metrics.strip_thickness as f32),
        scale: 1.0,
        opacity: BALL_OPACITY * 0.85,
        icon_alpha: 0.0,
        icon_scale: 0.35,
        grip_alpha: 0.42,
        halo_alpha: 0.0,
        tooltip_alpha: 0.0,
    }
}

fn frame_for(state: &BallWindowState) -> VisualFrame {
    let metrics = BallMetrics::new(state.base_size);
    match state.machine.visual() {
        BallVisual::Strip => strip_frame(
            state.machine.strip_rect(),
            state.machine.anchor().unwrap_or(BallEdge::Left),
            metrics,
        ),
        _ => ball_frame(state.machine.current_rect(), metrics),
    }
}

/// 从当前帧过渡到目标帧；拖入/拖动时叠加放大与光环。
fn start_transition(hwnd: HWND, mut target: VisualFrame, duration_ms: u64, curve: Curve) {
    let now = now_ms(hwnd);
    let Some((from, drop_active, dragging)) = with_state(hwnd, |state| {
        (state.frame, state.drop_active, state.drag.is_active())
    }) else {
        return;
    };
    if drop_active {
        target.scale = DROP_SCALE;
        target.halo_alpha = 1.0;
        target.tooltip_alpha = 1.0;
    } else if dragging {
        target.scale = DRAG_SCALE;
    }
    let _ = with_state_mut(hwnd, |state| {
        state.transition = Some(Transition {
            from,
            to: target,
            start_ms: now,
            duration_ms,
            curve,
        });
    });
    // SAFETY: `hwnd` 属于本线程；同一 id 重复 SetTimer 只保留最近一次。
    unsafe {
        SetTimer(Some(hwnd), TIMER_ANIM, 16, None);
    }
}

fn advance_animation(hwnd: HWND) {
    let now = now_ms(hwnd);
    let keep = with_state_mut(hwnd, |state| {
        if let Some(transition) = &state.transition {
            let (frame, finished) = transition.sample(now);
            state.frame = frame;
            if finished {
                state.transition = None;
            }
        }
        state.transition.is_some()
            || state.drop_active
            || (state.feedback == FeedbackState::Busy && state.feedback_progress.is_none())
    })
    .unwrap_or(false);

    render_current(hwnd);

    if !keep {
        // SAFETY: 与 `SetTimer(TIMER_ANIM)` 成对；没有动画/光环/不确定进度时停表。
        unsafe {
            let _ = KillTimer(Some(hwnd), TIMER_ANIM);
        }
    }
}

fn render_current(hwnd: HWND) {
    let now = now_ms(hwnd);
    let Some((
        frame,
        base_size,
        theme,
        feedback,
        feedback_progress,
        drop_active,
        anchor,
        cached_tooltip,
    )) = with_state(hwnd, |state| {
        (
            state.frame,
            state.base_size,
            state.theme,
            state.feedback,
            state.feedback_progress,
            state.drop_active,
            state.machine.anchor(),
            state.tooltip.clone(),
        )
    })
    else {
        return;
    };
    let metrics = BallMetrics::new(base_size);
    let drop = with_state(hwnd, |state| {
        state
            .drop_visual
            .lock()
            .map(|visual| visual.clone())
            .unwrap_or_default()
    })
    .unwrap_or_default();

    let tooltip = if drop_active {
        let text = if drop.active && !drop.label.is_empty() {
            drop.label.clone()
        } else {
            "松开存入".to_string()
        };
        let tip = Some((text, drop.accepted));
        let _ = with_state_mut(hwnd, |state| state.tooltip = tip.clone());
        tip
    } else {
        cached_tooltip
    };

    // 气泡淡出期间继续保留顶部空间，否则窗口先缩小会把气泡裁掉。
    let reserve_tip = drop_active || frame.tooltip_alpha > 0.01;
    let mut window = window_rect(frame.rect, anchor, reserve_tip, metrics);
    if reserve_tip {
        if let Some((text, _)) = &tooltip {
            let desired = tooltip_width(text.chars().count(), metrics) + metrics.shadow_margin * 2;
            if desired > window.width {
                let grow = (desired - window.width).div_ceil(2);
                window = PhysicalRect::new(
                    window.x - grow as i32,
                    window.y,
                    window.width + grow * 2,
                    window.height,
                );
            }
        }
    }
    let icon = if feedback == FeedbackState::Success {
        IconKind::Check
    } else if drop_active {
        IconKind::Drop
    } else {
        IconKind::Default
    };
    let (progress, indeterminate, failed) = match feedback {
        FeedbackState::Busy => (
            feedback_progress.map(|value| f32::from(value.min(99)) / 100.0),
            feedback_progress.is_none(),
            false,
        ),
        FeedbackState::Failure => (Some(1.0), false, true),
        FeedbackState::Idle | FeedbackState::Success => (None, false, false),
    };
    let elapsed = now as f32;
    let halo_rotation = (elapsed % 8000.0) / 8000.0 * std::f32::consts::TAU;
    let halo_breath =
        0.45 + 0.5 * (0.5 + 0.5 * (elapsed % 1600.0 / 1600.0 * std::f32::consts::TAU).sin());

    let payload = PaintPayload {
        icon,
        progress,
        indeterminate,
        failed,
        accepted: drop.accepted,
        halo_rotation,
        halo_breath,
        ring_rotation: (elapsed % 1200.0) / 1200.0 * std::f32::consts::TAU,
        tooltip,
    };
    if let Err(err) = render_ball(hwnd, window, &frame, &payload, theme, metrics) {
        warn!(error = %err, "failed to render the floating ball");
    }
}

fn apply_feedback(hwnd: HWND, feedback: BallFeedback) {
    let now = now_ms(hwnd);
    match feedback {
        BallFeedback::Busy { progress } => {
            let _ = with_state_mut(hwnd, |state| {
                state.feedback = FeedbackState::Busy;
                state.feedback_progress = progress;
                state.feedback_until = None;
            });
        }
        BallFeedback::Success => {
            let _ = with_state_mut(hwnd, |state| {
                state.feedback = FeedbackState::Success;
                state.feedback_progress = None;
                state.feedback_until = Some(now + SUCCESS_MS);
            });
            // SAFETY: 同一 id 重复 SetTimer 只保留最近一次。
            unsafe {
                SetTimer(
                    Some(hwnd),
                    TIMER_FEEDBACK,
                    SUCCESS_MS.min(u64::from(u32::MAX)) as u32,
                    None,
                );
            }
        }
        BallFeedback::Failure => {
            let _ = with_state_mut(hwnd, |state| {
                state.feedback = FeedbackState::Failure;
                state.feedback_progress = None;
                state.feedback_until = Some(now + FAILURE_MS);
            });
            // SAFETY: 同上。
            unsafe {
                SetTimer(
                    Some(hwnd),
                    TIMER_FEEDBACK,
                    FAILURE_MS.min(u64::from(u32::MAX)) as u32,
                    None,
                );
            }
        }
    }
    // 让绘制立即反映新状态；动画定时器会在无持续动画时自行停表。
    // SAFETY: 同上。
    unsafe {
        SetTimer(Some(hwnd), TIMER_ANIM, 16, None);
    }
    render_current(hwnd);
}

fn reset_feedback(hwnd: HWND) {
    let _ = with_state_mut(hwnd, |state| {
        state.feedback = FeedbackState::Idle;
        state.feedback_progress = None;
        state.feedback_until = None;
    });
    // SAFETY: 与 SetTimer(TIMER_FEEDBACK) 成对。
    unsafe {
        let _ = KillTimer(Some(hwnd), TIMER_FEEDBACK);
    }
    render_current(hwnd);
}

/// 把状态机产出的动作落到窗口上。
fn apply_actions(hwnd: HWND, actions: Vec<BallAction>) {
    for action in actions {
        match action {
            BallAction::Show { rect } => {
                let target = with_state_mut(hwnd, |state| {
                    state.position = PhysicalPoint {
                        x: rect.x,
                        y: rect.y,
                    };
                    state.size = state.base_size;
                    frame_for(state)
                });
                if let Some(target) = target {
                    start_transition(hwnd, target, MORPH_MS, Curve::Spring);
                }
                // 长条/球都在「用户希望可见」时才显示；捕获会话期间由可见性开关兜住。
                if with_state(hwnd, |state| state.visible.load(Ordering::SeqCst)).unwrap_or(false) {
                    // SAFETY: `hwnd` 属于本线程；显示不激活窗口。
                    unsafe {
                        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                    }
                }
                render_current(hwnd);
            }
            BallAction::Hide => {
                // SAFETY: `hwnd` 属于本线程。
                unsafe {
                    let _ = ShowWindow(hwnd, SW_HIDE);
                    let _ = KillTimer(Some(hwnd), TIMER_ANIM);
                }
            }
            // 尺寸先同步到绘制参数，随后同批次的 `Show` 会按新尺寸重算目标帧。
            BallAction::Resize { size, .. } => {
                let _ = with_state_mut(hwnd, |state| {
                    state.base_size = size;
                    state.size = size;
                });
            }
            BallAction::ScheduleTimer { after_ms } => {
                // SAFETY: 同一个 id 重复 SetTimer 会覆盖上一个超时，正是「只保留最近一次」的语义。
                unsafe {
                    SetTimer(
                        Some(hwnd),
                        TIMER_STATE,
                        after_ms.min(u64::from(u32::MAX)) as u32,
                        None,
                    );
                }
            }
        }
    }
}

/// 推进状态机一次并落地动作；`now_ms` 由线程起始时刻换算。
fn advance(hwnd: HWND, event: BallEvent) {
    let actions = with_state_mut(hwnd, |state| state.machine.handle(event)).unwrap_or_default();
    apply_actions(hwnd, actions);
}

/// 窗口消息处理所需的状态；只在悬浮球线程访问，每次消息短借用（见 [`with_state`]）。
struct BallWindowState {
    /// 配置的球体边长（不随动画变化）。
    base_size: u32,
    /// 当前逻辑球径（与 `base_size` 同步，拖入不再改它）。
    size: u32,
    /// 当前视觉左上角（全局物理像素）。
    position: PhysicalPoint,
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
    /// 贴边/收起/展开/全屏的纯状态机（见 `ball::state`）。
    machine: BallStateMachine,
    /// 线程起始时刻：状态机的时间轴统一用它换算毫秒。
    started: Instant,
    /// 最近一次全屏采样结果（`WM_TIMER` 的状态推进要用）。
    fullscreen: bool,
    /// 鼠标是否已向系统注册了 `WM_MOUSELEAVE`（避免每次移动都重复注册）。
    tracking_leave: bool,
    theme: Theme,
    /// 当前绘制的视觉帧（动画采样结果）。
    frame: VisualFrame,
    transition: Option<Transition>,
    feedback: FeedbackState,
    feedback_progress: Option<u8>,
    feedback_until: Option<u64>,
    drop_active: bool,
    /// 最近一次拖入气泡文案；拖入结束时保留到淡出完成。
    tooltip: Option<(String, bool)>,
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

/// `WM_NCHITTEST`：只让球体/胶囊（含贴边留白）接收鼠标，其余透明区域穿透。
fn hit_test(hwnd: HWND) -> isize {
    let mut point = POINT::default();
    // SAFETY: 输出指针指向栈上 `POINT`。
    if unsafe { GetCursorPos(&mut point) }.is_err() {
        return HTCLIENT as isize;
    }
    let Some((visual, anchor, size)) = with_state(hwnd, |state| {
        (state.frame.rect, state.machine.anchor(), state.base_size)
    }) else {
        return HTTRANSPARENT as isize;
    };
    let metrics = BallMetrics::new(size);
    if hits_visual(
        visual,
        anchor,
        metrics,
        PhysicalPoint {
            x: point.x,
            y: point.y,
        },
    ) {
        HTCLIENT as isize
    } else {
        HTTRANSPARENT as isize
    }
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
        WM_BALL_SET_SIZE => {
            apply_settings_message(hwnd, wparam.0);
            LRESULT(0)
        }
        WM_BALL_APPLY_SETTINGS => {
            apply_settings_message(hwnd, wparam.0);
            LRESULT(0)
        }
        WM_BALL_FEEDBACK => {
            apply_feedback(hwnd, BallFeedback::unpack(wparam.0));
            LRESULT(0)
        }
        WM_NCHITTEST => LRESULT(hit_test(hwnd)),
        WM_SETTINGCHANGE => {
            let theme = system_theme();
            let _ = with_state_mut(hwnd, |state| state.theme = theme);
            render_current(hwnd);
            LRESULT(0)
        }
        WM_TIMER => {
            on_timer(hwnd, wparam.0);
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            on_mouse_move(hwnd);
            drag_ball(hwnd);
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            let _ = with_state_mut(hwnd, |state| state.tracking_leave = false);
            let now = now_ms(hwnd);
            advance(hwnd, BallEvent::CursorLeave { now_ms: now });
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
    // 收起态按下先展开：既方便拖动，也让单击时的视觉反馈完整。
    if with_state(hwnd, |state| state.machine.visual() == BallVisual::Strip).unwrap_or(false) {
        let now = now_ms(hwnd);
        advance(hwnd, BallEvent::CursorEnter { now_ms: now });
    }

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
        let rect = state.machine.current_rect();
        let origin = PhysicalPoint {
            x: rect.x,
            y: rect.y,
        };
        state.position = origin;
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
    let Some(update) = with_state_mut(hwnd, |state| {
        state.drag.on_move((cursor.x, cursor.y), pressed)
    }) else {
        return;
    };

    match update {
        DragUpdate::Move(target) => {
            // 首次越过阈值且仍贴边：脱锚，回到自由球形态，后续完全跟手。
            let undock =
                with_state(hwnd, |state| state.machine.anchor().is_some()).unwrap_or(false);
            let _ = with_state_mut(hwnd, |state| {
                if undock {
                    state.machine.handle(BallEvent::DragStarted);
                    state.transition = None;
                }
                state.position = target;
                state.frame.rect =
                    PhysicalRect::new(target.x, target.y, state.base_size, state.base_size);
                state.frame.radii = CornerRadii::capsule(state.base_size as f32 / 2.0);
                state.frame.scale = DRAG_SCALE;
                state.frame.icon_alpha = 1.0;
                state.frame.icon_scale = 1.0;
                state.frame.grip_alpha = 0.0;
                state
                    .machine
                    .handle(BallEvent::Dragged { position: target });
            });
            if undock {
                // SAFETY: 与 SetTimer(TIMER_ANIM) 成对；拖动期间不需要逐帧插值。
                unsafe {
                    let _ = KillTimer(Some(hwnd), TIMER_ANIM);
                }
            }
            render_current(hwnd);
        }
        DragUpdate::Cancelled => {
            debug!("floating ball drag cancelled: left button released without WM_LBUTTONUP");
            release_capture();
            let target = with_state(hwnd, |state| {
                let mut frame = frame_for(state);
                frame.scale = 1.0;
                frame
            });
            if let Some(target) = target {
                start_transition(hwnd, target, SCALE_MS, Curve::Spring);
            }
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
        let target = with_state(hwnd, |state| {
            let mut frame = frame_for(state);
            frame.scale = 1.0;
            frame
        });
        if let Some(target) = target {
            start_transition(hwnd, target, SCALE_MS, Curve::Spring);
        }
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
        // 单击：缩放回常态（随后抓帧会隐藏球）。
        let target = with_state(hwnd, |state| {
            let mut frame = frame_for(state);
            frame.scale = 1.0;
            frame
        });
        if let Some(target) = target {
            start_transition(hwnd, target, SCALE_MS, Curve::Spring);
        }
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
    if clamped != position {
        position = clamped;
        let _ = with_state_mut(hwnd, |state| {
            state.position = clamped;
            state.frame.rect = PhysicalRect::new(clamped.x, clamped.y, size, size);
        });
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

    // 贴边判定与收起定时器交给状态机：`work_area` 用球所在显示器的工作区。
    let now = now_ms(hwnd);
    advance(
        hwnd,
        BallEvent::Released {
            position,
            work_area: target.work_area,
            now_ms: now,
        },
    );
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
    // 全屏采样只在球可见时运行：隐藏期间多一个 1s 定时器只会白烧空闲 CPU。
    // SAFETY: 同一个 id 重复 `SetTimer` 覆盖旧超时；`KillTimer` 对不存在的定时器无副作用。
    unsafe {
        if visible {
            SetTimer(Some(hwnd), TIMER_FULLSCREEN, 1_000, None);
        } else {
            let _ = KillTimer(Some(hwnd), TIMER_FULLSCREEN);
        }
    }
    // 让状态机知道「这次隐藏是会话要的」：全屏恢复时不能把球叫回来。
    let actions = with_state_mut(hwnd, |state| {
        state.machine.handle(BallEvent::SessionHidden(!visible))
    })
    .unwrap_or_default();
    if visible {
        // 会话结束：按当前形态恢复（全屏仍生效时状态机会拒绝）。
        apply_actions(hwnd, actions);
    }
    debug!(visible, "floating ball visibility changed");
}

/// 线程内单调毫秒（状态机的唯一时间源）。
fn now_ms(hwnd: HWND) -> u64 {
    with_state(hwnd, |state| state.started.elapsed().as_millis() as u64).unwrap_or(0)
}

/// 应用尺寸/设置补丁：`WM_BALL_SET_SIZE` 与 `WM_BALL_APPLY_SETTINGS` 共用。
fn apply_settings_message(hwnd: HWND, payload: usize) {
    let settings = if payload & (0x3 << 16) == 0 && payload <= u32::MAX as usize {
        // 只带尺寸的 `WM_BALL_SET_SIZE`：保留其余开关。
        with_state(hwnd, |state| BallSettings {
            size: payload as u32,
            edge_hide: state.machine.settings().edge_hide,
            hide_in_fullscreen: state.machine.settings().hide_in_fullscreen,
        })
        .unwrap_or_default()
    } else {
        unpack_settings(payload)
    };

    advance(hwnd, BallEvent::ConfigChanged { settings });
    // 尺寸变化后工作区可能不同（跨屏拖动过），顺带刷新一次并重启全屏采样。
    refresh_work_area(hwnd);
}

/// 重新计算球所在显示器的工作区，交给状态机。
fn refresh_work_area(hwnd: HWND) {
    let Some((position, size)) = with_state(hwnd, |state| (state.position, state.size)) else {
        return;
    };
    let displays = match enumerate_displays() {
        Ok(displays) => displays,
        Err(err) => {
            warn!(error = %err, "display enumeration failed; keeping the previous work area");
            return;
        }
    };
    let work_area = work_area_for(&displays, position, size);
    let rect = with_state_mut(hwnd, |state| {
        state.machine.set_work_area(work_area);
        state.machine.current_rect()
    });
    if let Some(rect) = rect {
        apply_actions(hwnd, vec![BallAction::Show { rect }]);
    }
}

/// `WM_TIMER`：区分状态截止时间与 1s 全屏采样。
fn on_timer(hwnd: HWND, timer_id: usize) {
    let now = now_ms(hwnd);
    match timer_id {
        TIMER_STATE => {
            // SAFETY: 与 `SetTimer` 成对；同一 id 的定时器只保留一个。
            unsafe {
                let _ = KillTimer(Some(hwnd), TIMER_STATE);
            }
            let fullscreen = with_state(hwnd, |state| state.fullscreen).unwrap_or(false);
            advance(
                hwnd,
                BallEvent::Tick {
                    now_ms: now,
                    fullscreen,
                },
            );
        }
        TIMER_FULLSCREEN => {
            let fullscreen = is_fullscreen();
            let _ = with_state_mut(hwnd, |state| state.fullscreen = fullscreen);
            advance(
                hwnd,
                BallEvent::Tick {
                    now_ms: now,
                    fullscreen,
                },
            );
        }
        TIMER_ANIM => advance_animation(hwnd),
        TIMER_FEEDBACK => {
            let expired = with_state(hwnd, |state| {
                state.feedback != FeedbackState::Idle
                    && state.feedback_until.is_some_and(|until| now >= until)
            })
            .unwrap_or(false);
            if expired {
                reset_feedback(hwnd);
            }
        }
        _ => {}
    }
}

/// `WM_MOUSEMOVE`：首次进入时注册 `WM_MOUSELEAVE` 并把 `CursorEnter` 交给状态机。
fn on_mouse_move(hwnd: HWND) {
    let tracking = with_state(hwnd, |state| state.tracking_leave).unwrap_or(true);
    if tracking {
        return;
    }

    let mut track = TRACKMOUSEEVENT {
        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
        dwFlags: TME_LEAVE,
        hwndTrack: hwnd,
        dwHoverTime: 0,
    };
    // SAFETY: `track` 在调用期间有效；注册失败只是收不到 `WM_MOUSELEAVE`。
    unsafe {
        let _ = TrackMouseEvent(&mut track);
    }
    let _ = with_state_mut(hwnd, |state| state.tracking_leave = true);

    let now = now_ms(hwnd);
    advance(hwnd, BallEvent::CursorEnter { now_ms: now });
}

/// 应用 OLE 拖入视觉：放大 + 光环 + 气泡，离开后还原。
///
/// 视觉状态来自共享的 [`DropVisual`]，本函数只读快照（不持锁调用窗口 API），
/// 位置/尺寸由视觉帧驱动，不再改写 `state.position` / `state.size`。
fn refresh_drop_visual(hwnd: HWND) {
    let active = with_state(hwnd, |state| {
        state
            .drop_visual
            .lock()
            .map(|slot| slot.active)
            .unwrap_or(false)
    })
    .unwrap_or(false);

    // 收起状态下开始拖入：先弹性展开，与原型「拖拽开始即弹出迎接」一致。
    if active
        && with_state(hwnd, |state| state.machine.visual() == BallVisual::Strip).unwrap_or(false)
    {
        let now = now_ms(hwnd);
        advance(hwnd, BallEvent::CursorEnter { now_ms: now });
    }
    let _ = with_state_mut(hwnd, |state| state.drop_active = active);

    let Some(target) = with_state(hwnd, |state| {
        let base = frame_for(state);
        if active {
            VisualFrame {
                scale: DROP_SCALE,
                halo_alpha: 1.0,
                tooltip_alpha: 1.0,
                ..base
            }
        } else {
            VisualFrame {
                scale: if state.drag.is_active() {
                    DRAG_SCALE
                } else {
                    1.0
                },
                halo_alpha: 0.0,
                tooltip_alpha: 0.0,
                ..base
            }
        }
    }) else {
        return;
    };
    start_transition(hwnd, target, SCALE_MS, Curve::Spring);
    render_current(hwnd);
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

/// 画一次悬浮球并用 `UpdateLayeredWindow` 呈现；绘制资源在返回前全部释放。
fn render_ball(
    hwnd: HWND,
    rect: PhysicalRect,
    frame: &VisualFrame,
    payload: &PaintPayload,
    theme: Theme,
    metrics: BallMetrics,
) -> Result<(), AppError> {
    // SAFETY: `GetDC(None)` 返回屏幕 DC；失败时为空句柄。
    let screen = unsafe { GetDC(None) };
    if screen.is_invalid() {
        return Err(AppError::Internal(format!(
            "GetDC failed for the floating ball: {}",
            WinError::from_thread()
        )));
    }
    let result = render_with_screen(hwnd, rect, screen, frame, payload, theme, metrics);
    // SAFETY: 与 `GetDC` 成对调用。
    unsafe { ReleaseDC(None, screen) };
    result
}

fn render_with_screen(
    hwnd: HWND,
    rect: PhysicalRect,
    screen: HDC,
    frame: &VisualFrame,
    payload: &PaintPayload,
    theme: Theme,
    metrics: BallMetrics,
) -> Result<(), AppError> {
    // SAFETY: `screen` 有效；失败时为空句柄。
    let memory = unsafe { CreateCompatibleDC(Some(screen)) };
    if memory.is_invalid() {
        return Err(AppError::Internal(format!(
            "CreateCompatibleDC failed: {}",
            WinError::from_thread()
        )));
    }

    let width = rect.width.max(1) as i32;
    let height = rect.height.max(1) as i32;
    let bitmap_info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            // 负高度 = top-down DIB，与 GDI+ 的扫描行序一致。
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            biSizeImage: (width * height * 4) as u32,
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

    let rendered = draw_ball(bits, rect, frame, payload, theme, metrics)
        .and_then(|()| present_layered(hwnd, screen, memory, rect));

    // SAFETY: 释放顺序为「恢复原对象 → 删除位图 → 删除内存 DC」。
    unsafe {
        SelectObject(memory, previous);
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
        let _ = DeleteDC(memory);
    }
    rendered
}

/// 用 GDI+ 在 DIB 上绘制球体/胶囊、图标与反馈（预乘 alpha，透明背景）。
fn draw_ball(
    bits: *mut c_void,
    rect: PhysicalRect,
    frame: &VisualFrame,
    payload: &PaintPayload,
    theme: Theme,
    metrics: BallMetrics,
) -> Result<(), AppError> {
    let width = rect.width.max(1) as i32;
    let height = rect.height.max(1) as i32;
    let stride = width * 4;
    let mut bitmap: *mut GpBitmap = null_mut();
    // SAFETY: `bits` 指向 `size × size × 4` 的 DIB 像素，调用期间有效；
    // PARGB 与 `UpdateLayeredWindow` 期望的预乘 alpha 一致。
    let status = unsafe {
        GdipCreateBitmapFromScan0(
            width,
            height,
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

    let painted = paint_ball(bitmap, rect, frame, payload, theme, metrics);
    // SAFETY: 位图由上面的调用创建，图形上下文已在 `paint_ball` 内释放。
    unsafe { GdipDisposeImage(bitmap.cast::<GpImage>()) };
    painted
}

fn scaled_rect(rect: PhysicalRect, scale: f32) -> PhysicalRect {
    if (scale - 1.0).abs() < f32::EPSILON {
        return rect;
    }
    let width = (rect.width as f32 * scale).round().max(1.0) as u32;
    let height = (rect.height as f32 * scale).round().max(1.0) as u32;
    PhysicalRect::new(
        rect.x + (rect.width as i32 - width as i32) / 2,
        rect.y + (rect.height as i32 - height as i32) / 2,
        width,
        height,
    )
}

fn with_alpha(color: u32, alpha: f32) -> u32 {
    let alpha = ((color >> 24) as f32 * alpha.clamp(0.0, 1.0)) as u8;
    (color & 0x00FF_FFFF) | (u32::from(alpha) << 24)
}

unsafe fn rounded_rect_path(
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    radii: CornerRadii,
) -> *mut GpPath {
    let mut path: *mut GpPath = null_mut();
    // SAFETY: 输出指针指向栈上；所有 GDI+ 调用在同一线程。
    if unsafe { GdipCreatePath(FillModeAlternate, &mut path) }.0 != 0 || path.is_null() {
        return null_mut();
    }
    let limit = width.min(height) / 2.0;
    let tl = radii.tl.clamp(0.0, limit);
    let tr = radii.tr.clamp(0.0, limit);
    let br = radii.br.clamp(0.0, limit);
    let bl = radii.bl.clamp(0.0, limit);

    // SAFETY: `path` 由上面的调用创建；失败时仍可安全调用后续 Path API。
    unsafe {
        if tl > 0.01 {
            GdipAddPathArc(path, x, y, tl * 2.0, tl * 2.0, 180.0, 90.0);
        }
        GdipAddPathLine(path, x + tl, y, x + width - tr, y);
        if tr > 0.01 {
            GdipAddPathArc(
                path,
                x + width - tr * 2.0,
                y,
                tr * 2.0,
                tr * 2.0,
                270.0,
                90.0,
            );
        }
        GdipAddPathLine(path, x + width, y + tr, x + width, y + height - br);
        if br > 0.01 {
            GdipAddPathArc(
                path,
                x + width - br * 2.0,
                y + height - br * 2.0,
                br * 2.0,
                br * 2.0,
                0.0,
                90.0,
            );
        }
        GdipAddPathLine(path, x + width - br, y + height, x + bl, y + height);
        if bl > 0.01 {
            GdipAddPathArc(
                path,
                x,
                y + height - bl * 2.0,
                bl * 2.0,
                bl * 2.0,
                90.0,
                90.0,
            );
        }
        GdipAddPathLine(path, x, y + height - bl, x, y + tl);
        GdipClosePathFigure(path);
    }
    path
}

fn fill_path(graphics: *mut GpGraphics, color: u32, path: *mut GpPath) {
    let mut brush: *mut GpSolidFill = null_mut();
    // SAFETY: `graphics`/`path` 有效；刷子使用后立即删除。
    unsafe {
        if GdipCreateSolidFill(color, &mut brush).0 == 0 && !brush.is_null() {
            GdipFillPath(graphics, brush.cast::<GpBrush>(), path);
            GdipDeleteBrush(brush.cast::<GpBrush>());
        }
    }
}

fn stroke_path(graphics: *mut GpGraphics, color: u32, width: f32, path: *mut GpPath) {
    let mut pen: *mut GpPen = null_mut();
    // SAFETY: `graphics`/`path` 有效；画笔使用后立即删除。
    unsafe {
        if GdipCreatePen1(color, width, UnitPixel, &mut pen).0 == 0 && !pen.is_null() {
            GdipDrawPath(graphics, pen, path);
            GdipDeletePen(pen);
        }
    }
}

fn stroke_line(
    graphics: *mut GpGraphics,
    color: u32,
    width: f32,
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
) {
    let mut pen: *mut GpPen = null_mut();
    // SAFETY: `graphics` 有效；画笔使用后立即删除。
    unsafe {
        if GdipCreatePen1(color, width, UnitPixel, &mut pen).0 != 0 || pen.is_null() {
            return;
        }
        GdipSetPenLineCap197819(pen, LineCapRound, LineCapRound, DashCapFlat);
        GdipDrawLine(graphics, pen, x1, y1, x2, y2);
        GdipDeletePen(pen);
    }
}

fn stroke_arc(
    graphics: *mut GpGraphics,
    color: u32,
    width: f32,
    center: (f32, f32),
    radius: f32,
    start_deg: f32,
    sweep_deg: f32,
) {
    let mut pen: *mut GpPen = null_mut();
    // SAFETY: `graphics` 有效；画笔使用后立即删除。
    unsafe {
        if GdipCreatePen1(color, width, UnitPixel, &mut pen).0 != 0 || pen.is_null() {
            return;
        }
        GdipSetPenLineCap197819(pen, LineCapRound, LineCapRound, DashCapFlat);
        GdipDrawArc(
            graphics,
            pen,
            center.0 - radius,
            center.1 - radius,
            radius * 2.0,
            radius * 2.0,
            start_deg,
            sweep_deg,
        );
        GdipDeletePen(pen);
    }
}

fn draw_icon(
    graphics: *mut GpGraphics,
    color: u32,
    kind: IconKind,
    cx: f32,
    cy: f32,
    extent: f32,
    alpha: f32,
) {
    let color = with_alpha(color, alpha);
    let stroke = (extent * 0.085).max(1.4);
    match kind {
        IconKind::Default => {
            let side = extent * 0.92;
            // SAFETY: 路径在本函数内创建并释放。
            unsafe {
                let path = rounded_rect_path(
                    cx - side / 2.0,
                    cy - side / 2.0,
                    side,
                    side,
                    CornerRadii::capsule(side * 0.28),
                );
                if !path.is_null() {
                    stroke_path(graphics, color, stroke, path);
                    GdipDeletePath(path);
                }
            }
            stroke_line(
                graphics,
                color,
                stroke,
                cx,
                cy - side * 0.26,
                cx,
                cy + side * 0.26,
            );
            stroke_line(
                graphics,
                color,
                stroke,
                cx - side * 0.26,
                cy,
                cx + side * 0.26,
                cy,
            );
        }
        IconKind::Drop => {
            stroke_line(
                graphics,
                color,
                stroke,
                cx,
                cy - extent * 0.44,
                cx,
                cy + extent * 0.26,
            );
            stroke_line(
                graphics,
                color,
                stroke,
                cx - extent * 0.26,
                cy + extent * 0.02,
                cx,
                cy + extent * 0.28,
            );
            stroke_line(
                graphics,
                color,
                stroke,
                cx + extent * 0.26,
                cy + extent * 0.02,
                cx,
                cy + extent * 0.28,
            );
            stroke_line(
                graphics,
                color,
                stroke,
                cx - extent * 0.4,
                cy + extent * 0.46,
                cx + extent * 0.4,
                cy + extent * 0.46,
            );
        }
        IconKind::Check => {
            stroke_line(
                graphics,
                color,
                stroke,
                cx - extent * 0.42,
                cy,
                cx - extent * 0.1,
                cy + extent * 0.3,
            );
            stroke_line(
                graphics,
                color,
                stroke,
                cx - extent * 0.1,
                cy + extent * 0.3,
                cx + extent * 0.46,
                cy - extent * 0.3,
            );
        }
    }
}

fn draw_grip(
    graphics: *mut GpGraphics,
    color: u32,
    frame: &VisualFrame,
    window: PhysicalRect,
    metrics: BallMetrics,
) {
    let factor = metrics.factor();
    let visual = scaled_rect(frame.rect, frame.scale);
    let cx = (visual.x - window.x) as f32 + visual.width as f32 / 2.0;
    let cy = (visual.y - window.y) as f32 + visual.height as f32 / 2.0;
    let length = (visual.width.max(visual.height) as f32 * 0.39).max(6.0);
    let alpha = with_alpha(color, 0.42 * frame.grip_alpha);
    let stroke = (2.0 * factor).max(1.2);
    if visual.height >= visual.width {
        stroke_line(
            graphics,
            alpha,
            stroke,
            cx,
            cy - length / 2.0,
            cx,
            cy + length / 2.0,
        );
    } else {
        stroke_line(
            graphics,
            alpha,
            stroke,
            cx - length / 2.0,
            cy,
            cx + length / 2.0,
            cy,
        );
    }
}

fn draw_ring(
    graphics: *mut GpGraphics,
    palette: &Palette,
    payload: &PaintPayload,
    frame: &VisualFrame,
    window: PhysicalRect,
    metrics: BallMetrics,
) {
    let factor = metrics.factor();
    let visual = scaled_rect(frame.rect, frame.scale);
    let diameter = visual.width.min(visual.height) as f32;
    let cx = (visual.x - window.x) as f32 + visual.width as f32 / 2.0;
    let cy = (visual.y - window.y) as f32 + visual.height as f32 / 2.0;
    let radius = (diameter / 2.0 - 4.0 * factor).max(2.0);

    stroke_arc(
        graphics,
        palette.ring_track,
        2.0 * factor,
        (cx, cy),
        radius,
        0.0,
        359.9,
    );

    let color = if payload.failed {
        argb(255, 107, 114, 128)
    } else {
        palette.icon
    };
    let rotation = payload.ring_rotation.to_degrees();
    if let Some(progress) = payload.progress {
        stroke_arc(
            graphics,
            color,
            2.2 * factor,
            (cx, cy),
            radius,
            -90.0,
            (progress * 359.9).max(2.0),
        );
    } else if payload.indeterminate {
        stroke_arc(
            graphics,
            color,
            2.2 * factor,
            (cx, cy),
            radius,
            -90.0 + rotation,
            72.0,
        );
    }
}

fn draw_halo(
    graphics: *mut GpGraphics,
    color: u32,
    payload: &PaintPayload,
    frame: &VisualFrame,
    window: PhysicalRect,
    metrics: BallMetrics,
) {
    let factor = metrics.factor();
    let visual = scaled_rect(frame.rect, frame.scale);
    let diameter = visual.width.min(visual.height) as f32;
    let cx = (visual.x - window.x) as f32 + visual.width as f32 / 2.0;
    let cy = (visual.y - window.y) as f32 + visual.height as f32 / 2.0;
    let radius = diameter / 2.0 + 10.0 * factor;
    let alpha = frame.halo_alpha * payload.halo_breath;
    let color = with_alpha(color, alpha);
    let stroke = (1.5 * factor).max(1.0);
    let base = payload.halo_rotation.to_degrees();
    for index in 0..16 {
        stroke_arc(
            graphics,
            color,
            stroke,
            (cx, cy),
            radius,
            base + index as f32 * 22.5,
            11.0,
        );
    }
}

fn draw_tooltip(
    graphics: *mut GpGraphics,
    palette: &Palette,
    metrics: BallMetrics,
    window: PhysicalRect,
    frame: &VisualFrame,
    text: &str,
    alpha: f32,
) {
    if alpha <= 0.01 {
        return;
    }
    let visual = scaled_rect(frame.rect, frame.scale);
    let rect = tooltip_rect(text.chars().count(), metrics, visual, window);
    let x = (rect.x - window.x) as f32;
    let y = (rect.y - window.y) as f32;
    let width = rect.width as f32;
    let height = rect.height as f32;

    // SAFETY: 路径在本函数内创建并释放。
    unsafe {
        let path = rounded_rect_path(x, y, width, height, CornerRadii::capsule(height / 2.0));
        if !path.is_null() {
            fill_path(graphics, with_alpha(palette.tooltip_bg, alpha), path);
            GdipDeletePath(path);
        }
    }

    let family_name = wide("Microsoft YaHei UI");
    let text = wide(text);
    let mut family: *mut GpFontFamily = null_mut();
    // SAFETY: 名称/文案以 NUL 结尾且在调用期间存活；每个对象使用后立即释放。
    unsafe {
        if GdipCreateFontFamilyFromName(PCWSTR(family_name.as_ptr()), null_mut(), &mut family).0
            != 0
            || family.is_null()
        {
            return;
        }
        let mut font: *mut GpFont = null_mut();
        let em = (11.0 * metrics.factor()).max(9.0);
        if GdipCreateFont(family, em, 0, UnitPixel, &mut font).0 != 0 || font.is_null() {
            GdipDeleteFontFamily(family);
            return;
        }
        let mut format: *mut GpStringFormat = null_mut();
        if GdipCreateStringFormat(0, 0, &mut format).0 != 0 || format.is_null() {
            GdipDeleteFont(font);
            GdipDeleteFontFamily(family);
            return;
        }
        GdipSetStringFormatAlign(format, StringAlignmentCenter);
        GdipSetStringFormatLineAlign(format, StringAlignmentCenter);
        let layout = RectF {
            X: x,
            Y: y,
            Width: width,
            Height: height,
        };
        let mut brush: *mut GpSolidFill = null_mut();
        if GdipCreateSolidFill(with_alpha(palette.tooltip_text, alpha), &mut brush).0 == 0
            && !brush.is_null()
        {
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

/// 实际的绘制步骤；GDI+ 对象都在本函数内创建并释放。
fn paint_ball(
    bitmap: *mut GpBitmap,
    window: PhysicalRect,
    frame: &VisualFrame,
    payload: &PaintPayload,
    theme: Theme,
    metrics: BallMetrics,
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

    let palette = palette(theme, payload.accepted);
    let visual = scaled_rect(frame.rect, frame.scale);
    let x = (visual.x - window.x) as f32;
    let y = (visual.y - window.y) as f32;
    let w = visual.width as f32;
    let h = visual.height as f32;
    let factor = metrics.factor();
    let radii = CornerRadii {
        tl: frame.radii.tl * frame.scale,
        tr: frame.radii.tr * frame.scale,
        br: frame.radii.br * frame.scale,
        bl: frame.radii.bl * frame.scale,
    };

    // SAFETY: `graphics` 有效；每个 GDI+ 对象都在使用后立即释放。
    unsafe {
        // 清成全透明，否则 UpdateLayeredWindow 会把未初始化像素算进 alpha。
        GdipGraphicsClear(graphics, argb(0, 0, 0, 0));
        GdipSetSmoothingMode(graphics, SmoothingModeAntiAlias);
        GdipSetTextRenderingHint(graphics, TextRenderingHintAntiAlias);

        // 球体 / 胶囊条（不绘制投影）。
        let body = argb(
            (frame.opacity.clamp(0.0, 1.0) * 255.0) as u8,
            palette.body.0,
            palette.body.1,
            palette.body.2,
        );
        let path = rounded_rect_path(x, y, w, h, radii);
        if !path.is_null() {
            fill_path(graphics, body, path);
            stroke_path(graphics, palette.border, 1.0, path);
            // 顶部内高光：上半圆弧，贴近描边内侧。
            let inset = 1.5 * factor;
            GdipDeletePath(path);
            let cx = x + w / 2.0;
            let cy = y + h / 2.0;
            let radius = (w.min(h) / 2.0 - inset).max(1.0);
            if w > h * 1.2 || h > w * 1.2 {
                stroke_line(
                    graphics,
                    palette.highlight,
                    1.0,
                    x + radii.tl.max(2.0),
                    y + inset,
                    x + w - radii.tr.max(2.0),
                    y + inset,
                );
            } else {
                stroke_arc(
                    graphics,
                    palette.highlight,
                    1.0,
                    (cx, cy),
                    radius,
                    180.0,
                    180.0,
                );
            }
        }

        // 图标。
        if frame.icon_alpha > 0.01 {
            let cx = x + w / 2.0;
            let cy = y + h / 2.0;
            let extent = w.min(h) * 0.38 * frame.icon_scale;
            draw_icon(
                graphics,
                palette.icon,
                payload.icon,
                cx,
                cy,
                extent,
                frame.icon_alpha,
            );
        }

        // 收起态指示线。
        if frame.grip_alpha > 0.01 {
            draw_grip(graphics, palette.icon, frame, window, metrics);
        }

        // 导入进度环 / 对勾背景。
        if payload.progress.is_some() || payload.indeterminate {
            draw_ring(graphics, &palette, payload, frame, window, metrics);
        }

        // 拖入光环（离开时按 halo_alpha 淡出）。
        if frame.halo_alpha > 0.01 {
            draw_halo(graphics, palette.icon, payload, frame, window, metrics);
        }

        // 气泡。
        if let Some((text, _accepted)) = &payload.tooltip {
            draw_tooltip(
                graphics,
                &palette,
                metrics,
                window,
                frame,
                text,
                frame.tooltip_alpha,
            );
        }

        GdipDeleteGraphics(graphics);
    }
    Ok(())
}

/// ARGB（GDI+ `Color` 的 32 位表示）。
fn argb(alpha: u8, red: u8, green: u8, blue: u8) -> u32 {
    (u32::from(alpha) << 24) | (u32::from(red) << 16) | (u32::from(green) << 8) | u32::from(blue)
}

fn present_layered(
    hwnd: HWND,
    screen: HDC,
    memory: HDC,
    rect: PhysicalRect,
) -> Result<(), AppError> {
    let destination = POINT {
        x: rect.x,
        y: rect.y,
    };
    let source = POINT { x: 0, y: 0 };
    let extent = SIZE {
        cx: rect.width.max(1) as i32,
        cy: rect.height.max(1) as i32,
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

    /// 拖入/拖动缩放以当前视觉中心为锚，不会把球拉回启动位置。
    #[test]
    fn scaled_rect_keeps_the_current_center() {
        let original = PhysicalRect::new(500, 400, 48, 48);
        let scaled = scaled_rect(original, DROP_SCALE);

        assert_eq!(scaled.width, 55);
        assert_eq!(scaled.x + scaled.width as i32 / 2, 524);
        assert_eq!(scaled.y + scaled.height as i32 / 2, 424);
        assert!(scaled.x < original.x);

        let restored = scaled_rect(scaled, 1.0 / DROP_SCALE);
        assert_eq!(restored.width, 48);
        assert_eq!(restored.x + restored.width as i32 / 2, 524);
        assert_eq!(restored.y + restored.height as i32 / 2, 424);
    }
}
