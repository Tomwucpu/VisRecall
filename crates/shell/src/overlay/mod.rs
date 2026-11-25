//! 覆盖层：每台显示器一个 `WS_POPUP` 窗口 + D3D11/D2D 冻结帧渲染（design §3、§7）。
//!
//! 线程模型：D3D11 设备、交换链、D2D 上下文与冻结纹理只在覆盖层线程创建和使用；
//! 其他线程只通过 [`OverlayHandle`]（crossbeam sender）投递 [`OverlayCommand`]，
//! 覆盖层线程通过 [`OverlayEvent`] 回报结果。帧数据 [`RawFrame`] 是 CPU `Vec<u8>`，
//! 可以跨线程传递，上传成纹理后立即释放。
//!
//! [`spawn_overlay_thread`] 在**返回前**完成覆盖层线程的 `CoInitializeEx`、
//! [`D3dContext::new`] 与每台显示器的隐藏窗口 + 交换链预创建，因此 `show` 只剩
//! 「上传纹理 → 显示窗口 → 首帧 `Present`」，避免每次捕获重复 D3D 初始化（Task 10
//! 实测首帧 139ms，已接近 150ms 预算）。
//!
//! 交互（Task 13）：窗口过程只把鼠标/键盘消息翻译成 [`input::InputEvent`] 放进线程
//! 本地队列（见 [`input`] 的模块文档），覆盖层线程 pump 完消息后统一处理——
//! `WM_LBUTTONDOWN` 用 [`hit_test_handle`] 决定框选/移动/缩放，`WM_MOUSEMOVE` 用
//! `hit_chain_excluding(.., 当前进程)` 取外部窗口候选（覆盖层自身永远被排除），
//! `Enter` 提交、`Esc`/右键取消、方向键 1px（`Shift` 10px）微调、滚轮切换高亮窗口。
//! 提交时对每台显示器求选区交集，用 staging 纹理从冻结纹理回读并在 CPU 侧重拼成
//! 全局选区尺寸的 BGRA 图（[`input::compose_bgra`]）。
//!
//! 热路径（Task 13 review）：命中链按阶段节流（悬停 ≤40ms、拖拽/调整 ≤100ms 才重做
//! `EnumWindows`+DWM 查询，其余移动复用上一条候选链），重绘前做脏检查
//! （[`needs_redraw`]），只有 `phase/rect/hover_rect/active` 变化才提交新帧；按下/抬起、
//! 滚轮、按键与定时器仍然强制重绘。多屏时只有含鼠标指针的窗口 `Present(1,0)`，其余
//! `Present(0,0)`，避免串行等待多个 vblank。会话结束时 `OVERLAY_PERF` 输出重绘次数、
//! `draw_ms` 与 `input_to_present_ms` 的均值/最大值。
//!
//! 捕获与清理：`SetCapture` 只在「按下点落在某台显示器内」之后调用；所有关闭路径
//! （提交/`Esc`/右键/回读失败/选区过小/显示失败/线程退出）都走 [`OverlayThread::hide_all`]，
//! 统一释放鼠标捕获、`KillTimer`、恢复焦点；收到 `WM_CAPTURECHANGED`（捕获被抢走）时
//! 结束进行中的拖拽，避免状态卡死。
//!
//! 重绘：冻结帧由 D3D/D2D 直接呈现到交换链，因此 `WM_PAINT` 不能走 GDI 绘制。窗口
//! 过程在 `WM_PAINT`/`WM_DISPLAYCHANGE`/`WM_DPICHANGED` 时置位线程级原子
//! [`REDRAW_REQUESTED`]，覆盖层线程 pump 完消息后对全部活动窗口重新绘制完整场景
//! （冻结帧 + 压暗 + 选区 + 高亮 + 角标）并 `Present`。空闲时没有定时器；「已冻结」
//! 角标在显示后用 50ms `SetTimer` 驱动淡出，淡出结束后 `KillTimer`（取消/提交时同样
//! 立即停止），不常驻 60fps 空闲重绘。

pub mod d3d;
pub mod frame;
pub mod input;
pub mod render;

pub use d3d::{D3dContext, SwapChain};
pub use frame::{upload_frame, validate_frame_shape, FrozenTexture};
pub use input::compose_bgra;
pub use render::Renderer;

use std::ffi::c_void;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{bounded, unbounded, Receiver, RecvTimeoutError, SendTimeoutError, Sender};
use tracing::{debug, info, warn};
use visrecall_capture::display_for_point;
use visrecall_capture::enumerate_displays;
use visrecall_capture::hit_chain_excluding;
use visrecall_domain::capture::{CapturedImage, DisplayInfo, ForegroundInfo, RawFrame, WindowInfo};
use visrecall_domain::geometry::{PhysicalPoint, PhysicalRect};
use visrecall_domain::AppError;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    ERROR_CLASS_ALREADY_EXISTS, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, PAINTSTRUCT};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{
    AttachThreadInput, GetCurrentProcessId, GetCurrentThreadId,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetCapture, ReleaseCapture, SetCapture, SetFocus,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect,
    GetForegroundWindow, GetWindowThreadProcessId, IsWindow, KillTimer, LoadCursorW, PeekMessageW,
    RegisterClassW, SetForegroundWindow, SetTimer, SetWindowPos, ShowWindow, TranslateMessage,
    UnregisterClassW, CS_HREDRAW, CS_VREDRAW, HICON, IDC_ARROW, MSG, PM_REMOVE, SWP_NOACTIVATE,
    SWP_NOZORDER, SW_HIDE, SW_SHOW, WINDOW_EX_STYLE, WM_CAPTURECHANGED, WM_CLOSE, WM_DISPLAYCHANGE,
    WM_DPICHANGED, WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
    WM_MOUSEWHEEL, WM_PAINT, WM_QUIT, WM_RBUTTONDOWN, WM_TIMER, WNDCLASSW, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP,
};

use crate::selection::{
    clamp_to_virtual_screen, hit_test_handle, Handle, Modifiers, SelectionPhase, SelectionState,
    MIN_SELECTION_SIZE,
};
use input::InputEvent;

/// 覆盖层窗口类名（design §7.1）；示例/测试通过 `FindWindowW` 定位覆盖窗口。
pub const OVERLAY_CLASS_NAME: &str = "VisRecallOverlay";
const OVERLAY_WINDOW_TITLE: &str = "VisRecall Overlay";
/// 激活态等待命令的超时：既要及时 pump 窗口消息，又不在空闲时忙等。
const ACTIVE_POLL_INTERVAL: Duration = Duration::from_millis(8);
/// 发送「可丢弃」事件（[`OverlayEvent::Cancelled`]）的最长阻塞时间：
/// pump 循环不能被一个没有读者的事件通道卡死。
const EVENT_SEND_TIMEOUT: Duration = Duration::from_millis(100);
/// 悬停态下鼠标命中链的最短刷新间隔。
///
/// `hit_chain_excluding` 是 `EnumWindows` + 每个候选一次 DWM 边界查询，放在每次
/// `WM_MOUSEMOVE` 上会明显拖慢拖拽手感；悬停态每 40ms 刷新一次足够跟上窗口切换。
const HOVER_HIT_SCAN_INTERVAL: Duration = Duration::from_millis(40);
/// 拖拽/调整态下命中链的最短刷新间隔：候选链只用于吸附，100ms 内复用手感无差异。
const DRAG_HIT_SCAN_INTERVAL: Duration = Duration::from_millis(100);

/// 「已冻结」角标完全可见的时长。
const BADGE_VISIBLE: Duration = Duration::from_secs(3);
/// 角标淡出时长（在 `BADGE_VISIBLE` 之后）。
const BADGE_FADE: Duration = Duration::from_millis(300);
/// 角标淡出的重绘节拍：约 20fps，只在角标存活期间运行。
const BADGE_TIMER_INTERVAL_MS: u32 = 50;
/// 角标计时器 id。
const BADGE_TIMER_ID: usize = 1;
/// 「选区过小」提示停留时长。
const NOTICE_DURATION: Duration = Duration::from_millis(1000);
/// 选区无效时的提示文字。
const TOO_SMALL_NOTICE: &str = "选区过小";
/// 悬停标签里标题/类名/进程名的最大字符数（超出部分截断，避免撑破标签）。
const LABEL_TITLE_CHARS: usize = 60;
const LABEL_FIELD_CHARS: usize = 32;

/// 窗口过程 → 覆盖层线程的「需要重绘」标志（线程级，无锁）。
///
/// 由 `WM_PAINT`/`WM_DISPLAYCHANGE`/`WM_DPICHANGED`/`WM_TIMER`（角标淡出）置位；
/// 覆盖层线程在 pump 完消息后用 `swap(false)` 消费一次并对全部活动窗口重新绘制完整
/// 场景。窗口过程里不做 D3D 呈现，避免渲染调用发生在消息分发过程中间。
static REDRAW_REQUESTED: AtomicBool = AtomicBool::new(false);

/// 一次重绘依赖的选区状态快照（不含时间相关的角标淡出）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RedrawState {
    phase: SelectionPhase,
    rect: PhysicalRect,
    hover_rect: PhysicalRect,
    active: Option<Handle>,
}

/// 脏检查：`before` 与 `after` 完全一致时这一帧不需要重绘。
///
/// 鼠标在同一个窗口上连续移动时 `phase/rect/hover_rect/active` 都不变，跳过
/// `draw_scene` + `Present` 是热路径上最省的一步（每帧 Present 会等一个 vblank）。
/// 按下/抬起、滚轮、按键与定时器不经过这里，始终强制重绘。
fn needs_redraw(before: RedrawState, after: RedrawState) -> bool {
    before != after
}

/// 覆盖层会话的可观测统计；会话结束时以 `OVERLAY_PERF` 输出（design §7.2 门禁参考）。
#[derive(Clone, Copy, Debug, Default)]
struct PerfStats {
    /// 实际提交的帧数（一次 `redraw_active_windows` 计一次）。
    redraws: u64,
    /// 所有窗口 `draw_scene` 耗时之和（不含 `Present`）。
    draw_total: Duration,
    draw_max: Duration,
    /// `hit_chain_excluding`（`EnumWindows`+DWM）实际执行次数，用于验证节流生效。
    hit_scans: u64,
    /// `WM_MOUSEMOVE` 入队 → 该次 Present 完成的采样数。
    input_samples: u64,
    input_total: Duration,
    input_max: Duration,
}

impl PerfStats {
    fn record_redraw(&mut self, draw_time: Duration) {
        self.redraws += 1;
        self.draw_total += draw_time;
        self.draw_max = self.draw_max.max(draw_time);
    }

    fn record_hit_scan(&mut self) {
        self.hit_scans += 1;
    }

    fn record_input(&mut self, latency: Duration) {
        self.input_samples += 1;
        self.input_total += latency;
        self.input_max = self.input_max.max(latency);
    }

    /// 输出本会话的统计（`draw_ms` 为一次重绘内全部窗口 `draw_scene` 之和）。
    fn log(&self) {
        info!(
            redraws = self.redraws,
            draw_avg_ms = avg_ms(self.draw_total, self.redraws),
            draw_max_ms = ms(self.draw_max),
            hit_chain_scans = self.hit_scans,
            input_to_present_samples = self.input_samples,
            input_to_present_avg_ms = avg_ms(self.input_total, self.input_samples),
            input_to_present_max_ms = ms(self.input_max),
            "OVERLAY_PERF"
        );
    }
}

/// 时长转毫秒。
fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

/// 平均毫秒数；样本为 0 时返回 0。
fn avg_ms(total: Duration, samples: u64) -> f64 {
    if samples == 0 {
        0.0
    } else {
        ms(total) / samples as f64
    }
}

/// app-core → 覆盖层线程的指令。
#[derive(Debug)]
pub enum OverlayCommand {
    /// 显示冻结帧；`frames` 与 `displays` 按显示器一一对应（抓帧顺序）。
    Show {
        frames: Vec<RawFrame>,
        displays: Vec<DisplayInfo>,
        foreground: ForegroundInfo,
    },
    /// 隐藏覆盖层并恢复焦点。
    Cancel,
    /// 隐藏覆盖层后通过独立通道确认，避免和用户取消事件混淆。
    CancelAndConfirm(Sender<()>),
}

/// 覆盖层线程 → app-core 的事件。
#[derive(Debug)]
pub enum OverlayEvent {
    /// 提交选区后的回读结果：`image` 是选区尺寸的 top-down BGRA8，
    /// `global_rect` 是选区在虚拟屏幕上的物理像素位置。
    ///
    /// 覆盖层线程用**阻塞 send** 投递（[`OverlayThread::send_event_blocking`]），
    /// 不走 100ms 超时丢弃路径；日志只记录尺寸与矩形，不打印像素数据。
    Committed {
        image: CapturedImage,
        global_rect: PhysicalRect,
        foreground: ForegroundInfo,
    },
    /// 用户取消或覆盖层显示失败。
    Cancelled,
}

/// 覆盖层线程句柄；只持有 crossbeam sender（`Send + Sync`）。
#[derive(Clone, Debug)]
pub struct OverlayHandle {
    tx: Sender<OverlayCommand>,
}

impl OverlayHandle {
    /// 投递冻结帧；返回发送结果（覆盖层线程已退出时返回 [`AppError::Internal`]）。
    pub fn show(
        &self,
        frames: Vec<RawFrame>,
        displays: Vec<DisplayInfo>,
        foreground: ForegroundInfo,
    ) -> Result<(), AppError> {
        self.send(OverlayCommand::Show {
            frames,
            displays,
            foreground,
        })
    }

    /// 请求取消：隐藏所有覆盖窗口并把焦点还给捕获前的前台窗口。
    ///
    /// 无论当前是否显示中，覆盖层线程都会回一次 [`OverlayEvent::Cancelled`]
    /// （重复 `Cancelled` 幂等），因此 `cancel()` 之后等待事件的调用方不会永久挂起。
    pub fn cancel(&self) -> Result<(), AppError> {
        self.send(OverlayCommand::Cancel)
    }

    /// 等待覆盖层线程清理当前会话。捕获循环随后丢弃已排队的旧会话事件。
    pub fn cancel_and_wait(&self) -> Result<(), AppError> {
        let (done, confirmed) = bounded(1);
        self.send(OverlayCommand::CancelAndConfirm(done))?;
        confirmed
            .recv()
            .map_err(|_| AppError::Internal("overlay thread stopped before cancelling".into()))
    }

    fn send(&self, command: OverlayCommand) -> Result<(), AppError> {
        self.tx
            .send(command)
            .map_err(|_| AppError::Internal("overlay thread is not running".into()))
    }
}

/// 启动覆盖层线程，并同步等待其初始化完成。
///
/// 初始化在覆盖层线程内完成：`CoInitializeEx(STA)` + [`D3dContext::new`] + 按当前
/// 显示器预创建隐藏窗口与交换链。任一显示器初始化失败会返回错误（已创建的窗口/设备
/// 由线程退出路径清理）。调用方需保证进程已声明 Per-Monitor V2 DPI 感知
/// （`visrecall_capture::set_per_monitor_v2`，必须在创建任何窗口前调用）。
///
/// 事件通道关闭后覆盖层线程自行退出；`OverlayHandle` 全部 drop 后命令通道断开，
/// 线程隐藏窗口、恢复焦点并释放 D3D 资源。
pub fn spawn_overlay_thread(events: Sender<OverlayEvent>) -> Result<OverlayHandle, AppError> {
    let (command_tx, command_rx) = unbounded();
    let (ready_tx, ready_rx) = bounded::<Result<(), AppError>>(1);

    thread::Builder::new()
        .name("visrecall-overlay".into())
        .spawn(move || overlay_thread_main(command_rx, events, ready_tx))
        .map_err(|err| AppError::Internal(format!("spawn overlay thread: {err}")))?;

    match ready_rx.recv() {
        Ok(Ok(())) => Ok(OverlayHandle { tx: command_tx }),
        Ok(Err(err)) => Err(err),
        // 线程在回报初始化结果前就消失了，只能是内部错误。
        Err(_) => Err(AppError::Internal(
            "overlay thread exited before initialization".into(),
        )),
    }
}

fn overlay_thread_main(
    commands: Receiver<OverlayCommand>,
    events: Sender<OverlayEvent>,
    ready: Sender<Result<(), AppError>>,
) {
    let mut overlay = match OverlayThread::start(events) {
        Ok(overlay) => overlay,
        Err(err) => {
            let _ = ready.send(Err(err));
            return;
        }
    };
    if ready.send(Ok(())).is_err() {
        debug!("overlay caller vanished before initialization completed");
        return;
    }
    overlay.run(&commands);
}

/// 一台显示器对应的隐藏覆盖窗口、交换链与当前冻结帧。
struct OverlayWindow {
    display_id: String,
    hwnd: HWND,
    renderer: Renderer,
    frozen: Option<FrozenTexture>,
    /// 所在显示器边界（虚拟屏幕物理像素）；用于全局坐标 → 客户区坐标换算。
    bounds: PhysicalRect,
    /// 所在显示器 DPI（控制点尺寸 `8 × dpi / 96`）。
    dpi: u32,
}

/// 「选区过小」一类提示：显示到 `until` 后取消会话。
struct Notice {
    text: &'static str,
    until: Instant,
}

/// 「已冻结」角标：`hwnd` 上挂着 50ms 重绘计时器，到 `until` 后停止。
struct Badge {
    hwnd: HWND,
    until: Instant,
}

/// 一次重绘共享的全局坐标场景（每个窗口再换算到自己的客户区）。
struct SceneView {
    selection: Option<PhysicalRect>,
    selection_label: Option<String>,
    /// 尺寸标签的唯一归属窗口（跨屏时只画一次）。
    selection_label_owner: Option<usize>,
    hover: Option<PhysicalRect>,
    hover_label: Option<String>,
    /// 悬停标签的唯一归属窗口。
    hover_label_owner: Option<usize>,
    notice: Option<String>,
    badge_alpha: f32,
    /// 所有显示器的 bounds；控制点贴显示器临空边（含虚拟屏外沿）时判归属用。
    display_bounds: Vec<PhysicalRect>,
}

/// 覆盖层线程独占的状态；D3D/D2D 资源只在这里创建和使用。
struct OverlayThread {
    hinstance: HINSTANCE,
    /// 本线程是否注册了窗口类（决定 Drop 时是否注销）。
    class_registered: bool,
    /// 声明在 `device` 之前：Rust 按声明顺序 drop 字段，窗口持有的
    /// Renderer/SwapChain（及冻结纹理）必须先于设备释放。
    windows: Vec<OverlayWindow>,
    device: D3dContext,
    events: Sender<OverlayEvent>,
    /// 本次捕获前的前台窗口，取消时用于恢复焦点。
    foreground: Option<ForegroundInfo>,
    /// 本次捕获的显示器列表（提交裁剪、DPI 与命中测试）。
    displays: Vec<DisplayInfo>,
    /// 所有覆盖窗口共享的选区状态机。
    selection: SelectionState,
    /// 指针下的外部窗口链（Z 序根→子）：吸附候选、滚轮切换与悬停标签。
    hover_chain: Vec<WindowInfo>,
    /// 上次刷新 [`OverlayThread::hover_chain`] 的时刻；为 `None` 时下次移动必刷新。
    last_hit_scan: Option<Instant>,
    /// 待显示的提示；到期后取消会话。
    notice: Option<Notice>,
    /// 「已冻结」角标的显示/淡出状态；`None` 表示角标已结束或会话未激活。
    badge: Option<Badge>,
    /// 本次会话的重绘/延迟统计，会话结束时以 `OVERLAY_PERF` 输出。
    perf: PerfStats,
    /// 是否有可见的覆盖层；决定命令循环的等待策略。
    active: bool,
    /// 必须最后 drop：清理窗口/纹理之后再 `CoUninitialize`。
    _com: ComGuard,
}

impl OverlayThread {
    fn start(events: Sender<OverlayEvent>) -> Result<OverlayThread, AppError> {
        let com = ComGuard::init()?;

        // SAFETY: 取本进程 HINSTANCE；`None` 表示当前可执行文件。
        let module = unsafe { GetModuleHandleW(None) }
            .map_err(|err| AppError::Internal(format!("GetModuleHandleW failed: {err}")))?;
        let hinstance = HINSTANCE(module.0);

        // 先建设备、再注册窗口类：`D3dContext::new()` 失败时不会留下已注册的类
        // （反之则需要显式 `UnregisterClassW` 才能不泄漏）。
        let device = D3dContext::new()?;
        let class_registered = register_overlay_class(hinstance)?;

        let mut overlay = OverlayThread {
            hinstance,
            class_registered,
            windows: Vec::new(),
            device,
            events,
            foreground: None,
            displays: Vec::new(),
            selection: SelectionState::new(),
            hover_chain: Vec::new(),
            last_hit_scan: None,
            notice: None,
            badge: None,
            perf: PerfStats::default(),
            active: false,
            _com: com,
        };

        // 预创建：把 D3D 初始化、窗口与交换链创建挪到 show 之前完成。
        // 任一步失败都会让这个局部变量提前 drop，`Drop` 负责注销窗口类并销毁已建窗口。
        for display in &enumerate_displays()? {
            let display_id = display.id.as_str();
            let bounds = display.bounds;
            if bounds.is_empty() {
                warn!(display = %display_id, "skipping display with empty bounds");
                continue;
            }
            let window =
                create_overlay_window(&overlay.device, hinstance, display_id, bounds, display.dpi)?;
            overlay.windows.push(window);
        }
        if overlay.windows.is_empty() {
            return Err(AppError::CaptureFailed(
                "no displays available for overlay windows".into(),
            ));
        }

        info!(
            windows = overlay.windows.len(),
            driver = overlay.device.driver_name(),
            adapter = overlay.device.adapter_name(),
            "overlay thread initialized"
        );
        Ok(overlay)
    }

    fn run(&mut self, commands: &Receiver<OverlayCommand>) {
        loop {
            if self.active {
                // 激活时既有窗口消息（输入、WM_PAINT 重绘、WM_QUIT）又有命令要处理：
                // 先 pump 并处理本轮输入，再短超时收命令。
                if self.pump_messages() {
                    info!("overlay thread quitting: WM_QUIT received");
                    break;
                }
                self.process_input();
                self.tick();
            }

            let command = if self.active {
                match commands.recv_timeout(ACTIVE_POLL_INTERVAL) {
                    Ok(command) => command,
                    Err(RecvTimeoutError::Timeout) => continue,
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            } else {
                // 非激活时阻塞等待，空闲零 CPU。
                match commands.recv() {
                    Ok(command) => command,
                    Err(_) => break,
                }
            };
            self.handle_command(command);
        }
        debug!("overlay command loop exited; overlay thread stopping");
    }

    fn handle_command(&mut self, command: OverlayCommand) {
        match command {
            OverlayCommand::Show {
                frames,
                displays,
                foreground,
            } => {
                if let Err(err) = self.show(frames, displays, foreground) {
                    warn!(error = %err, "frozen overlay show failed; cancelling");
                    self.hide_all();
                    self.send_event(OverlayEvent::Cancelled);
                }
            }
            OverlayCommand::Cancel => {
                self.cancel();
                self.send_event(OverlayEvent::Cancelled);
            }
            OverlayCommand::CancelAndConfirm(done) => {
                self.cancel();
                let _ = done.send(());
            }
        }
    }

    /// 上传冻结帧、显示窗口并完成首帧 `Present`。
    fn show(
        &mut self,
        frames: Vec<RawFrame>,
        displays: Vec<DisplayInfo>,
        foreground: ForegroundInfo,
    ) -> Result<(), AppError> {
        let received = Instant::now();

        // 已有活动覆盖层时先收掉旧的：旧会话必须先收到 `Cancelled`，再显示新帧，
        // 否则等待旧会话事件的调用方会一直等不到结果。
        if self.active {
            debug!("replacing active overlay before showing new frozen frames");
            self.hide_all();
            self.send_event(OverlayEvent::Cancelled);
        }
        self.foreground = Some(foreground);
        // 新会话：重置选区/候选/提示，角标从头计时；丢弃上个会话残留的输入事件
        // （否则一条陈旧的 `Commit` 会在新会话里立即触发「选区过小」）。
        self.selection = SelectionState::new();
        self.hover_chain.clear();
        self.last_hit_scan = None;
        self.notice = None;
        self.perf = PerfStats::default();
        self.displays = displays.clone();
        let _ = input::take();
        input::clear_mouse_move_at();
        for window in &mut self.windows {
            window.frozen = None;
        }

        let frame_count = frames.len();
        let primary_id = displays
            .iter()
            .find(|display| display.primary)
            .map(|display| display.id.clone());

        for frame in frames {
            let display = displays
                .iter()
                .find(|display| display.id == frame.display_id)
                .ok_or_else(|| {
                    AppError::InvalidInput(format!(
                        "no display info for frame {}",
                        frame.display_id
                    ))
                })?;
            let bounds = display.bounds;
            let dpi = display.dpi;
            let index =
                self.ensure_window(&frame.display_id, bounds, dpi, frame.width, frame.height)?;
            info!(
                display = %frame.display_id,
                window_width = frame.width,
                window_height = frame.height,
                "overlay window ready for frozen frame"
            );
            let frozen = upload_frame(&self.device, &frame)?;
            info!(
                display = %frame.display_id,
                texture_width = frozen.width,
                texture_height = frozen.height,
                frame_bytes = frame.bgra.len(),
                "frozen frame uploaded"
            );
            self.windows[index].frozen = Some(frozen);
            // `frames: Vec<RawFrame>` 已经让所有帧整体驻留（峰值内存由命令本身决定）；
            // 这里逐帧 `drop` 只是尽早上传完成后立刻归还该帧的 CPU 位图，不让它在
            // 循环剩余期间继续占内存。
            drop(frame);
        }

        // 先显示窗口（覆盖层需要键盘焦点），再逐窗绘制 + 首帧 Present。
        for window in &self.windows {
            if window.frozen.is_some() {
                // SAFETY: hwnd 由本线程创建，尚未销毁；SW_SHOW 只显示窗口。
                let _ = unsafe { ShowWindow(window.hwnd, SW_SHOW) };
            }
        }

        let focus = self.focus_target(primary_id.as_deref());
        let bounds: Vec<PhysicalRect> = self.windows.iter().map(|window| window.bounds).collect();
        let view = SceneView {
            selection: None,
            selection_label: None,
            selection_label_owner: None,
            hover: None,
            hover_label: None,
            hover_label_owner: None,
            notice: None,
            // 首帧的角标完全不透明；淡出由 `BADGE_*` 计时器驱动。
            badge_alpha: 1.0,
            display_bounds: bounds,
        };

        let mut first_present_ms = None;
        // 与重绘一致：只有含鼠标指针的窗口等 vblank，其余窗口 `Present(0,0)`。
        let cursor = input::cursor_point();
        for index in 0..self.windows.len() {
            if self.windows[index].frozen.is_none() {
                continue;
            }
            let scene = self.build_scene(index, &view);
            let window = &mut self.windows[index];
            let Some(frozen) = window.frozen.as_ref() else {
                continue;
            };
            debug!(display = %window.display_id, "drawing frozen frame");
            window.renderer.draw_scene(frozen, &scene)?;
            debug!(display = %window.display_id, "presenting frozen frame");
            let vsync = cursor.is_some_and(|point| window.bounds.contains(point));
            window.renderer.present_vsync(vsync)?;
            if first_present_ms.is_none() {
                first_present_ms = Some(received.elapsed().as_secs_f64() * 1000.0);
            }

            let mut client = RECT::default();
            // SAFETY: hwnd 有效，`client` 是栈上有效指针。
            let _ = unsafe { GetClientRect(window.hwnd, &mut client) };
            info!(
                display = %window.display_id,
                window_width = client.right - client.left,
                window_height = client.bottom - client.top,
                texture_width = frozen.width,
                texture_height = frozen.height,
                "frozen frame presented"
            );
        }

        self.active = true;
        match focus {
            Some(hwnd) => {
                focus_window(hwnd);
                self.start_badge_timer(hwnd);
            }
            None => warn!("no overlay window has a frozen frame; focus unchanged"),
        }

        info!(
            frames = frame_count,
            windows = self.windows.iter().filter(|w| w.frozen.is_some()).count(),
            show_to_first_present_ms = first_present_ms.unwrap_or(-1.0),
            show_total_ms = received.elapsed().as_secs_f64() * 1000.0,
            "frozen overlay shown"
        );
        Ok(())
    }

    /// 取消：隐藏窗口、释放冻结纹理并把焦点还给捕获前的前台窗口。
    fn cancel(&mut self) {
        if self.active {
            self.selection.cancel();
            self.hover_chain.clear();
            self.notice = None;
            self.hide_all();
            info!("frozen overlay cancelled");
        }
    }

    /// 用户取消（`Esc`/右键）：结束会话并回报一次 [`OverlayEvent::Cancelled`]。
    fn cancel_session(&mut self) {
        self.selection.cancel();
        self.hover_chain.clear();
        self.notice = None;
        let was_active = self.active;
        self.hide_all();
        if was_active {
            info!("overlay session cancelled by user");
        }
        self.send_event(OverlayEvent::Cancelled);
    }

    /// 隐藏全部覆盖窗口并释放冻结纹理；活动时先恢复焦点再隐藏。
    ///
    /// 这是**唯一的会话结束清理路径**：提交、`Esc`/右键取消、回读失败、选区过小、
    /// 显示失败与线程退出都经过这里，因此鼠标捕获释放（[`OverlayThread::release_capture`]）、
    /// 角标计时器 `KillTimer`（[`OverlayThread::stop_badge_timer`]）与焦点恢复
    /// 都不会被某条关闭路径漏掉。
    ///
    /// 顺序很重要：覆盖层仍是前台进程时 `SetForegroundWindow(目标应用)` 更容易被前台锁
    /// 接受；若先 `SW_HIDE`，前台窗口会变成空/其他进程窗口，只能退化为
    /// `AttachThreadInput` 兜底（更容易失败，日志里也会多一条 warn）。
    fn hide_all(&mut self) {
        // 先放掉鼠标捕获：隐藏窗口后若仍持有捕获，键盘/鼠标会卡在覆盖层线程上。
        self.release_capture();
        // 会话结束时立刻停掉角标计时器，避免隐藏后继续 20fps 重绘。
        self.stop_badge_timer();
        if self.active {
            // 会话结束：输出本会话的 `OVERLAY_PERF`，并把统计清零留给下一次。
            self.perf.log();
            if let Some(foreground) = self.foreground.as_ref() {
                restore_foreground(foreground);
            }
        }
        for window in &mut self.windows {
            window.frozen = None;
            // SAFETY: hwnd 由本线程创建，尚未销毁；SW_HIDE 不销毁窗口。
            let _ = unsafe { ShowWindow(window.hwnd, SW_HIDE) };
        }
        // 丢弃陈旧的 WM_PAINT 请求，避免隐藏期间标志残留导致下次显示多一帧重绘。
        REDRAW_REQUESTED.store(false, Ordering::Release);
        self.active = false;
        self.notice = None;
        self.last_hit_scan = None;
        self.perf = PerfStats::default();
    }

    /// 释放本线程持有的鼠标捕获（幂等，未持有时什么也不做）。
    ///
    /// `GetCapture` 只返回**本线程**的捕获窗口，因此不会误放其他线程/进程的捕获。
    fn release_capture(&self) {
        // SAFETY: 两个调用都只影响本线程的捕获状态，无指针参数。
        unsafe {
            let capture = GetCapture();
            if capture.0.is_null() || !self.windows.iter().any(|window| window.hwnd == capture) {
                return;
            }
            if ReleaseCapture().is_ok() {
                info!("released mouse capture at session end");
            } else {
                debug!("ReleaseCapture failed; capture already released");
            }
        }
    }

    /// 启动「已冻结」角标的 50ms 重绘计时器，并记录淡出结束时刻。
    fn start_badge_timer(&mut self, hwnd: HWND) {
        // SAFETY: hwnd 由本线程创建；`TIMERPROC` 传 `None` 表示 WM_TIMER 进本线程队列。
        let timer = unsafe { SetTimer(Some(hwnd), BADGE_TIMER_ID, BADGE_TIMER_INTERVAL_MS, None) };
        if timer == 0 {
            warn!("SetTimer failed; frozen badge will not animate");
        }
        self.badge = Some(Badge {
            hwnd,
            until: Instant::now() + BADGE_VISIBLE + BADGE_FADE,
        });
    }

    /// 停掉角标计时器（幂等）。
    fn stop_badge_timer(&mut self) {
        if let Some(badge) = self.badge.take() {
            // SAFETY: hwnd 由本线程创建；计时器由本线程在同一窗口上设置。
            let _ = unsafe { KillTimer(Some(badge.hwnd), BADGE_TIMER_ID) };
        }
    }

    /// 角标当前不透明度：前 [`BADGE_VISIBLE`] 完全可见，随后 [`BADGE_FADE`] 内淡出到 0。
    fn badge_alpha(&self, now: Instant) -> f32 {
        let Some(badge) = self.badge.as_ref() else {
            return 0.0;
        };
        let fade_start = badge.until - BADGE_FADE;
        if now <= fade_start {
            return 1.0;
        }
        let remaining = badge.until.saturating_duration_since(now).as_secs_f32();
        (remaining / BADGE_FADE.as_secs_f32()).clamp(0.0, 1.0)
    }

    /// 发送覆盖层事件；通道阻塞超过 [`EVENT_SEND_TIMEOUT`] 就丢弃并 warn。
    ///
    /// 绝不阻塞 pump：事件通道的读者（app-core）可能正忙或已经退出，覆盖层线程不能
    /// 因此卡在 `send` 上，连窗口消息都处理不了。Task 14 会使用无界通道，正常情况下
    /// 不会触发超时分支。携带图像的 [`OverlayEvent::Committed`] 必须走
    /// [`OverlayThread::send_event_blocking`]，不能被超时丢弃。
    ///
    /// 丢弃分支只记录事件种类与尺寸/矩形：`CapturedImage.bgra` 可能有几十 MB，
    /// 绝不能 `Debug` 打印。
    fn send_event(&self, event: OverlayEvent) {
        match self.events.send_timeout(event, EVENT_SEND_TIMEOUT) {
            Ok(()) => {}
            Err(SendTimeoutError::Timeout(event)) => {
                log_dropped_event("overlay event channel blocked; event dropped", &event)
            }
            Err(SendTimeoutError::Disconnected(event)) => {
                log_dropped_event("overlay event channel closed; event dropped", &event)
            }
        }
    }

    /// 阻塞发送事件，确保不丢（通道断开时立即返回错误，不会永久阻塞）。
    ///
    /// 只用于 [`OverlayEvent::Committed`]：它携带整张选区图像，丢弃等于用户白白框选。
    /// 日志只记录尺寸这类元信息，**不打印** `bgra`。
    fn send_event_blocking(&self, event: OverlayEvent) {
        match &event {
            OverlayEvent::Committed {
                image, global_rect, ..
            } => debug!(
                width = image.width,
                height = image.height,
                global_rect = ?global_rect,
                "sending committed overlay event"
            ),
            OverlayEvent::Cancelled => {}
        }
        if self.events.send(event).is_err() {
            warn!("overlay event channel closed; committed result dropped");
        }
    }

    /// 对所有活动窗口重新绘制并呈现完整场景（输入、`WM_PAINT`、角标计时器触发）。
    ///
    /// 只在有冻结纹理的窗口上工作；单个窗口失败只 warn，不影响其他显示器。
    /// 多屏时只有包含鼠标指针的窗口 `Present(1,0)`（等 vblank），其余窗口
    /// `Present(0,0)` 立即入队，避免一次重绘串行等待多个刷新周期（design §7.2）。
    fn redraw_active_windows(&mut self) {
        if !self.active {
            return;
        }
        let now = Instant::now();
        let selection = (!self.selection.rect.is_empty()).then_some(self.selection.rect);
        // 拖拽/调整期间不画悬停高亮：此刻它是上一帧的候选，会和正在变化的选区抢视觉焦点。
        let dragging = matches!(
            self.selection.phase,
            SelectionPhase::Dragging | SelectionPhase::Adjusting
        );
        let hover = if dragging {
            None
        } else {
            self.selection.hovered_rect()
        };
        let bounds: Vec<PhysicalRect> = self.windows.iter().map(|window| window.bounds).collect();
        let view = SceneView {
            selection,
            selection_label: selection.map(|rect| format!("{}×{}", rect.width, rect.height)),
            // 跨屏时控制点与标签只归属一台显示器，避免每块屏各画一套。
            selection_label_owner: selection
                .and_then(|rect| label_owner_bottom_right(rect, &bounds)),
            hover,
            hover_label: hover.map(|rect| self.hover_label(rect)),
            hover_label_owner: hover.and_then(|rect| label_owner_top_left(rect, &bounds)),
            notice: self.notice.as_ref().map(|notice| notice.text.to_string()),
            badge_alpha: self.badge_alpha(now),
            display_bounds: bounds,
        };

        let cursor = input::cursor_point();
        let mut draw_time = Duration::ZERO;
        let mut redrawn = 0usize;
        for index in 0..self.windows.len() {
            if self.windows[index].frozen.is_none() {
                continue;
            }
            let scene = self.build_scene(index, &view);
            let window = &mut self.windows[index];
            let Some(frozen) = window.frozen.as_ref() else {
                continue;
            };
            let draw_start = Instant::now();
            if let Err(err) = window.renderer.draw_scene(frozen, &scene) {
                warn!(display = %window.display_id, error = %err, "redrawing overlay scene failed");
                continue;
            }
            draw_time += draw_start.elapsed();
            let vsync = cursor.is_some_and(|point| window.bounds.contains(point));
            if let Err(err) = window.renderer.present_vsync(vsync) {
                warn!(display = %window.display_id, error = %err, "presenting redrawn overlay scene failed");
                continue;
            }
            redrawn += 1;
        }
        if redrawn == 0 {
            debug!("no active window to redraw");
            return;
        }
        self.perf.record_redraw(draw_time);
    }

    /// 把全局坐标的场景换算成某个窗口的客户区场景。
    ///
    /// 与窗口不相交的元素置 `None`（该窗口本来也画不出来）；`dpi` 用于控制点尺寸。
    /// 选区矩形**不**与窗口求交：越界部分交给 D2D 裁剪，这样跨屏选区不会在屏间缝隙
    /// 多画一条边框。控制点与标签只在该元素的归属窗口上出现（跨屏不重复绘制）；
    /// 控制点归属需要全部显示器 bounds（[`SceneView::display_bounds`]）来判断「贴显示器
    /// 临空边（含虚拟屏外沿）的控制点该归谁」。
    fn build_scene(&self, index: usize, view: &SceneView) -> render::Scene {
        let window = &self.windows[index];
        let origin = window.bounds;
        let (width, height) = window.renderer.size();
        let window_rect = PhysicalRect::new(0, 0, width, height);
        let local = |rect: PhysicalRect| -> Option<PhysicalRect> {
            let shifted = PhysicalRect::new(
                rect.x.saturating_sub(origin.x),
                rect.y.saturating_sub(origin.y),
                rect.width,
                rect.height,
            );
            // 只用来判断可见性；真正绘制的是未裁剪的本地矩形（由 D2D 裁剪）。
            shifted.intersect(&window_rect).map(|_| shifted)
        };

        let selection = view.selection.and_then(local);
        let hover = view.hover.and_then(local);
        let handles = view
            .selection
            .map(|rect| render::handles_for_window(rect, origin, &view.display_bounds))
            .unwrap_or_default();
        render::Scene {
            selection,
            selection_label: if view.selection_label_owner == Some(index) {
                view.selection_label.clone()
            } else {
                None
            },
            handles,
            hover,
            hover_label: if view.hover_label_owner == Some(index) {
                view.hover_label.clone()
            } else {
                None
            },
            notice: view.notice.clone(),
            badge_alpha: view.badge_alpha,
            dpi: window.dpi,
        }
    }

    /// 悬停标签文字：窗口标题 / 类名 · 进程名 · 宽×高（超长字段截断）。
    fn hover_label(&self, rect: PhysicalRect) -> String {
        let size = format!("{}×{}", rect.width, rect.height);
        let Some(window) = self.hover_chain.iter().find(|w| w.bounds == rect) else {
            return size;
        };
        let title = if window.title.trim().is_empty() {
            "桌面/显示器".to_string()
        } else {
            truncate(&window.title, LABEL_TITLE_CHARS)
        };
        let class = truncate(&window.class_name, LABEL_FIELD_CHARS);
        let process = truncate(&window.process_name, LABEL_FIELD_CHARS);
        if class.is_empty() && process.is_empty() {
            format!("{title}\n{size}")
        } else {
            format!("{title}\n{class} · {process} · {size}")
        }
    }

    /// 处理本轮 pump 收集到的全部输入事件（提交/取消后停止处理剩余事件）。
    fn process_input(&mut self) {
        for event in input::take() {
            if !self.active {
                break;
            }
            self.handle_input(event);
        }
    }

    fn handle_input(&mut self, event: InputEvent) {
        // 提示期内按下/滚轮/`Enter` 统一在这里吞掉：`Enter` 必须一起忽略，否则
        // `commit_selection` 的失败分支会重新计时，连按就会无限续期提示。
        if notice_swallows(self.notice.is_some(), &event) {
            debug!(
                event = input_event_name(&event),
                "input ignored while the selection notice is showing"
            );
            return;
        }
        match event {
            InputEvent::MouseDown { point } => self.on_mouse_down(point),
            InputEvent::MouseMove { point, modifiers } => {
                // 入队时刻 → 本次 Present 完成：只有真的重绘了才是一个有效延迟样本。
                let arrived = input::take_mouse_move_at();
                if self.on_mouse_move(point, modifiers) {
                    if let Some(arrived) = arrived {
                        self.perf.record_input(arrived.elapsed());
                    }
                }
            }
            InputEvent::MouseUp => {
                self.selection.on_mouse_up();
                self.redraw_active_windows();
            }
            InputEvent::CaptureLost => self.on_capture_lost(),
            InputEvent::Nudge { key, shift } => {
                let candidates = self.candidates();
                self.selection.on_key(key, shift, &candidates);
                self.redraw_active_windows();
            }
            InputEvent::Commit => self.commit_selection(),
            InputEvent::Cancel => self.cancel_session(),
            InputEvent::Wheel {
                point,
                modifiers,
                delta,
            } => self.on_wheel(point, modifiers, delta),
        }
    }

    /// 鼠标按下：起点必须落在某台显示器内，否则忽略；命中已有选区的控制点/内部则
    /// 进入调整/移动，否则从空白处开始框选。
    ///
    /// 提示期内的按下已由 [`notice_swallows`] 在 [`OverlayThread::handle_input`] 吞掉，
    /// 这里不必再判 `notice`。`SetCapture` 放在「起点在某台显示器内」之后：点在显示器
    /// 之外时不必占着捕获（否则下一次按键/点击会先落到覆盖层线程，容易表现为界面卡死）。
    fn on_mouse_down(&mut self, point: PhysicalPoint) {
        let Some(target_display) = display_for_point(&self.displays, point) else {
            debug!(
                x = point.x,
                y = point.y,
                "mouse down outside every display; ignored"
            );
            return;
        };
        let dpi = target_display.dpi;
        match self
            .windows
            .iter()
            .find(|window| window.display_id == target_display.id)
        {
            Some(window) => {
                // SAFETY: 窗口由本线程创建；捕获后跨显示器拖拽的鼠标消息仍送到覆盖层。
                unsafe { SetCapture(window.hwnd) };
            }
            None => debug!(
                display = %target_display.id,
                "no overlay window for the pressed display; mouse not captured"
            ),
        }
        let handle = hit_test_handle(self.selection.rect, point, dpi);
        self.selection.on_mouse_down(point, handle);
        debug!(
            x = point.x,
            y = point.y,
            handle = ?handle,
            "selection mouse down"
        );
        self.redraw_active_windows();
    }

    /// 鼠标移动：全局坐标；用 `hit_chain_excluding` 取外部窗口链（排除覆盖层自身），
    /// 交给状态机更新选区/悬停高亮。返回本次是否真的重绘并呈现。
    ///
    /// 命中链按阶段节流（[`OverlayThread::hit_chain_due`]），重绘前做脏检查
    /// （[`needs_redraw`]）：热点路径上既不每次 `EnumWindows`，也不每次等一个 vblank。
    fn on_mouse_move(&mut self, point: PhysicalPoint, modifiers: Modifiers) -> bool {
        let before = self.redraw_state();
        if self.hit_chain_due(Instant::now()) {
            let previous = self.hover_chain.last().map(|window| window.hwnd);
            self.hover_chain = self.hit_chain_at(point);
            let current = self.hover_chain.last().map(|window| window.hwnd);
            if current != previous {
                // 悬停目标变化时记一条 info：手工验证「覆盖层自身的窗口已被排除」。
                match self.hover_chain.first() {
                    Some(root) => info!(
                        hwnd = root.hwnd,
                        class = %root.class_name,
                        title = %truncate(&root.title, 40),
                        depth = self.hover_chain.len(),
                        "hover window changed"
                    ),
                    None => info!("hover window cleared (no external window under pointer)"),
                }
            }
        }
        let candidates = self.candidates();
        self.selection.on_mouse_move(point, modifiers, &candidates);
        if !needs_redraw(before, self.redraw_state()) {
            return false;
        }
        self.redraw_active_windows();
        true
    }

    /// 滚轮：在指针下的候选窗口链中切换高亮（`hover_rect`），不改变用户选区。
    ///
    /// 提示期内的滚轮已由 [`notice_swallows`] 在 [`OverlayThread::handle_input`] 吞掉。
    fn on_wheel(&mut self, point: PhysicalPoint, modifiers: Modifiers, delta: i32) {
        self.last_hit_scan = Some(Instant::now());
        self.perf.record_hit_scan();
        self.hover_chain = self.hit_chain_at(point);
        let candidates = self.candidates();
        if candidates.is_empty() {
            return;
        }
        // 先把当前候选链交给状态机（`Idle | HoverWindow` 下只更新 `hover_rect`），
        // 再按滚轮增量循环；`Dragging | Adjusting` 期间 `on_wheel` 自行忽略。
        self.selection.on_mouse_move(point, modifiers, &candidates);
        self.selection.on_wheel(delta);
        debug!(
            delta,
            hover = ?self.selection.hovered_rect(),
            "wheel switched hovered window"
        );
        self.redraw_active_windows();
    }

    /// `WM_CAPTURECHANGED`：本线程失去了鼠标捕获（被其他进程抢走或窗口被禁用）。
    ///
    /// 正在拖拽/调整时立即收尾（等价于 `WM_LBUTTONUP`），否则状态机会停在
    /// `Dragging`，后续移动一直在改选区、按键也判不出「可提交」。
    fn on_capture_lost(&mut self) {
        if !matches!(
            self.selection.phase,
            SelectionPhase::Dragging | SelectionPhase::Adjusting
        ) {
            return;
        }
        info!(
            phase = ?self.selection.phase,
            "mouse capture lost; ending the in-flight drag"
        );
        self.selection.on_mouse_up();
        self.redraw_active_windows();
    }

    /// 命中链是否该刷新（并按阶段节流）：悬停态每 [`HOVER_HIT_SCAN_INTERVAL`] 一次，
    /// 拖拽/调整态每 [`DRAG_HIT_SCAN_INTERVAL`] 一次；未到间隔时复用上一条候选链。
    fn hit_chain_due(&mut self, now: Instant) -> bool {
        let interval = if matches!(
            self.selection.phase,
            SelectionPhase::Dragging | SelectionPhase::Adjusting
        ) {
            DRAG_HIT_SCAN_INTERVAL
        } else {
            HOVER_HIT_SCAN_INTERVAL
        };
        if let Some(last) = self.last_hit_scan {
            if now.saturating_duration_since(last) < interval {
                return false;
            }
        }
        self.last_hit_scan = Some(now);
        self.perf.record_hit_scan();
        true
    }

    /// 当前选区内与重绘相关的状态快照（脏检查用）。
    fn redraw_state(&self) -> RedrawState {
        RedrawState {
            phase: self.selection.phase,
            rect: self.selection.rect,
            hover_rect: self.selection.hover_rect,
            active: self.selection.active,
        }
    }

    fn hit_chain_at(&self, point: PhysicalPoint) -> Vec<WindowInfo> {
        // SAFETY: 纯查询，无指针参数。
        let pid = unsafe { GetCurrentProcessId() };
        match hit_chain_excluding(point, pid) {
            Ok(chain) => chain,
            Err(err) => {
                debug!(x = point.x, y = point.y, error = %err, "hit chain unavailable");
                Vec::new()
            }
        }
    }

    fn candidates(&self) -> Vec<PhysicalRect> {
        self.hover_chain
            .iter()
            .map(|window| window.bounds)
            .collect()
    }

    /// 提交选区：`commit()` 失败或裁剪后仍小于 [`MIN_SELECTION_SIZE`] 时显示「选区过小」
    /// 提示，约 [`NOTICE_DURATION`] 后取消；成功则回读冻结帧并阻塞发送 `Committed`。
    fn commit_selection(&mut self) {
        let Some(rect) = self.selection.commit() else {
            debug!("commit rejected: selection below minimum size");
            self.show_notice(TOO_SMALL_NOTICE);
            return;
        };
        let rect = clamp_to_virtual_screen(rect, &self.displays);
        if rect.width < MIN_SELECTION_SIZE || rect.height < MIN_SELECTION_SIZE {
            debug!(
                width = rect.width,
                height = rect.height,
                "commit rejected: clamped selection below minimum size"
            );
            self.selection.cancel();
            self.show_notice(TOO_SMALL_NOTICE);
            return;
        }

        match self.read_selection(rect) {
            Ok(image) => {
                info!(
                    width = image.width,
                    height = image.height,
                    x = rect.x,
                    y = rect.y,
                    "selection readback complete"
                );
                let foreground = self.foreground.clone().unwrap_or_default();
                // 先隐藏窗口并把焦点还给来源窗口，再交付结果（app-core 可能立刻落盘）。
                self.hide_all();
                self.send_event_blocking(OverlayEvent::Committed {
                    image,
                    global_rect: rect,
                    foreground,
                });
            }
            Err(err) => {
                warn!(
                    x = rect.x,
                    y = rect.y,
                    width = rect.width,
                    height = rect.height,
                    error = %err,
                    "selection readback failed; cancelling"
                );
                self.hide_all();
                self.send_event(OverlayEvent::Cancelled);
            }
        }
    }

    /// 显示一条提示并立即重绘；到期后由 [`OverlayThread::tick`] 取消会话。
    fn show_notice(&mut self, text: &'static str) {
        self.notice = Some(Notice {
            text,
            until: Instant::now() + NOTICE_DURATION,
        });
        self.redraw_active_windows();
    }

    /// 逐显示器回读全局选区并拼合成全局尺寸的 BGRA 图。
    ///
    /// 每台显示器：`global_rect ∩ 冻结纹理范围` → staging 回读 → 以
    /// `(选区内偏移, 纹理内偏移)` 拼进目标缓冲；不相交的显示器不贡献像素。
    fn read_selection(&self, rect: PhysicalRect) -> Result<CapturedImage, AppError> {
        let mut regions: Vec<(u32, u32, u32, u32, Vec<u8>)> = Vec::new();
        for display in &self.displays {
            let Some(window) = self
                .windows
                .iter()
                .find(|window| window.display_id == display.id)
            else {
                continue;
            };
            let Some(frozen) = window.frozen.as_ref() else {
                continue;
            };
            // 纹理左上角与显示器左上角对齐；纹理比显示器小（分辨率切换瞬间）时按纹理裁剪。
            let texture = PhysicalRect::new(
                display.bounds.x,
                display.bounds.y,
                frozen.width,
                frozen.height,
            );
            let Some((dst_x, dst_y, src_x, src_y, width, height)) =
                input::region_offsets(rect, texture)
            else {
                continue;
            };
            let data =
                self.device
                    .read_back_region(&frozen.texture, src_x, src_y, width, height)?;
            regions.push((dst_x, dst_y, width, height, data));
        }

        if regions.is_empty() {
            return Err(AppError::CaptureFailed(
                "selection does not overlap any frozen display".into(),
            ));
        }
        let bgra = input::compose_bgra(rect.width, rect.height, &regions);
        // SAFETY 无关：这里只做长度自检，避免把半截图像交给上层。
        if bgra.len() != rect.width as usize * rect.height as usize * 4 {
            return Err(AppError::Internal(format!(
                "composed image size mismatch: {} bytes for {}x{}",
                bgra.len(),
                rect.width,
                rect.height
            )));
        }
        Ok(CapturedImage {
            width: rect.width,
            height: rect.height,
            bgra,
        })
    }

    /// 每个命令循环周期的收尾工作：提示到期 → 取消会话；角标淡出结束 → 停计时器。
    fn tick(&mut self) {
        if !self.active {
            return;
        }
        let now = Instant::now();
        if self
            .notice
            .as_ref()
            .is_some_and(|notice| notice_expired(notice, now))
        {
            self.notice = None;
            info!("selection too small notice expired; cancelling session");
            self.cancel_session();
            return;
        }
        if self.badge.as_ref().is_some_and(|badge| now >= badge.until) {
            self.stop_badge_timer();
            // 再重绘一次，擦掉最后的角标（此时 `badge_alpha` 已为 0）。
            REDRAW_REQUESTED.store(true, Ordering::Release);
        }
    }

    /// 找到（或按帧尺寸重建）`display_id` 对应的窗口；返回其在 `windows` 中的下标。
    fn ensure_window(
        &mut self,
        display_id: &str,
        bounds: PhysicalRect,
        dpi: u32,
        width: u32,
        height: u32,
    ) -> Result<usize, AppError> {
        let (window_width, window_height) = window_size(width, height)?;
        if let Some(index) = self
            .windows
            .iter()
            .position(|window| window.display_id == display_id)
        {
            if self.windows[index].renderer.size() == (width, height) {
                let hwnd = self.windows[index].hwnd;
                // 显示器可能被重新排列：把窗口几何同步到 DisplayInfo.bounds。
                // SAFETY: hwnd 有效；只修改本进程窗口的位置/尺寸，不激活、不改 Z 序。
                unsafe {
                    SetWindowPos(
                        hwnd,
                        None,
                        bounds.x,
                        bounds.y,
                        window_width,
                        window_height,
                        SWP_NOACTIVATE | SWP_NOZORDER,
                    )
                }
                .map_err(|err| AppError::Internal(format!("SetWindowPos failed: {err}")))?;
                // 全局坐标 → 客户区坐标依赖最新边界与 DPI。
                self.windows[index].bounds = bounds;
                self.windows[index].dpi = dpi;
                return Ok(index);
            }
            info!(
                display = %display_id,
                "overlay window size changed (resolution/DPI); recreating"
            );
            self.remove_window(index);
        }

        let window = create_overlay_window(&self.device, self.hinstance, display_id, bounds, dpi)?;
        self.windows.push(window);
        Ok(self.windows.len() - 1)
    }

    /// 先释放交换链/冻结纹理，再销毁窗口。
    fn remove_window(&mut self, index: usize) {
        let old = self.windows.remove(index);
        let hwnd = old.hwnd;
        drop(old);
        // SAFETY: hwnd 由本线程创建且尚未销毁。
        let _ = unsafe { DestroyWindow(hwnd) };
    }

    /// 优先聚焦主显示器窗口，否则取第一个有冻结帧的窗口。
    fn focus_target(&self, primary_id: Option<&str>) -> Option<HWND> {
        if let Some(primary_id) = primary_id {
            if let Some(window) = self
                .windows
                .iter()
                .find(|window| window.display_id == primary_id && window.frozen.is_some())
            {
                return Some(window.hwnd);
            }
        }
        self.windows
            .iter()
            .find(|window| window.frozen.is_some())
            .map(|window| window.hwnd)
    }

    /// 处理本线程队列里的全部窗口消息；返回是否收到 `WM_QUIT`（调用方据此退出外层循环）。
    ///
    /// 重绘不在窗口过程里做：`WM_PAINT` 等消息只置位 [`REDRAW_REQUESTED`]，pump 结束后
    /// 这里统一对全部活动窗口 `draw_frozen + present`。
    fn pump_messages(&mut self) -> bool {
        let mut quit = false;
        let mut msg = MSG::default();
        loop {
            // SAFETY: `msg` 指向栈上有效变量；`PM_REMOVE` 取走消息。
            let has_message = unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) };
            if !has_message.as_bool() {
                break;
            }
            if msg.message == WM_QUIT {
                debug!("overlay thread received WM_QUIT");
                quit = true;
                break;
            }
            // SAFETY: 消息来自本线程队列，转发给本模块的窗口过程。
            unsafe {
                let _ = TranslateMessage(&msg);
                let _ = DispatchMessageW(&msg);
            }
        }
        if quit {
            return true;
        }
        if REDRAW_REQUESTED.swap(false, Ordering::AcqRel) {
            self.redraw_active_windows();
        }
        false
    }
}

/// 「选区过小」提示期的输入门控：返回 `true` 表示该事件被提示吞掉。
///
/// 提示显示的 1s 内，按下、滚轮与 `Enter` 提交都被忽略：用户在提示期间重试不会产生
/// 新选区或新提交；`Enter` 若被放过，`commit_selection` 的失败分支会再次
/// [`OverlayThread::show_notice`]，连按就会不停续期，会话永远不会结束。
/// `Esc`/右键取消不受影响；提示到期后仍由 [`OverlayThread::tick`] 取消会话。
fn notice_swallows(notice_active: bool, event: &InputEvent) -> bool {
    notice_active
        && matches!(
            event,
            InputEvent::MouseDown { .. } | InputEvent::Wheel { .. } | InputEvent::Commit
        )
}

/// 提示是否到期（[`OverlayThread::tick`] 据此取消会话）。
///
/// 到期时刻只由 `Notice.until` 决定；提示期内的输入被 [`notice_swallows`] 吞掉，
/// 因此连按 `Enter` 不会重置 `until`，会话一定会在首次显示后 1s 结束。
fn notice_expired(notice: &Notice, now: Instant) -> bool {
    now >= notice.until
}

/// 输入事件种类名（日志用；不打印坐标或图像数据）。
fn input_event_name(event: &InputEvent) -> &'static str {
    match event {
        InputEvent::MouseDown { .. } => "MouseDown",
        InputEvent::MouseMove { .. } => "MouseMove",
        InputEvent::MouseUp => "MouseUp",
        InputEvent::CaptureLost => "CaptureLost",
        InputEvent::Nudge { .. } => "Nudge",
        InputEvent::Commit => "Commit",
        InputEvent::Cancel => "Cancel",
        InputEvent::Wheel { .. } => "Wheel",
    }
}

impl Drop for OverlayThread {
    fn drop(&mut self) {
        let was_active = self.active;
        self.hide_all();
        // 线程退出（WM_QUIT、通道断开）时若仍处于激活态，补发一次 Cancelled：
        // 等待事件的调用方不会被永久挂起。
        if was_active {
            self.send_event(OverlayEvent::Cancelled);
        }
        let hwnds: Vec<HWND> = self.windows.iter().map(|window| window.hwnd).collect();
        // 先释放冻结纹理与交换链（Renderer），再销毁窗口。
        self.windows.clear();
        for hwnd in hwnds {
            // SAFETY: hwnd 由本线程创建且尚未销毁。
            let _ = unsafe { DestroyWindow(hwnd) };
        }
        // 重复注册的类不属于本线程，不注销（见 `register_overlay_class`）。
        if self.class_registered {
            unregister_overlay_class(self.hinstance);
            self.class_registered = false;
        }
    }
}

/// COM 单线程 apartment 守卫：离开作用域时 `CoUninitialize`。
///
/// `!Send`：apartment 绑定当前线程，守卫只能由创建它的覆盖层线程 drop。
struct ComGuard {
    _not_send: PhantomData<*const ()>,
}

impl ComGuard {
    fn init() -> Result<ComGuard, AppError> {
        // SAFETY: 单线程 apartment 初始化；`None` 表示默认内存分配器。
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
            .ok()
            .map_err(|err| AppError::Internal(format!("CoInitializeEx failed: {err}")))?;
        Ok(ComGuard {
            _not_send: PhantomData,
        })
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        // SAFETY: 与 `CoInitializeEx` 成对调用，且只在覆盖层线程。
        unsafe { CoUninitialize() };
    }
}

/// 注册覆盖层窗口类；返回本次是否真正注册（重复注册时返回 `false`）。
fn register_overlay_class(hinstance: HINSTANCE) -> Result<bool, AppError> {
    let class_name = wide(OVERLAY_CLASS_NAME);
    // SAFETY: 系统光标资源；`IDC_ARROW` 是资源 id，无需释放。
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
        let err = windows::core::Error::from_thread();
        // 同类已注册（例如上一次覆盖层线程的残留）不视为失败，但不由本次注销。
        if err.code() == windows::core::HRESULT::from_win32(ERROR_CLASS_ALREADY_EXISTS.0) {
            debug!("overlay window class already registered");
            return Ok(false);
        }
        return Err(AppError::Internal(format!(
            "RegisterClassW failed for {OVERLAY_CLASS_NAME}: {err}"
        )));
    }
    Ok(true)
}

/// 注销覆盖层窗口类；失败只影响下次注册，忽略返回值。
fn unregister_overlay_class(hinstance: HINSTANCE) {
    let class_name = wide(OVERLAY_CLASS_NAME);
    // SAFETY: 类名以 NUL 结尾且在调用期间存活；该类所有窗口都已销毁。
    unsafe {
        let _ = UnregisterClassW(PCWSTR(class_name.as_ptr()), Some(hinstance));
    }
}

/// 创建一个隐藏的置顶弹出窗口，并为它建立交换链与渲染器。
///
/// `bounds` 是所在显示器的物理像素边界（窗口位置 = `bounds.x/y`，尺寸 = `bounds`）；
/// 窗口创建后保持隐藏，`show` 时再 `SW_SHOW`，避免预创建阶段闪烁。
fn create_overlay_window(
    device: &D3dContext,
    hinstance: HINSTANCE,
    display_id: &str,
    bounds: PhysicalRect,
    dpi: u32,
) -> Result<OverlayWindow, AppError> {
    let (window_width, window_height) = window_size(bounds.width, bounds.height)?;
    let class_name = wide(OVERLAY_CLASS_NAME);
    let title = wide(OVERLAY_WINDOW_TITLE);
    // 不用 WS_EX_NOACTIVATE：覆盖层需要键盘焦点（design §7.1）。
    let ex_style = WS_EX_TOPMOST | WS_EX_TOOLWINDOW;
    // SAFETY: 类名/标题以 NUL 结尾且在调用期间存活；父窗口/菜单为空；hinstance 来自本进程。
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(ex_style.0),
            PCWSTR(class_name.as_ptr()),
            PCWSTR(title.as_ptr()),
            WS_POPUP,
            bounds.x,
            bounds.y,
            window_width,
            window_height,
            None,
            None,
            Some(hinstance),
            None,
        )
    }
    .map_err(|err| AppError::Internal(format!("CreateWindowExW failed: {err}")))?;

    let renderer = match device
        .create_swap_chain(hwnd, bounds.width, bounds.height)
        .and_then(|swap| Renderer::new(device, swap))
    {
        Ok(renderer) => renderer,
        Err(err) => {
            // SAFETY: hwnd 刚创建；交换链/渲染器不可用，窗口不再使用。
            let _ = unsafe { DestroyWindow(hwnd) };
            return Err(err);
        }
    };

    debug!(
        display = %display_id,
        x = bounds.x,
        y = bounds.y,
        width = bounds.width,
        height = bounds.height,
        "hidden overlay window created"
    );
    Ok(OverlayWindow {
        display_id: display_id.to_string(),
        hwnd,
        renderer,
        frozen: None,
        bounds,
        dpi,
    })
}

/// 窗口/交换链尺寸转 Win32 `i32`。
fn window_size(width: u32, height: u32) -> Result<(i32, i32), AppError> {
    let window_width = i32::try_from(width)
        .map_err(|_| AppError::InvalidInput(format!("window width {width} too large")))?;
    let window_height = i32::try_from(height)
        .map_err(|_| AppError::InvalidInput(format!("window height {height} too large")))?;
    Ok((window_width, window_height))
}

/// 把前台窗口切到 `hwnd` 并给它键盘焦点（design §7.1）。
fn focus_window(hwnd: HWND) {
    // SAFETY: hwnd 由覆盖层线程创建；即使已被销毁，这里的调用也只是失败。
    unsafe {
        if !IsWindow(Some(hwnd)).as_bool() {
            warn!("overlay window no longer exists; cannot focus");
            return;
        }
        if !SetForegroundWindow(hwnd).as_bool() {
            if attached_set_foreground(hwnd) {
                info!("overlay window brought to foreground via AttachThreadInput fallback");
            } else {
                warn!("failed to bring overlay window to the foreground");
            }
        } else {
            debug!("overlay window moved to foreground");
        }
        if SetFocus(Some(hwnd)).is_err() {
            debug!("SetFocus on overlay window failed");
        }
    }
}

/// 把焦点还给捕获前的前台窗口；窗口已销毁时静默跳过。
fn restore_foreground(foreground: &ForegroundInfo) {
    if foreground.hwnd == 0 {
        return;
    }
    let hwnd = HWND(foreground.hwnd as *mut c_void);
    // SAFETY: hwnd 是捕获前记录的前台窗口句柄，可能已被目标进程销毁；先 `IsWindow` 校验。
    unsafe {
        if !IsWindow(Some(hwnd)).as_bool() {
            debug!("captured foreground window no longer exists; focus not restored");
            return;
        }
        let restored = SetForegroundWindow(hwnd).as_bool() || attached_set_foreground(hwnd);
        if restored {
            info!(
                hwnd = foreground.hwnd,
                "focus restored to captured foreground window"
            );
        } else {
            warn!(
                hwnd = foreground.hwnd,
                "failed to restore focus to captured foreground window"
            );
        }
    }
}

/// `AttachThreadInput` 兜底：把本线程输入队列临时挂到其他线程上，继承前台权后再
/// `SetForegroundWindow(hwnd)`（design §7.1 的兜底路径）。
///
/// 先挂到当前前台窗口的线程（标准做法）；当前台窗口就是本线程的覆盖层时，改挂到
/// `hwnd` 自己的线程。附着/解除严格配对：每次成功的 `AttachThreadInput(current, t, true)`
/// 都在同一分支内紧跟 `AttachThreadInput(current, t, false)`（`SetForegroundWindow`
/// 失败也照样解除），分支内没有 `?`/提前 `return`，因此不会遗留附着状态。
fn attached_set_foreground(hwnd: HWND) -> bool {
    // SAFETY: 线程 id 由 Win32 查询得到；附着与解除成对调用，窗口句柄只读。
    unsafe {
        let current = GetCurrentThreadId();
        let foreground = GetForegroundWindow();
        let mut candidates = [0u32; 2];
        let mut count = 0;
        if !foreground.0.is_null() {
            candidates[count] = GetWindowThreadProcessId(foreground, None);
            count += 1;
        }
        let target_thread = GetWindowThreadProcessId(hwnd, None);
        if target_thread != 0 && !candidates[..count].contains(&target_thread) {
            candidates[count] = target_thread;
            count += 1;
        }

        for &thread in &candidates[..count] {
            if thread == 0 || thread == current {
                continue;
            }
            if !AttachThreadInput(current, thread, true).as_bool() {
                continue;
            }
            let ok = SetForegroundWindow(hwnd).as_bool();
            // SAFETY: 与上面的 `AttachThreadInput(current, thread, true)` 成对；
            // 无论 `SetForegroundWindow` 成功与否都必须解除附着。
            let _ = AttachThreadInput(current, thread, false);
            if ok {
                return true;
            }
        }
        false
    }
}

/// 覆盖层窗口过程：绘制类消息只置位重绘标志，输入消息翻译成 [`InputEvent`] 入队。
///
/// 这里**不碰**覆盖层线程的可变状态（`SelectionState` 等）：所有输入都交给
/// [`input::push`]，覆盖层线程 pump 完消息后统一处理。`SetCapture` 不在这里调用——
/// 是否捕获取决于「按下点是否落在某台显示器内」，由覆盖层线程
/// （[`OverlayThread::on_mouse_down`]）判断后再设置；抬起时这里直接释放，保证不会
/// 因为一条被丢弃的输入事件而永久持有捕获。
unsafe extern "system" fn overlay_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_PAINT => {
            // 冻结帧由 D3D/D2D 呈现，GDI 只负责验证更新区域（否则 WM_PAINT 会反复投递），
            // 真正的重绘交给覆盖层线程（见 `REDRAW_REQUESTED`）。
            let mut paint = PAINTSTRUCT::default();
            // SAFETY: hwnd 有效；`paint` 是栈上有效结构。
            unsafe {
                let _ = BeginPaint(hwnd, &mut paint);
                let _ = EndPaint(hwnd, &paint);
            }
            REDRAW_REQUESTED.store(true, Ordering::Release);
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            if let Some(point) = input::cursor_point() {
                input::push(InputEvent::MouseMove {
                    point,
                    modifiers: input::current_modifiers(),
                });
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            // 捕获由覆盖层线程在确认「起点落在显示器内」之后设置（见 on_mouse_down）。
            if let Some(point) = input::cursor_point() {
                input::push(InputEvent::MouseDown { point });
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            input::push(InputEvent::MouseUp);
            // SAFETY: 与 `on_mouse_down` 里的 `SetCapture` 成对；未捕获时返回失败但无副作用。
            // 放在 push 之后：`WM_CAPTURECHANGED` 由 `ReleaseCapture` 同步触发，
            // 这样队列顺序是 `MouseUp` 在前、`CaptureLost` 在后。
            unsafe {
                let _ = ReleaseCapture();
            }
            LRESULT(0)
        }
        // 捕获被抢走（其他进程 `SetCapture`、窗口被禁用等）：交给覆盖层线程收尾，
        // 避免状态机卡在 `Dragging/Adjusting`。
        WM_CAPTURECHANGED => {
            input::push(InputEvent::CaptureLost);
            LRESULT(0)
        }
        WM_RBUTTONDOWN => {
            input::push(InputEvent::Cancel);
            LRESULT(0)
        }
        WM_KEYDOWN => {
            let shift = input::current_modifiers().shift;
            if let Some(event) = input::event_for_key(wparam.0 as u16, shift) {
                input::push(event);
            }
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            if let Some(point) = input::cursor_point() {
                input::push(InputEvent::Wheel {
                    point,
                    modifiers: input::current_modifiers(),
                    delta: input::wheel_delta(wparam.0),
                });
            }
            LRESULT(0)
        }
        // 角标淡出节拍：只请求重绘，是否继续由覆盖层线程按截止时间决定。
        WM_TIMER => {
            REDRAW_REQUESTED.store(true, Ordering::Release);
            LRESULT(0)
        }
        // 分辨率/DPI 变化后交换链内容需要重绘（几何重建由下一次 `show`/`ensure_window`
        // 负责）；先置位标志，再交给默认处理。
        WM_DISPLAYCHANGE | WM_DPICHANGED => {
            REDRAW_REQUESTED.store(true, Ordering::Release);
            // SAFETY: 默认处理；参数原样转发。
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        // 背景由 D3D 负责，禁止 GDI 擦除造成闪烁。
        WM_ERASEBKGND => LRESULT(1),
        // 取消由 `Esc`/右键（输入事件）驱动；忽略 WM_CLOSE/Alt+F4，避免窗口在管理结构
        // 之外被销毁。
        WM_CLOSE => LRESULT(0),
        // SAFETY: 默认处理；参数原样转发。
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// UTF-16（NUL 结尾）缓冲区。
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 按字符数截断（超出时补 `…`），避免超长标题撑破覆盖层标签。
fn truncate(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let mut out: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        out.push('…');
    }
    out
}

/// 尺寸标签的唯一归属窗口：包含选区右下角最后一个像素的窗口，否则包含中心的窗口。
///
/// 跨屏选区只在其中一台显示器上画尺寸标签，避免每块屏各画一次。
fn label_owner_bottom_right(rect: PhysicalRect, windows: &[PhysicalRect]) -> Option<usize> {
    render::point_owner(render::bottom_right(rect), windows)
        .or_else(|| render::point_owner(render::center(rect), windows))
}

/// 悬停窗口标签的唯一归属窗口：包含 hover_rect 左上角的窗口，否则包含中心的窗口。
fn label_owner_top_left(rect: PhysicalRect, windows: &[PhysicalRect]) -> Option<usize> {
    render::point_owner(
        PhysicalPoint {
            x: rect.x,
            y: rect.y,
        },
        windows,
    )
    .or_else(|| render::point_owner(render::center(rect), windows))
}

/// 记录被丢弃的覆盖层事件：只输出种类与尺寸/矩形。
///
/// `CapturedImage.bgra` 可能有几十 MB，绝不能 `Debug` 打印整个事件。
fn log_dropped_event(reason: &'static str, event: &OverlayEvent) {
    match event {
        OverlayEvent::Committed {
            image, global_rect, ..
        } => warn!(
            reason,
            event = "Committed",
            width = image.width,
            height = image.height,
            x = global_rect.x,
            y = global_rect.y,
            "overlay event dropped"
        ),
        OverlayEvent::Cancelled => {
            warn!(reason, event = "Cancelled", "overlay event dropped")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(
        phase: SelectionPhase,
        rect: PhysicalRect,
        hover: PhysicalRect,
        active: Option<Handle>,
    ) -> RedrawState {
        RedrawState {
            phase,
            rect,
            hover_rect: hover,
            active,
        }
    }

    #[test]
    fn needs_redraw_only_when_render_state_changes() {
        let rect = PhysicalRect::new(10, 20, 100, 50);
        let hover = PhysicalRect::new(0, 0, 640, 480);
        let before = state(SelectionPhase::HoverWindow, rect, hover, None);

        // 状态完全一致：鼠标在同一窗口内移动时不重绘。
        assert!(!needs_redraw(before, before));
        // 选区、悬停矩形、阶段、手柄任一变化都要重绘。
        assert!(needs_redraw(
            before,
            state(
                SelectionPhase::HoverWindow,
                PhysicalRect::new(10, 20, 101, 50),
                hover,
                None
            )
        ));
        assert!(needs_redraw(
            before,
            state(
                SelectionPhase::HoverWindow,
                rect,
                PhysicalRect::new(0, 0, 641, 480),
                None
            )
        ));
        assert!(needs_redraw(
            before,
            state(SelectionPhase::Dragging, rect, hover, None)
        ));
        assert!(needs_redraw(
            before,
            state(SelectionPhase::Adjusting, rect, hover, Some(Handle::East))
        ));
    }

    #[test]
    fn label_owners_pick_a_single_window() {
        let left = PhysicalRect::new(0, 0, 2560, 1440);
        let right = PhysicalRect::new(2560, 0, 2560, 1440);
        let windows = [left, right];

        // 跨屏选区：右下角在右屏 → 尺寸标签只归右屏。
        let cross = PhysicalRect::new(2400, 100, 400, 200);
        assert_eq!(label_owner_bottom_right(cross, &windows), Some(1));
        // 悬停窗口整体在左屏 → 悬停标签只归左屏。
        let hover = PhysicalRect::new(100, 100, 300, 200);
        assert_eq!(label_owner_top_left(hover, &windows), Some(0));
        // 完全落在虚拟屏幕之外（屏间空隙）时没有归属窗口。
        let gap = PhysicalRect::new(-100, 100, 50, 50);
        assert_eq!(label_owner_bottom_right(gap, &windows), None);
        assert_eq!(label_owner_top_left(gap, &windows), None);
    }

    #[test]
    fn perf_stats_average_ignores_zero_samples() {
        let empty = PerfStats::default();
        assert_eq!(avg_ms(empty.draw_total, empty.redraws), 0.0);
        assert_eq!(avg_ms(empty.input_total, empty.input_samples), 0.0);

        let mut stats = PerfStats::default();
        stats.record_redraw(Duration::from_millis(3));
        stats.record_redraw(Duration::from_millis(5));
        assert_eq!(stats.redraws, 2);
        assert!((ms(stats.draw_max) - 5.0).abs() < 1e-9);
        assert!((avg_ms(stats.draw_total, stats.redraws) - 4.0).abs() < 1e-9);

        stats.record_input(Duration::from_micros(2500));
        assert_eq!(stats.input_samples, 1);
        assert!((ms(stats.input_max) - 2.5).abs() < 1e-9);
    }

    #[test]
    fn notice_swallows_mouse_and_commit_but_not_escape() {
        let point = PhysicalPoint { x: 10, y: 20 };
        let modifiers = Modifiers::default();
        let mouse_down = InputEvent::MouseDown { point };
        let wheel = InputEvent::Wheel {
            point,
            modifiers,
            delta: 120,
        };

        // 提示期内：按下、滚轮与 `Enter` 都被吞掉。`Enter` 不进入 `commit_selection`，
        // 因此不会再次 `show_notice`、也不会把提示计时续期。
        assert!(notice_swallows(true, &mouse_down));
        assert!(notice_swallows(true, &wheel));
        assert!(notice_swallows(true, &InputEvent::Commit));

        // `Esc`/右键取消、抬起、移动、微调照常处理。
        assert!(!notice_swallows(true, &InputEvent::Cancel));
        assert!(!notice_swallows(true, &InputEvent::MouseUp));
        assert!(!notice_swallows(
            true,
            &InputEvent::MouseMove { point, modifiers }
        ));
        assert!(!notice_swallows(
            true,
            &InputEvent::Nudge {
                key: crate::selection::Key::Left,
                shift: false
            }
        ));
        assert!(!notice_swallows(true, &InputEvent::CaptureLost));

        // 没有提示时什么都不吞。
        assert!(!notice_swallows(false, &mouse_down));
        assert!(!notice_swallows(false, &wheel));
        assert!(!notice_swallows(false, &InputEvent::Commit));
    }

    #[test]
    fn notice_deadline_is_not_extended_by_ignored_input() {
        let start = Instant::now();
        let notice = Notice {
            text: TOO_SMALL_NOTICE,
            until: start + NOTICE_DURATION,
        };
        // 提示期内的 `Enter` 被门控吞掉（不会重新 `show_notice`），截止时刻保持不变。
        let before = notice.until;
        assert!(notice_swallows(true, &InputEvent::Commit));
        assert_eq!(notice.until, before);
        // 到期判定只看 `until`：<1s 未到期，≥1s 到期。
        assert!(!notice_expired(&notice, start));
        assert!(!notice_expired(&notice, before - Duration::from_millis(1)));
        assert!(notice_expired(&notice, before));
    }
}
