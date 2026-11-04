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
//! 阶段 01 的 Task 11 只实现冻结帧显示与线程/句柄，不包含选区状态机、输入处理与
//! 提交回读（Task 12/13）；窗口过程只处理绘制/擦除消息。
//!
//! 重绘：冻结帧由 D3D/D2D 直接呈现到交换链，因此 `WM_PAINT` 不能走 GDI 绘制。窗口
//! 过程在 `WM_PAINT`/`WM_DISPLAYCHANGE`/`WM_DPICHANGED` 时置位线程级原子
//! [`REDRAW_REQUESTED`]，覆盖层线程 pump 完消息后对全部活动窗口重新
//! `draw_frozen + present`；空闲时不使用定时器。

pub mod d3d;
pub mod frame;
pub mod render;

pub use d3d::{D3dContext, SwapChain};
pub use frame::{upload_frame, validate_frame_shape, FrozenTexture};
pub use render::Renderer;

use std::ffi::c_void;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{bounded, unbounded, Receiver, RecvTimeoutError, SendTimeoutError, Sender};
use tracing::{debug, info, warn};
use visrecall_capture::enumerate_displays;
use visrecall_domain::capture::{CapturedImage, DisplayInfo, ForegroundInfo, RawFrame};
use visrecall_domain::geometry::PhysicalRect;
use visrecall_domain::AppError;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    ERROR_CLASS_ALREADY_EXISTS, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, PAINTSTRUCT};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect,
    GetForegroundWindow, GetWindowThreadProcessId, IsWindow, LoadCursorW, PeekMessageW,
    RegisterClassW, SetForegroundWindow, SetWindowPos, ShowWindow, TranslateMessage,
    UnregisterClassW, CS_HREDRAW, CS_VREDRAW, HICON, IDC_ARROW, MSG, PM_REMOVE, SWP_NOACTIVATE,
    SWP_NOZORDER, SW_HIDE, SW_SHOW, WINDOW_EX_STYLE, WM_CLOSE, WM_DISPLAYCHANGE, WM_DPICHANGED,
    WM_ERASEBKGND, WM_PAINT, WM_QUIT, WNDCLASSW, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

/// 覆盖层窗口类名（design §7.1）；示例/测试通过 `FindWindowW` 定位覆盖窗口。
pub const OVERLAY_CLASS_NAME: &str = "VisRecallOverlay";
const OVERLAY_WINDOW_TITLE: &str = "VisRecall Overlay";
/// 激活态等待命令的超时：既要及时 pump 窗口消息，又不在空闲时忙等。
const ACTIVE_POLL_INTERVAL: Duration = Duration::from_millis(8);
/// 发送事件的最长阻塞时间：pump 循环不能被一个没有读者的事件通道卡死。
const EVENT_SEND_TIMEOUT: Duration = Duration::from_millis(100);

/// 窗口过程 → 覆盖层线程的「冻结帧需要重绘」标志（线程级，无锁）。
///
/// 由 `WM_PAINT`/`WM_DISPLAYCHANGE`/`WM_DPICHANGED` 置位；覆盖层线程在 pump 完消息后
/// 用 `swap(false)` 消费一次并对全部活动窗口重新呈现。窗口过程里不做 D3D 呈现，
/// 避免渲染调用发生在消息分发过程中间。
static REDRAW_REQUESTED: AtomicBool = AtomicBool::new(false);

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
}

/// 覆盖层线程 → app-core 的事件。
#[derive(Debug)]
pub enum OverlayEvent {
    /// 提交选区后的回读结果（Task 13 才发送）。
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
            let window = create_overlay_window(
                &overlay.device,
                hinstance,
                display_id,
                bounds.x,
                bounds.y,
                bounds.width,
                bounds.height,
            )?;
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
            let command = if self.active {
                // 激活时既有窗口消息（WM_PAINT 重绘、WM_QUIT）又有命令要处理：先 pump，
                // 再短超时收命令。
                if self.pump_messages() {
                    info!("overlay thread quitting: WM_QUIT received");
                    break;
                }
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
                OverlayCommand::Cancel => self.cancel(),
            }
        }
        debug!("overlay command loop exited; overlay thread stopping");
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
            let index = self.ensure_window(
                &frame.display_id,
                bounds.x,
                bounds.y,
                frame.width,
                frame.height,
            )?;
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

        let mut first_present_ms = None;
        for window in &mut self.windows {
            let Some(frozen) = window.frozen.as_ref() else {
                continue;
            };
            debug!(display = %window.display_id, "drawing frozen frame");
            window.renderer.draw_frozen(frozen)?;
            debug!(display = %window.display_id, "presenting frozen frame");
            window.renderer.present()?;
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
        match self.focus_target(primary_id.as_deref()) {
            Some(hwnd) => focus_window(hwnd),
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
    ///
    /// 无论是否处于激活态都发一次 [`OverlayEvent::Cancelled`]：`OverlayHandle::cancel()`
    /// 之后等待事件的调用方不会因为「本来就没显示」而永久挂起。重复的 `Cancelled`
    /// 必须幂等（Task 14 的编排按此语义实现）。
    fn cancel(&mut self) {
        if self.active {
            self.hide_all();
            info!("frozen overlay cancelled");
        } else {
            debug!("cancel received with no active overlay; acknowledging with Cancelled");
        }
        self.send_event(OverlayEvent::Cancelled);
    }

    /// 隐藏全部覆盖窗口并释放冻结纹理；活动时先恢复焦点再隐藏。
    ///
    /// 顺序很重要：覆盖层仍是前台进程时 `SetForegroundWindow(目标应用)` 更容易被前台锁
    /// 接受；若先 `SW_HIDE`，前台窗口会变成空/其他进程窗口，只能退化为
    /// `AttachThreadInput` 兜底（更容易失败，日志里也会多一条 warn）。
    fn hide_all(&mut self) {
        if self.active {
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
    }

    /// 发送覆盖层事件；通道阻塞超过 [`EVENT_SEND_TIMEOUT`] 就丢弃并 warn。
    ///
    /// 绝不阻塞 pump：事件通道的读者（app-core）可能正忙或已经退出，覆盖层线程不能
    /// 因此卡在 `send` 上，连窗口消息都处理不了。Task 14 会使用无界通道，正常情况下
    /// 不会触发超时分支。
    fn send_event(&self, event: OverlayEvent) {
        match self.events.send_timeout(event, EVENT_SEND_TIMEOUT) {
            Ok(()) => {}
            Err(SendTimeoutError::Timeout(event)) => {
                warn!(?event, "overlay event channel blocked; event dropped");
            }
            Err(SendTimeoutError::Disconnected(event)) => {
                warn!(?event, "overlay event channel closed; event dropped");
            }
        }
    }

    /// 对所有活动窗口重新绘制并呈现当前冻结帧（`WM_PAINT`/显示变化触发）。
    ///
    /// 只在有冻结纹理的窗口上工作；单个窗口失败只 warn，不影响其他显示器。
    fn redraw_active_windows(&mut self) {
        if !self.active {
            return;
        }
        let mut redrawn = 0usize;
        for window in &mut self.windows {
            let Some(frozen) = window.frozen.as_ref() else {
                continue;
            };
            if let Err(err) = window.renderer.draw_frozen(frozen) {
                warn!(display = %window.display_id, error = %err, "redrawing frozen frame failed");
                continue;
            }
            if let Err(err) = window.renderer.present() {
                warn!(display = %window.display_id, error = %err, "presenting redrawn frozen frame failed");
                continue;
            }
            redrawn += 1;
        }
        if redrawn > 0 {
            info!(windows = redrawn, "frozen frames redrawn after WM_PAINT");
        }
    }

    /// 找到（或按帧尺寸重建）`display_id` 对应的窗口；返回其在 `windows` 中的下标。
    fn ensure_window(
        &mut self,
        display_id: &str,
        x: i32,
        y: i32,
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
                        x,
                        y,
                        window_width,
                        window_height,
                        SWP_NOACTIVATE | SWP_NOZORDER,
                    )
                }
                .map_err(|err| AppError::Internal(format!("SetWindowPos failed: {err}")))?;
                return Ok(index);
            }
            info!(
                display = %display_id,
                "overlay window size changed (resolution/DPI); recreating"
            );
            self.remove_window(index);
        }

        let window = create_overlay_window(
            &self.device,
            self.hinstance,
            display_id,
            x,
            y,
            width,
            height,
        )?;
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
/// 位置/尺寸都是物理像素；`width × height` 是窗口客户区（`WS_POPUP` 无边框）。
/// 窗口创建后保持隐藏，`show` 时再 `SW_SHOW`，避免预创建阶段闪烁。
fn create_overlay_window(
    device: &D3dContext,
    hinstance: HINSTANCE,
    display_id: &str,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
) -> Result<OverlayWindow, AppError> {
    let (window_width, window_height) = window_size(width, height)?;
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
            x,
            y,
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
        .create_swap_chain(hwnd, width, height)
        .and_then(|swap| Renderer::new(device, swap))
    {
        Ok(renderer) => renderer,
        Err(err) => {
            // SAFETY: hwnd 刚创建；交换链/渲染器不可用，窗口不再使用。
            let _ = unsafe { DestroyWindow(hwnd) };
            return Err(err);
        }
    };

    debug!(display = %display_id, x, y, width, height, "hidden overlay window created");
    Ok(OverlayWindow {
        display_id: display_id.to_string(),
        hwnd,
        renderer,
        frozen: None,
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

/// 覆盖层窗口过程：绘制类消息只置位重绘标志，取消与输入由覆盖层线程的命令驱动。
unsafe extern "system" fn overlay_wnd_proc(
    hwnd: HWND,
    msg: u32,
    _wparam: WPARAM,
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
        // 分辨率/DPI 变化后交换链内容需要重绘（几何重建由下一次 `show`/`ensure_window`
        // 负责）；先置位标志，再交给默认处理。
        WM_DISPLAYCHANGE | WM_DPICHANGED => {
            REDRAW_REQUESTED.store(true, Ordering::Release);
            // SAFETY: 默认处理；参数原样转发。
            unsafe { DefWindowProcW(hwnd, msg, _wparam, lparam) }
        }
        // 背景由 D3D 负责，禁止 GDI 擦除造成闪烁。
        WM_ERASEBKGND => LRESULT(1),
        // 阶段 01 的取消由 `OverlayCommand::Cancel` 驱动（Task 13 接入 Esc）；
        // 忽略 WM_CLOSE/Alt+F4，避免窗口在管理结构之外被销毁。
        WM_CLOSE => LRESULT(0),
        // SAFETY: 默认处理；参数原样转发。
        _ => unsafe { DefWindowProcW(hwnd, msg, _wparam, lparam) },
    }
}

/// UTF-16（NUL 结尾）缓冲区。
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}
