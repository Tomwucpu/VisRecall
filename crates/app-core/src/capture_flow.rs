//! 捕获编排与落盘：快捷键/托盘触发 → 记录来源 → 隐藏自有窗口 → 抓帧 → 覆盖层，
//! 以及覆盖层提交后的 PNG 编码、临时文件 → `rename` → 入库（design §5、§10.2）。
//!
//! 入库并发 `item_created` 之后，缩略图（`thumbs/<id>.png`）派发到有界缩略图 worker
//! （[`crate::thumbnail`]，全局队列 + 固定线程数），捕获循环不等它完成。
//!
//! 线程模型（design §3）：本模块的方法在 app-core 的捕获循环线程上同步执行；
//! 抓帧与窗口探针通过 trait 注入，覆盖层通过 [`OverlayPresenter`]（生产实现为
//! [`OverlayHandle`]）投递，便于用假实现测试失败分支。
//!
//! 会话互斥（review I1）：一个 [`CaptureCoordinator`] 同一时刻只允许一个捕获会话。
//! `start` 在会话进行中直接忽略；结束事件（`Committed`/`Cancelled`）只结束当前会话，
//! 因此旧会话的重复结束事件不会恢复第二个会话的窗口。
//!
//! 已知限制（阶段 01 不实现）：抓帧失败后的**实时模式**（透明覆盖层 + 提交时现场
//! 抓选区）降级为阶段 02；当前行为是重试一次后报 `CAPTURE_FAILED`，
//! `CaptureConfig.realtime_fallback` 保留但未生效（design §5 / §13 已记录降级）。

use std::fs;
use std::io::Write;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;

use image::codecs::png::PngEncoder;
use image::{ExtendedColorType, ImageEncoder};
use sha2::{Digest, Sha256};
use tracing::{error, info, warn};
use visrecall_capture::grab_gdi::is_blank_frame;
use visrecall_capture::{enumerate_displays, FrameGrabber, RawFrame, WindowProbe};
use visrecall_domain::capture::{CapturedImage, DisplayInfo, ForegroundInfo};
use visrecall_domain::item::{Item, ItemKind, NewItem};
use visrecall_domain::{new_item_id, now_millis, AppError};
use visrecall_library::files;
use visrecall_library::Library;
use visrecall_shell::overlay::OverlayHandle;
use visrecall_shell::{CaptureTrigger, OverlayEvent};

use crate::events::{AppEvent, CaptureFailedEvent, EventSink, ItemEvent};
use crate::thumbnail;

/// 开始捕获所需的最低可用空间（design §12：<2GB 拒绝新捕获，保留浏览）。
pub const MIN_FREE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// 库已用字节数的探测点。
///
/// 用闭包注入而不是直接调 `Library::usage()`：测试要在不造 50GB 数据的前提下覆盖
/// 「已用 ≥ 容量」这条分支（design §6.7）。
pub type UsageProbe = Arc<dyn Fn(&Library) -> Option<u64> + Send + Sync>;

/// 自有窗口（主窗口等）在抓帧期间隐藏、结束后恢复。
///
/// 生产实现是 `src-tauri` 的 `MainWindowVisibility`；测试用 [`NoopWindowVisibility`]。
/// 两条路径都必须可重复调用且安全：结束路径只依赖「恢复一次」的幂等性。
pub trait WindowVisibility: Send + Sync {
    fn hide_own_windows(&self);
    fn show_own_windows(&self);
}

/// 什么都不做的可见性实现；用于不涉及真实窗口的场景（测试、无 UI 路径）。
#[derive(Clone, Copy, Debug, Default)]
pub struct NoopWindowVisibility;

impl WindowVisibility for NoopWindowVisibility {
    fn hide_own_windows(&self) {}
    fn show_own_windows(&self) {}
}

/// 覆盖层投递抽象；生产实现是 [`OverlayHandle`]，测试可注入假实现。
pub trait OverlayPresenter: Send + Sync {
    fn show(
        &self,
        frames: Vec<RawFrame>,
        displays: Vec<DisplayInfo>,
        foreground: ForegroundInfo,
    ) -> Result<(), AppError>;
}

impl OverlayPresenter for OverlayHandle {
    fn show(
        &self,
        frames: Vec<RawFrame>,
        displays: Vec<DisplayInfo>,
        foreground: ForegroundInfo,
    ) -> Result<(), AppError> {
        OverlayHandle::show(self, frames, displays, foreground)
    }
}

/// 可用空间查询注入点（测试用它模拟磁盘将满）；生产实现是 [`files::free_space_bytes`]。
pub type FreeSpaceProbe = Arc<dyn Fn(&Path) -> Result<u64, AppError> + Send + Sync>;

/// 显示器枚举注入点（测试用它给出确定的显示器集合）；生产实现是 [`enumerate_displays`]。
///
/// 抓帧结果按 `display_id` 与这里返回的显示器配对，因此测试不能依赖真实桌面。
pub type DisplayProbe = Arc<dyn Fn() -> Result<Vec<DisplayInfo>, AppError> + Send + Sync>;

/// 一次抓帧里可用的帧/显示器配对，以及被丢弃的全黑显示器。
///
/// `frames` 与 `displays` 严格一一对应（覆盖层 `show` 的契约：帧按 `display_id` 找显示器）。
struct FrameSelection {
    frames: Vec<RawFrame>,
    displays: Vec<DisplayInfo>,
    /// 因全黑（疑似 DRM/显示器休眠）被丢弃的显示器 id。
    blank_displays: Vec<String>,
}

/// 按 `display_id` 把帧与显示器配对，丢弃全黑帧及其显示器条目（P1-R03、design §5）。
///
/// 部分显示器全黑（副屏休眠、DPMS、DRM 窗口独占某块屏）不再让整次捕获失败：
/// 只丢掉这些屏，其余屏继续显示覆盖层。**只有全部显示器都全黑**时调用方才会
/// 报 [`AppError::DrmProtected`]。
///
/// 丢弃帧时必须同步丢弃对应的显示器条目，否则覆盖层要么拿到没有显示器的帧
/// （`show` 报错），要么留下没有帧的显示器。保留顺序与输入一致：帧保持抓帧顺序、
/// 显示器保持枚举顺序；没有对应显示器的帧无法显示，同样丢弃（正常路径不会出现）。
///
/// 由于保留**全部**可用屏，主屏/鼠标指针所在屏只要可用就一定保留。
fn select_usable_frames(frames: Vec<RawFrame>, displays: Vec<DisplayInfo>) -> FrameSelection {
    let mut kept = Vec::with_capacity(frames.len());
    let mut blank_displays = Vec::new();

    for frame in frames {
        if !displays
            .iter()
            .any(|display| display.id == frame.display_id)
        {
            warn!(
                display_id = %frame.display_id,
                "frame has no matching display info; dropped"
            );
            continue;
        }
        if is_blank_frame(&frame) {
            warn!(
                display_id = %frame.display_id,
                "captured frame is all black; dropping this display (suspected DRM or a sleeping display)"
            );
            blank_displays.push(frame.display_id.clone());
            continue;
        }
        kept.push(frame);
    }

    let displays = displays
        .into_iter()
        .filter(|display| kept.iter().any(|frame| frame.display_id == display.id))
        .collect();

    FrameSelection {
        frames: kept,
        displays,
        blank_displays,
    }
}

/// 捕获编排：把一次 `CaptureRequest` 推进到「覆盖层显示冻结帧」，并在覆盖层
/// 提交后完成落盘与入库。
pub struct CaptureCoordinator {
    library: Arc<Library>,
    grabber: Arc<dyn FrameGrabber>,
    probe: Arc<dyn WindowProbe>,
    overlay: Arc<dyn OverlayPresenter>,
    visibility: Arc<dyn WindowVisibility>,
    sink: Arc<dyn EventSink>,
    free_space: FreeSpaceProbe,
    /// 库已用字节数；容量软配额用它判定是否拒绝新捕获。
    usage: UsageProbe,
    /// 迁移/待重启闸门；`None` 表示没有闸门（测试与阶段 01 路径）。
    ///
    /// 这里是**纵深防御**：主检查在捕获循环的事件分发处（覆盖 `CaptureRequest` 与
    /// `Drop` 两条写入口），但将来新增入口若绕过循环，这道检查仍会挡住。
    gate: Option<Arc<AtomicU8>>,
    /// 显示器枚举；测试注入固定的显示器集合，生产实现是 [`enumerate_displays`]。
    displays: DisplayProbe,
    /// 是否有会话正在进行（`start` 占位 → 覆盖层结束 / `start` 失败时清除）。
    ///
    /// 捕获循环是单线程的，标志用于挡住「覆盖层还开着时又按快捷键」的重入：
    /// 第二次 `start` 直接忽略，不再隐藏窗口、不再抓帧，也不重复投递覆盖层。
    /// 结束事件只结束**当前**会话（`swap(false)` 成功才算），所以旧会话的
    /// 迟到/重复结束事件不会在第二个会话里恢复窗口。
    session_active: AtomicBool,
}

impl CaptureCoordinator {
    pub fn new(
        library: Arc<Library>,
        grabber: Arc<dyn FrameGrabber>,
        probe: Arc<dyn WindowProbe>,
        overlay: OverlayHandle,
        visibility: Arc<dyn WindowVisibility>,
        sink: Arc<dyn EventSink>,
    ) -> Self {
        Self::with_overlay(library, grabber, probe, Arc::new(overlay), visibility, sink)
    }

    /// 与 [`CaptureCoordinator::new`] 相同，但覆盖层以 trait 对象注入（测试用）。
    pub fn with_overlay(
        library: Arc<Library>,
        grabber: Arc<dyn FrameGrabber>,
        probe: Arc<dyn WindowProbe>,
        overlay: Arc<dyn OverlayPresenter>,
        visibility: Arc<dyn WindowVisibility>,
        sink: Arc<dyn EventSink>,
    ) -> Self {
        Self {
            library,
            grabber,
            probe,
            overlay,
            visibility,
            sink,
            free_space: Arc::new(files::free_space_bytes),
            usage: Arc::new(|library| library.usage().ok().map(|usage| usage.used_bytes)),
            gate: None,
            displays: Arc::new(enumerate_displays),
            session_active: AtomicBool::new(false),
        }
    }

    /// 替换可用空间查询（测试专用）。
    #[doc(hidden)]
    pub fn with_free_space_probe(mut self, probe: FreeSpaceProbe) -> Self {
        self.free_space = probe;
        self
    }

    /// 接上迁移闸门（`AppCore::start_runtime` 里注入）。
    pub fn with_gate(mut self, gate: Arc<AtomicU8>) -> Self {
        self.gate = Some(gate);
        self
    }

    /// 替换用量探测（测试专用）。
    #[doc(hidden)]
    pub fn with_usage_probe(mut self, probe: UsageProbe) -> Self {
        self.usage = probe;
        self
    }

    /// 替换显示器枚举（测试专用，避免测试依赖真实显示器）。
    #[doc(hidden)]
    pub fn with_display_probe(mut self, probe: DisplayProbe) -> Self {
        self.displays = probe;
        self
    }

    /// 开始一次捕获：空间检查 → 前台窗口 → 隐藏自有窗口 + `DwmFlush` → 抓帧 →
    /// 按显示器丢弃全黑帧（只有全部全黑才整次失败）→ 覆盖层显示冻结帧。
    ///
    /// 已有会话进行中时直接忽略（返回 `Ok`）：不隐藏窗口、不抓帧、不重复投递
    /// 覆盖层，避免第二次快捷键把第一个会话的冻结帧/窗口状态搅乱。
    ///
    /// 失败时恢复自有窗口、清除会话标志并发 `capture_failed`；成功时窗口保持隐藏，
    /// 等 [`CaptureCoordinator::on_overlay_event`] 的结束路径再恢复。
    pub fn start(&self, trigger: CaptureTrigger) -> Result<(), AppError> {
        if self
            .session_active
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            info!(?trigger, "capture session already active; ignoring trigger");
            return Ok(());
        }
        self.run_session(trigger)
    }

    /// 会话标志已置位后的主流程。
    ///
    /// **每条失败分支都必须清除会话标志**（[`Self::abort_session`] 或空间不足时
    /// [`Self::clear_session`]），否则后续捕获会被重入保护永久忽略。
    fn run_session(&self, trigger: CaptureTrigger) -> Result<(), AppError> {
        info!(?trigger, "capture starting");

        // 迁移/待重启期间一律拒绝，且用 `BUSY` 而不是「内部错误」。
        // 判定与文案来自 `app::gate_rejection`：主检查在捕获循环的分发处，这里是
        // 纵深防御（新入口若绕过循环仍然被挡住）。
        if let Some(gate) = &self.gate {
            if let Some(err) = crate::app::gate_rejection(gate) {
                warn!(error = %err, "capture rejected by the relocation gate");
                self.clear_session();
                self.emit_failed(&err);
                return Err(err);
            }
        }

        if let Some(free) = self.check_free_space() {
            if free < MIN_FREE_BYTES {
                warn!(free_bytes = free, "free space below capture threshold");
                let err = AppError::StorageFull;
                // 未隐藏窗口：只清标志，不调用 `show_own_windows`。
                self.clear_session();
                self.emit_failed(&err);
                return Err(err);
            }
        }

        // 容量软配额：与磁盘剩余空间是两道独立的门。容量是「这个库的上限」，
        // 已用 ≥ 容量时拒绝新捕获，但**不自动删除任何素材**（P2-R07）。
        if let Some(used) = (self.usage)(&self.library) {
            let capacity = self.library.capacity_bytes();
            if used >= capacity {
                warn!(used, capacity, "library usage reached its capacity");
                let err = AppError::StorageFull;
                self.clear_session();
                self.emit_failed(&err);
                return Err(err);
            }
        }

        // 来源窗口只是元数据：识别失败仍允许捕获（design §5 失败分支）。
        let foreground = match self.probe.foreground() {
            Ok(foreground) => foreground,
            Err(err) => {
                warn!(error = %err, "foreground window unavailable; capturing without source");
                ForegroundInfo::default()
            }
        };

        self.visibility.hide_own_windows();
        if let Err(err) = visrecall_capture::dwm_flush() {
            // DWM 不可用时继续抓帧：可能抓到隐藏前的旧画面，但不阻断捕获。
            warn!(error = %err, "DwmFlush failed; grabbing without waiting for composition");
        }

        let displays = match (self.displays)() {
            Ok(displays) if !displays.is_empty() => displays,
            Ok(_) => {
                let err = AppError::CaptureFailed("no displays available".into());
                self.abort_session(&err);
                return Err(err);
            }
            Err(err) => {
                self.abort_session(&err);
                return Err(err);
            }
        };

        let frames = match self.grab_with_retry(&displays) {
            Ok(frames) if !frames.is_empty() => frames,
            Ok(_) => {
                let err = AppError::CaptureFailed("frame grabber returned no frames".into());
                self.abort_session(&err);
                return Err(err);
            }
            Err(err) => {
                self.abort_session(&err);
                return Err(err);
            }
        };

        // 全黑帧通常是 DRM 保护窗口或显示器休眠：只丢弃这一屏（P1-R03、design §5）。
        // 只有**全部**显示器都全黑时才整次失败，避免一块副屏休眠就毁掉整次捕获。
        let selection = select_usable_frames(frames, displays);

        if selection.frames.is_empty() {
            let err = AppError::DrmProtected;
            warn!(
                blank = %selection.blank_displays.join(", "),
                "no usable frames after dropping blank displays; suspected DRM protection"
            );
            self.abort_session(&err);
            return Err(err);
        }

        if !selection.blank_displays.is_empty() {
            // 部分屏全黑：提示一次被丢弃的显示器，但会话继续，其余屏幕照常显示覆盖层。
            let dropped = selection.blank_displays.join(", ");
            warn!(displays = %dropped, "blank displays dropped from this capture");
            self.sink.emit(AppEvent::CaptureFailed(CaptureFailedEvent {
                code: capture_failed_code(&AppError::DrmProtected).to_string(),
                message: format!("dropped blank displays (suspected DRM): {dropped}"),
            }));
        }

        if let Err(err) = self
            .overlay
            .show(selection.frames, selection.displays, foreground)
        {
            warn!(error = %err, "overlay show failed");
            self.abort_session(&err);
            return Err(err);
        }

        info!("frozen frames handed to the overlay");
        Ok(())
    }

    /// 处理覆盖层结果：提交则恢复窗口 + 落盘 + 入库 + `item_created`，取消则只恢复窗口。
    ///
    /// 只有**当前会话**的结束事件才生效（会话标志 `swap(false)` 成功）：迟到或重复的
    /// 旧会话事件会被忽略，因此不会恢复第二个会话正在隐藏的窗口。每个会话的恢复
    /// 只在结束时调用一次；`show_own_windows` 本身也要求幂等。
    pub fn on_overlay_event(&self, event: OverlayEvent) -> Result<(), AppError> {
        match event {
            OverlayEvent::Committed {
                image,
                global_rect,
                foreground,
            } => {
                if !self.session_is_active() {
                    warn!("committed overlay event without active capture session; ignoring");
                    return Ok(());
                }
                info!(
                    width = image.width,
                    height = image.height,
                    x = global_rect.x,
                    y = global_rect.y,
                    "overlay committed; persisting capture"
                );
                // 先恢复自有窗口再落盘：PNG 编码/fsync 期间主窗口不该继续隐藏。
                self.end_session();
                let result = persist_capture(image, foreground, &self.library);
                match result {
                    Ok(item) => {
                        self.sink.emit(AppEvent::ItemCreated(ItemEvent {
                            id: item.id.clone(),
                        }));
                        // 缩略图交给有界 worker（design §10.2）：捕获循环不等 PNG 解码/缩放。
                        self.spawn_thumbnail(&item);
                        Ok(())
                    }
                    Err(err) => {
                        warn!(error = %err, "failed to persist committed capture");
                        self.emit_failed(&err);
                        Err(err)
                    }
                }
            }
            OverlayEvent::Cancelled => {
                if self.end_session() {
                    info!("overlay cancelled; own windows restored");
                } else {
                    warn!("cancelled overlay event without active capture session; ignoring");
                }
                Ok(())
            }
        }
    }

    /// 兜底释放：清除会话标志并恢复自有窗口。
    ///
    /// 两处调用方共用：捕获线程某一步 panic（[`guard_session`]），以及覆盖层通道断开
    /// 等**正常退出**路径——不释放的话，主窗口会一直隐藏、会话标志也会残留。
    /// `show_own_windows` 幂等，即使会话已正常结束也不会重复显示窗口。
    pub fn release_windows(&self) {
        self.clear_session();
        self.visibility.show_own_windows();
    }

    /// 派发缩略图任务（`item_created` 之后调用，前端先拿到素材再收到状态更新）。
    ///
    /// 派发失败只影响这一张图：记录 error 并发 `capture_failed { THUMBNAIL_FAILED }`，
    /// 避免素材永久静默停在 `parsing`。
    fn spawn_thumbnail(&self, item: &Item) {
        if let Err(err) = thumbnail::spawn_thumbnail(
            Arc::clone(&self.library),
            Arc::clone(&self.sink),
            item.id.clone(),
            item.original_path.clone(),
        ) {
            error!(id = %item.id, error = %err, "failed to queue the thumbnail job");
            self.sink.emit(AppEvent::CaptureFailed(CaptureFailedEvent {
                code: thumbnail::THUMBNAIL_FAILED_CODE.to_string(),
                message: format!("failed to queue the thumbnail job: {err}"),
            }));
        }
    }

    fn session_is_active(&self) -> bool {
        self.session_active.load(Ordering::SeqCst)
    }

    /// 清除会话标志但不碰窗口（用于尚未隐藏过的失败分支，如空间不足）。
    fn clear_session(&self) {
        self.session_active.store(false, Ordering::SeqCst);
    }

    /// 结束当前会话并恢复自有窗口；返回是否由本次调用结束（`false` = 没有活动会话）。
    fn end_session(&self) -> bool {
        if self.session_active.swap(false, Ordering::SeqCst) {
            self.visibility.show_own_windows();
            true
        } else {
            false
        }
    }

    fn check_free_space(&self) -> Option<u64> {
        match (self.free_space)(self.library.root()) {
            Ok(bytes) => Some(bytes),
            Err(err) => {
                // 查询失败（例如刚被卸载的卷）不阻止捕获，落盘失败会再报错。
                warn!(error = %err, "free space check failed; continuing");
                None
            }
        }
    }

    /// 抓帧失败重试一次；仍失败返回 [`AppError::CaptureFailed`]。
    fn grab_with_retry(&self, displays: &[DisplayInfo]) -> Result<Vec<RawFrame>, AppError> {
        match self.grabber.grab(displays) {
            Ok(frames) => Ok(frames),
            Err(first) => {
                warn!(error = %first, "frame grab failed; retrying once");
                self.grabber.grab(displays).map_err(|second| {
                    warn!(error = %second, "frame grab retry failed");
                    AppError::CaptureFailed(format!("grab failed after retry: {second}"))
                })
            }
        }
    }

    /// 隐藏之后的失败路径：清除会话标志、恢复自有窗口（每会话一次）并发 `capture_failed`。
    fn abort_session(&self, err: &AppError) {
        self.end_session();
        self.emit_failed(err);
    }

    /// 把错误按 `capture_failed` 上报（码由 [`capture_failed_code`] 决定）。
    ///
    /// 公开是因为捕获循环的前置检查（迁移闸门拒绝）也走这条路径：错误码与文案
    /// 必须和会话内失败完全一致，否则前端要按两套码分支。
    pub fn emit_failed(&self, err: &AppError) {
        self.sink.emit(AppEvent::CaptureFailed(CaptureFailedEvent {
            code: capture_failed_code(err).to_string(),
            message: err.to_string(),
        }));
    }
}

/// 在捕获循环里执行一步；panic 时记录 error、结束会话并恢复自有窗口。
///
/// 返回 `None` 表示该步 panic 已被兜住（调用方继续处理后续事件），正常返回时是
/// `Some(值)`。通道断开等**正常退出**不在这里处理：调用方仍需显式 `break`，
/// 不会因为兜底而吞掉退出。
pub fn guard_session<R>(coordinator: &CaptureCoordinator, work: impl FnOnce() -> R) -> Option<R> {
    match std::panic::catch_unwind(AssertUnwindSafe(work)) {
        Ok(value) => Some(value),
        Err(payload) => {
            error!(
                panic = %panic_message(payload.as_ref()),
                "capture step panicked; own windows restored"
            );
            coordinator.release_windows();
            None
        }
    }
}

/// 从 `catch_unwind` 的载荷里取可读消息（`&str` / `String` / 其他）。
fn panic_message(payload: &(dyn std::any::Any + Send)) -> &str {
    if let Some(message) = payload.downcast_ref::<&str>() {
        message
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.as_str()
    } else {
        "non-string panic payload"
    }
}

/// [`AppError`] → `capture_failed` 的稳定 `code`（与 IPC 错误码同一套命名）。
fn capture_failed_code(err: &AppError) -> &'static str {
    match err {
        AppError::ConfigMissing => "CONFIG_MISSING",
        AppError::InvalidInput(_) => "INVALID_INPUT",
        AppError::NotFound => "NOT_FOUND",
        AppError::StorageFull => "STORAGE_FULL",
        AppError::CaptureFailed(_) => "CAPTURE_FAILED",
        AppError::CaptureCancelled => "CAPTURE_CANCELLED",
        AppError::DrmProtected => "DRM_PROTECTED",
        AppError::FileMissing(_) => "FILE_MISSING",
        AppError::ClipboardBusy => "CLIPBOARD_BUSY",
        AppError::ClipboardFailed(_) => "CLIPBOARD_FAILED",
        // 迁移/清理闸门拒绝捕获时必须是 BUSY，否则前端会显示成「内部错误」。
        AppError::Busy(_) => "BUSY",
        AppError::Io(_) => "IO",
        AppError::Database(_) => "DATABASE",
        AppError::Internal(_) => "INTERNAL",
        AppError::ModelNotReady(_) => "MODEL_NOT_READY",
        AppError::Provider(_) => "PROVIDER",
        AppError::ProviderConfig(_) => "PROVIDER_CONFIG",
        AppError::SchemaViolation(_) => "SCHEMA_VIOLATION",
    }
}

/// 把已回读的选区图像落盘并入库，返回状态为 `parsing` 的素材。
///
/// 流程（design §10.2）：BGRA→RGBA（alpha 固定 255）PNG 编码 → sha256 →
/// `tmp/<id>.tmp` 写入 + `sync_all` → 建目录 → `rename` 到
/// `originals/images/YYYY/MM/<id>.png` → `Library::insert_item`。
/// 任一步失败清理临时文件；数据库只存相对库根的路径。入库失败时删除已 `rename`
/// 的目标文件，避免留下没有 DB 记录的孤儿 PNG。
pub fn persist_capture(
    image: CapturedImage,
    foreground: ForegroundInfo,
    library: &Library,
) -> Result<Item, AppError> {
    let width = image.width;
    let height = image.height;
    if width == 0 || height == 0 {
        return Err(AppError::InvalidInput(format!(
            "captured image has empty size: {width}x{height}"
        )));
    }
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| {
            AppError::InvalidInput(format!("captured image size overflow: {width}x{height}"))
        })?;
    if image.bgra.len() != expected {
        return Err(AppError::InvalidInput(format!(
            "captured image buffer mismatch: {} bytes for {width}x{height}",
            image.bgra.len()
        )));
    }

    let png = encode_png(&image)?;
    let content_hash = sha256_hex(&png);

    let id = new_item_id();
    let root = library.root();
    let (relative_path, target) = image_target(root, &id);
    write_png(root, &id, &target, &png)?;

    let item = library
        .insert_item_with_id(
            id,
            NewItem {
                kind: ItemKind::Image,
                captured_at: now_millis(),
                source_app: non_empty(foreground.process_name),
                source_title: non_empty(foreground.title),
                source_url: None,
                width: Some(width),
                height: Some(height),
                char_count: None,
                original_path: relative_path,
                content_hash,
            },
        )
        // 入库失败时 PNG 已在最终位置：删掉它，避免留下孤儿文件。
        .map_err(|err| remove_orphan(&target, err))?;

    info!(
        id = %item.id,
        path = %item.original_path,
        width,
        height,
        "captured image persisted"
    );
    Ok(item)
}

/// 入库失败后删除已 `rename` 的目标文件，返回原错误。
///
/// 删除失败只记录 warn：用户数据目录可能被占用，但捕获流程的失败原因以数据库
/// 错误为准，不因清理失败改变错误码。
fn remove_orphan(target: &Path, err: AppError) -> AppError {
    match fs::remove_file(target) {
        Ok(()) => warn!(
            path = %target.display(),
            error = %err,
            "removed orphan image after database insert failure"
        ),
        Err(remove_err) if remove_err.kind() == std::io::ErrorKind::NotFound => {}
        Err(remove_err) => warn!(
            path = %target.display(),
            error = %remove_err,
            "failed to remove orphan image after database insert failure"
        ),
    }
    err
}

/// BGRA8 → RGBA8（alpha 固定 255，透明黑在 PNG 里应表现为不透明黑）→ PNG 编码。
fn encode_png(image: &CapturedImage) -> Result<Vec<u8>, AppError> {
    let mut rgba = Vec::with_capacity(image.bgra.len());
    for px in image.bgra.chunks_exact(4) {
        rgba.extend_from_slice(&[px[2], px[1], px[0], 0xFF]);
    }
    encode_rgba_png(image.width, image.height, &rgba)
}

/// RGBA8 → PNG（导入流程复用：BGRA 图像与文本/链接不经过这里）。
pub(crate) fn encode_rgba_png(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, AppError> {
    let mut png = Vec::new();
    PngEncoder::new(&mut png)
        .write_image(rgba, width, height, ExtendedColorType::Rgba8)
        .map_err(|err| AppError::Internal(format!("png encode failed: {err}")))?;
    Ok(png)
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// 目标路径：绝对路径 + 相对库根的路径（`originals/images/YYYY/MM/<id>.png`）。
///
/// 日期取本地时区（用户按本地月份翻找素材）；读不到本地偏移时回退 UTC。
pub(crate) fn image_target(root: &Path, id: &str) -> (String, PathBuf) {
    let now = time::OffsetDateTime::now_local().unwrap_or_else(|err| {
        warn!(error = %err, "local time unavailable; falling back to UTC for the image path");
        time::OffsetDateTime::now_utc()
    });
    let month: u8 = now.month().into();
    let relative = format!("originals/images/{:04}/{:02}/{id}.png", now.year(), month);
    (relative.clone(), root.join(relative))
}

/// `tmp/<id>.tmp` 写入 + `sync_all` → 建目标目录 → `rename`；任一步失败清理临时文件。
fn write_png(root: &Path, id: &str, target: &Path, png: &[u8]) -> Result<(), AppError> {
    write_bytes_atomic(root, id, target, png)
}

/// 原子写入库内文件（design §10.2）：`tmp/<tmp_id>.tmp` → `sync_all` → `rename`。
///
/// 临时文件固定在 `<root>/tmp/`，失败时清理；导入流程的图片与文本都走这里。
pub(crate) fn write_bytes_atomic(
    root: &Path,
    tmp_id: &str,
    target: &Path,
    bytes: &[u8],
) -> Result<(), AppError> {
    write_bytes_atomic_in(&root.join("tmp"), tmp_id, target, bytes)
}

/// 同 [`write_bytes_atomic`]，但临时目录由调用方指定。
///
/// 缩略图写在 `thumbs/` 旁边，避免与捕获的 `tmp/` 启动清理策略互相影响；
/// 临时文件仍在同一卷上，`rename` 保持原子。
pub(crate) fn write_bytes_atomic_in(
    tmp_dir: &Path,
    tmp_id: &str,
    target: &Path,
    bytes: &[u8],
) -> Result<(), AppError> {
    fs::create_dir_all(tmp_dir)
        .map_err(|err| AppError::Io(format!("create dir {}: {err}", tmp_dir.display())))?;
    let tmp = tmp_dir.join(format!("{tmp_id}.tmp"));

    let write = || -> Result<(), AppError> {
        let mut file = fs::File::create(&tmp)
            .map_err(|err| AppError::Io(format!("create {}: {err}", tmp.display())))?;
        file.write_all(bytes)
            .map_err(|err| AppError::Io(format!("write {}: {err}", tmp.display())))?;
        file.sync_all()
            .map_err(|err| AppError::Io(format!("sync {}: {err}", tmp.display())))?;

        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .map_err(|err| AppError::Io(format!("create dir {}: {err}", parent.display())))?;
        }
        fs::rename(&tmp, target)
            .map_err(|err| AppError::Io(format!("rename {}: {err}", target.display())))
    };

    if let Err(err) = write() {
        // 失败不留下孤立临时文件（design §12「落盘失败清理临时文件」）。
        let _ = fs::remove_file(&tmp);
        return Err(err);
    }
    Ok(())
}

/// 空字符串转 `None`，避免把「未知来源」写成空串落库。
fn non_empty(value: String) -> Option<String> {
    if value.trim().is_empty() {
        None
    } else {
        Some(value)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    use visrecall_domain::geometry::PhysicalRect;
    use visrecall_domain::ItemStatus;

    use super::*;
    use crate::events::RecordingSink;

    /// 两台固定的假显示器（主屏 + 副屏）：测试不依赖真实桌面。
    fn fake_displays() -> Vec<DisplayInfo> {
        vec![
            DisplayInfo {
                id: "display-1".into(),
                bounds: PhysicalRect::new(0, 0, 1920, 1080),
                work_area: PhysicalRect::new(0, 0, 1920, 1040),
                dpi: 96,
                primary: true,
            },
            DisplayInfo {
                id: "display-2".into(),
                bounds: PhysicalRect::new(1920, 0, 1280, 1024),
                work_area: PhysicalRect::new(1920, 0, 1280, 984),
                dpi: 96,
                primary: false,
            },
        ]
    }

    /// 1×1 的帧；`blank` 时 BGRA 四个通道全为 0（DRM 保护窗口的典型结果）。
    fn frame_for(display_id: &str, blank: bool) -> RawFrame {
        let pixel = if blank {
            [0, 0, 0, 0]
        } else {
            [0x7F, 0x7F, 0x7F, 0xFF]
        };
        RawFrame {
            display_id: display_id.to_string(),
            width: 1,
            height: 1,
            stride: 4,
            bgra: pixel.to_vec(),
        }
    }

    /// 抓帧假实现：按传入的显示器逐台生成帧，可配置「失败」与「哪些显示器全黑」。
    struct FakeGrabber {
        calls: AtomicUsize,
        fail: AtomicBool,
        /// `true` 时所有显示器都返回全黑帧。
        blank: bool,
        /// 只让这些 display id 的帧全黑（用于部分屏 DRM/休眠的场景）。
        blank_ids: Vec<String>,
    }

    impl FakeGrabber {
        fn new(fail: bool, blank: bool) -> Arc<Self> {
            Arc::new(Self {
                calls: AtomicUsize::new(0),
                fail: AtomicBool::new(fail),
                blank,
                blank_ids: Vec::new(),
            })
        }

        /// 只有 `ids` 里的显示器返回全黑帧，其余正常。
        fn with_blank_ids(fail: bool, ids: &[&str]) -> Arc<Self> {
            Arc::new(Self {
                calls: AtomicUsize::new(0),
                fail: AtomicBool::new(fail),
                blank: false,
                blank_ids: ids.iter().map(|id| (*id).to_string()).collect(),
            })
        }

        fn set_fail(&self, fail: bool) {
            self.fail.store(fail, Ordering::SeqCst);
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl FrameGrabber for FakeGrabber {
        fn grab(&self, displays: &[DisplayInfo]) -> Result<Vec<RawFrame>, AppError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail.load(Ordering::SeqCst) {
                return Err(AppError::CaptureFailed("fake grab failure".into()));
            }
            Ok(displays
                .iter()
                .map(|display| {
                    let blank = self.blank
                        || self
                            .blank_ids
                            .iter()
                            .any(|id| id.as_str() == display.id.as_str());
                    frame_for(&display.id, blank)
                })
                .collect())
        }
    }

    /// 窗口探针假实现：不触碰真实桌面。
    struct FakeProbe;

    impl WindowProbe for FakeProbe {
        fn foreground(&self) -> Result<ForegroundInfo, AppError> {
            Ok(ForegroundInfo {
                hwnd: 0,
                pid: 1,
                process_name: "chrome.exe".into(),
                display_name: "Chrome".into(),
                title: "Test".into(),
                bounds: PhysicalRect::new(0, 0, 1, 1),
            })
        }

        fn enumerate(&self) -> Result<Vec<visrecall_domain::WindowInfo>, AppError> {
            Ok(Vec::new())
        }

        fn hit_chain(
            &self,
            _point: visrecall_domain::PhysicalPoint,
        ) -> Result<Vec<visrecall_domain::WindowInfo>, AppError> {
            Ok(Vec::new())
        }

        fn hit_chain_excluding(
            &self,
            _point: visrecall_domain::PhysicalPoint,
            _excluded_pid: u32,
        ) -> Result<Vec<visrecall_domain::WindowInfo>, AppError> {
            Ok(Vec::new())
        }
    }

    /// 覆盖层假实现：只在成功时记录一次 `show`，并保留最近一次的帧/显示器组合。
    struct FakeOverlay {
        shows: AtomicUsize,
        fail: AtomicBool,
        /// 最近一次成功 `show` 的（帧 display id, 显示器 id），用于校验一一对应契约。
        last: Mutex<Option<(Vec<String>, Vec<String>)>>,
    }

    impl FakeOverlay {
        fn new(fail: bool) -> Arc<Self> {
            Arc::new(Self {
                shows: AtomicUsize::new(0),
                fail: AtomicBool::new(fail),
                last: Mutex::new(None),
            })
        }

        fn set_fail(&self, fail: bool) {
            self.fail.store(fail, Ordering::SeqCst);
        }

        fn shows(&self) -> usize {
            self.shows.load(Ordering::SeqCst)
        }

        /// 最近一次 `show` 收到的（帧 display id, 显示器 id）。
        fn last_pair(&self) -> Option<(Vec<String>, Vec<String>)> {
            self.last
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .clone()
        }
    }

    impl OverlayPresenter for FakeOverlay {
        fn show(
            &self,
            frames: Vec<RawFrame>,
            displays: Vec<DisplayInfo>,
            _foreground: ForegroundInfo,
        ) -> Result<(), AppError> {
            if self.fail.load(Ordering::SeqCst) {
                return Err(AppError::Internal("fake overlay failure".into()));
            }
            let pair = (
                frames
                    .iter()
                    .map(|frame| frame.display_id.clone())
                    .collect(),
                displays.iter().map(|display| display.id.clone()).collect(),
            );
            *self
                .last
                .lock()
                .unwrap_or_else(|poison| poison.into_inner()) = Some(pair);
            self.shows.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    /// 可见性假实现：记录隐藏/恢复次数。
    #[derive(Default)]
    struct FakeVisibility {
        hides: AtomicUsize,
        shows: AtomicUsize,
    }

    impl FakeVisibility {
        fn hides(&self) -> usize {
            self.hides.load(Ordering::SeqCst)
        }

        fn shows(&self) -> usize {
            self.shows.load(Ordering::SeqCst)
        }
    }

    impl WindowVisibility for FakeVisibility {
        fn hide_own_windows(&self) {
            self.hides.fetch_add(1, Ordering::SeqCst);
        }

        fn show_own_windows(&self) {
            self.shows.fetch_add(1, Ordering::SeqCst);
        }
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        coordinator: CaptureCoordinator,
        grabber: Arc<FakeGrabber>,
        overlay: Arc<FakeOverlay>,
        visibility: Arc<FakeVisibility>,
        sink: Arc<RecordingSink>,
    }

    fn fixture(grabber: Arc<FakeGrabber>, overlay: Arc<FakeOverlay>, free: u64) -> Fixture {
        fixture_with_usage(grabber, overlay, free, 0)
    }

    /// 与 [`fixture`] 相同，但注入固定的「库已用字节数」。
    fn fixture_with_usage(
        grabber: Arc<FakeGrabber>,
        overlay: Arc<FakeOverlay>,
        free: u64,
        used: u64,
    ) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let library = Arc::new(Library::open(dir.path().to_path_buf()).unwrap());
        let visibility = Arc::new(FakeVisibility::default());
        let sink = Arc::new(RecordingSink::default());
        let coordinator = CaptureCoordinator::with_overlay(
            library,
            grabber.clone(),
            Arc::new(FakeProbe),
            overlay.clone(),
            visibility.clone(),
            sink.clone(),
        )
        .with_free_space_probe(Arc::new(move |_path: &Path| Ok(free)))
        .with_display_probe(Arc::new(|| Ok(fake_displays())))
        // 默认用例注入 0 字节用量：容量门禁只在新用例里生效。
        .with_usage_probe(Arc::new(move |_library: &Library| Some(used)));
        Fixture {
            _dir: dir,
            coordinator,
            grabber,
            overlay,
            visibility,
            sink,
        }
    }

    fn event_codes(sink: &RecordingSink) -> Vec<(String, String)> {
        sink.events()
            .into_iter()
            .filter_map(|event| match event {
                AppEvent::CaptureFailed(payload) => Some((payload.code, payload.message)),
                _ => None,
            })
            .collect()
    }

    /// 容量软配额：注入「已用 ≥ 容量」时同样拒绝，且不进入抓帧（P2-R07）。
    #[test]
    fn usage_at_capacity_rejects_capture_before_hiding() {
        // 默认容量 50 GiB；`>=` 即拒绝（等于容量也算满）。
        const CAPACITY: u64 = 50 * 1024 * 1024 * 1024;

        for used in [CAPACITY, CAPACITY + 1] {
            let fixture = fixture_with_usage(
                FakeGrabber::new(false, false),
                FakeOverlay::new(false),
                MIN_FREE_BYTES,
                used,
            );
            let err = fixture
                .coordinator
                .start(CaptureTrigger::Hotkey)
                .unwrap_err();
            assert!(matches!(err, AppError::StorageFull), "used {used}");
            assert_eq!(fixture.grabber.calls(), 0);
            assert_eq!(fixture.visibility.hides(), 0);
            assert_eq!(
                event_codes(&fixture.sink),
                vec![("STORAGE_FULL".to_string(), "storage full".to_string())]
            );
        }
    }

    /// 用量低于容量时照常进入抓帧，说明门禁没有把正常路径挡掉。
    #[test]
    fn usage_below_capacity_allows_capture() {
        let fixture = fixture_with_usage(
            FakeGrabber::new(false, false),
            FakeOverlay::new(false),
            MIN_FREE_BYTES,
            1024,
        );

        fixture.coordinator.start(CaptureTrigger::Hotkey).unwrap();
        assert_eq!(fixture.grabber.calls(), 1);
    }

    #[test]
    fn storage_full_rejects_capture_before_hiding() {
        let fixture = fixture(
            FakeGrabber::new(false, false),
            FakeOverlay::new(false),
            MIN_FREE_BYTES - 1,
        );

        let err = fixture
            .coordinator
            .start(CaptureTrigger::Hotkey)
            .unwrap_err();
        assert!(matches!(err, AppError::StorageFull));
        assert_eq!(fixture.grabber.calls(), 0);
        assert_eq!(fixture.visibility.hides(), 0);
        assert_eq!(fixture.visibility.shows(), 0);
        assert_eq!(fixture.overlay.shows(), 0);

        // 空间不足也要清除会话标志：第二次触发仍走失败分支而不是被当成重入忽略。
        let err = fixture
            .coordinator
            .start(CaptureTrigger::Hotkey)
            .unwrap_err();
        assert!(matches!(err, AppError::StorageFull));

        let codes = event_codes(&fixture.sink);
        assert_eq!(codes.len(), 2);
        assert_eq!(codes[0].0, "STORAGE_FULL");
        assert_eq!(codes[1].0, "STORAGE_FULL");
    }

    #[test]
    fn blank_frame_is_reported_as_drm_and_restores_windows() {
        let fixture = fixture(
            FakeGrabber::new(false, true),
            FakeOverlay::new(false),
            MIN_FREE_BYTES,
        );

        let err = fixture
            .coordinator
            .start(CaptureTrigger::Hotkey)
            .unwrap_err();
        assert!(matches!(err, AppError::DrmProtected));
        assert_eq!(fixture.grabber.calls(), 1);
        assert_eq!(fixture.visibility.hides(), 1);
        assert_eq!(fixture.visibility.shows(), 1);
        assert_eq!(fixture.overlay.shows(), 0);

        let codes = event_codes(&fixture.sink);
        assert_eq!(codes.len(), 1);
        assert_eq!(codes[0].0, "DRM_PROTECTED");
    }

    /// I-2：部分显示器全黑（副屏休眠/DPMS）只丢弃该屏，其余屏继续显示覆盖层。
    #[test]
    fn partial_blank_display_is_dropped_and_the_overlay_still_shows() {
        let fixture = fixture(
            FakeGrabber::with_blank_ids(false, &["display-2"]),
            FakeOverlay::new(false),
            MIN_FREE_BYTES,
        );

        fixture
            .coordinator
            .start(CaptureTrigger::Hotkey)
            .expect("capture continues with the usable display");

        // 帧与显示器一一对应，只保留可用屏（主屏）。
        let (frame_ids, display_ids) = fixture.overlay.last_pair().expect("overlay shown");
        assert_eq!(frame_ids, vec!["display-1"]);
        assert_eq!(display_ids, vec!["display-1"]);

        // 会话仍在进行：窗口保持隐藏，等覆盖层结束再恢复一次。
        assert_eq!(fixture.visibility.shows(), 0);
        fixture
            .coordinator
            .on_overlay_event(OverlayEvent::Cancelled)
            .unwrap();
        assert_eq!(fixture.visibility.shows(), 1);

        // 只提示一次被丢弃的屏。
        let codes = event_codes(&fixture.sink);
        assert_eq!(codes.len(), 1, "exactly one DRM notice: {codes:?}");
        assert_eq!(codes[0].0, "DRM_PROTECTED");
        assert!(
            codes[0].1.contains("display-2"),
            "notice names the dropped display: {}",
            codes[0].1
        );
    }

    /// I-2：主屏全黑、副屏可用时保留副屏（不能因为丢了主屏就整次失败）。
    #[test]
    fn blank_primary_display_keeps_the_usable_secondary() {
        let fixture = fixture(
            FakeGrabber::with_blank_ids(false, &["display-1"]),
            FakeOverlay::new(false),
            MIN_FREE_BYTES,
        );

        fixture.coordinator.start(CaptureTrigger::Hotkey).unwrap();
        let (frame_ids, display_ids) = fixture.overlay.last_pair().expect("overlay shown");
        assert_eq!(frame_ids, vec!["display-2"]);
        assert_eq!(display_ids, vec!["display-2"]);
        assert_eq!(event_codes(&fixture.sink)[0].0, "DRM_PROTECTED");
    }

    /// I-2：全部显示器全黑才整次失败：不发覆盖层、恢复窗口、只报一次 DRM。
    #[test]
    fn all_blank_displays_report_drm_without_showing_the_overlay() {
        let fixture = fixture(
            FakeGrabber::new(false, true),
            FakeOverlay::new(false),
            MIN_FREE_BYTES,
        );

        let err = fixture
            .coordinator
            .start(CaptureTrigger::Hotkey)
            .unwrap_err();
        assert!(matches!(err, AppError::DrmProtected));
        assert_eq!(fixture.overlay.shows(), 0);
        assert_eq!(fixture.visibility.shows(), 1);

        let codes = event_codes(&fixture.sink);
        assert_eq!(codes.len(), 1);
        assert_eq!(codes[0].0, "DRM_PROTECTED");

        // 会话标志已清除：可以立刻重新开始（这一次被下一次抓帧覆盖写入）。
        fixture
            .coordinator
            .start(CaptureTrigger::Hotkey)
            .unwrap_err();
        assert_eq!(fixture.grabber.calls(), 2);
    }

    /// I-2：配对只保留可用屏；丢弃全黑屏时 display 条目同步丢弃、元数据保持原值。
    #[test]
    fn select_usable_frames_keeps_pairs_and_drops_blank_displays() {
        let frames = vec![
            frame_for("display-2", false),
            frame_for("display-1", true),
            // 没有对应显示器的帧无法显示，同样丢弃。
            frame_for("ghost", false),
        ];
        let selection = select_usable_frames(frames, fake_displays());

        assert_eq!(selection.frames.len(), 1);
        assert_eq!(selection.frames[0].display_id, "display-2");
        assert_eq!(selection.displays.len(), 1);
        assert_eq!(selection.displays[0].id, "display-2");
        assert_eq!(
            selection.displays[0].bounds,
            PhysicalRect::new(1920, 0, 1280, 1024),
            "display metadata must be carried over unchanged"
        );
        assert!(!selection.displays[0].primary);
        assert_eq!(selection.blank_displays, vec!["display-1"]);

        // 全部全黑：没有可用帧/显示器，但仍报告被丢弃的屏。
        let all_blank = select_usable_frames(
            vec![frame_for("display-1", true), frame_for("display-2", true)],
            fake_displays(),
        );
        assert!(all_blank.frames.is_empty());
        assert!(all_blank.displays.is_empty());
        assert_eq!(all_blank.blank_displays, vec!["display-1", "display-2"]);
    }

    #[test]
    fn grab_failure_retries_once_then_reports_capture_failed() {
        let fixture = fixture(
            FakeGrabber::new(true, false),
            FakeOverlay::new(false),
            MIN_FREE_BYTES,
        );

        let err = fixture
            .coordinator
            .start(CaptureTrigger::Hotkey)
            .unwrap_err();
        assert!(matches!(err, AppError::CaptureFailed(_)));
        assert_eq!(fixture.grabber.calls(), 2);
        assert_eq!(fixture.visibility.shows(), 1);
        assert_eq!(fixture.overlay.shows(), 0);

        let codes = event_codes(&fixture.sink);
        assert_eq!(codes.len(), 1);
        assert_eq!(codes[0].0, "CAPTURE_FAILED");
    }

    #[test]
    fn overlay_show_failure_restores_windows_and_reports() {
        let fixture = fixture(
            FakeGrabber::new(false, false),
            FakeOverlay::new(true),
            MIN_FREE_BYTES,
        );

        let err = fixture
            .coordinator
            .start(CaptureTrigger::Hotkey)
            .unwrap_err();
        assert!(matches!(err, AppError::Internal(_)));
        assert_eq!(fixture.visibility.shows(), 1);
        assert_eq!(fixture.overlay.shows(), 0);

        let codes = event_codes(&fixture.sink);
        assert_eq!(codes.len(), 1);
        assert_eq!(codes[0].0, "INTERNAL");
    }

    #[test]
    fn committed_event_persists_item_and_emits_item_created() {
        let fixture = fixture(
            FakeGrabber::new(false, false),
            FakeOverlay::new(false),
            MIN_FREE_BYTES,
        );
        fixture
            .coordinator
            .start(CaptureTrigger::Hotkey)
            .expect("start session");

        let image = CapturedImage {
            width: 2,
            height: 1,
            bgra: vec![0, 0, 255, 255, 0, 255, 0, 255],
        };
        let foreground = FakeProbe.foreground().unwrap();
        fixture
            .coordinator
            .on_overlay_event(OverlayEvent::Committed {
                image,
                global_rect: PhysicalRect::new(10, 20, 2, 1),
                foreground,
            })
            .unwrap();

        assert_eq!(fixture.visibility.shows(), 1);
        let events = fixture.sink.events();
        assert!(!events.is_empty());
        // 缩略图在 `item_created` 之后异步生成：首个事件必须是 item_created。
        assert_eq!(events[0].name(), "item_created");

        let library = fixture.coordinator.library.clone();
        let items = library.list_items(None, 10).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].width, Some(2));
        assert_eq!(items[0].height, Some(1));
        assert!(items[0].original_path.starts_with("originals/images/"));
        assert!(Path::new(&items[0].original_path).is_relative());

        // 缩略图线程在 5s 内把素材转 `ready` 并写入 `thumbs/<id>.png`。
        let id = items[0].id.clone();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let stored = loop {
            let stored = library.get_item(&id).unwrap().unwrap();
            if stored.status == ItemStatus::Ready {
                break stored;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "capture thumbnail was not generated within 5s"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        let thumb = format!("thumbs/{id}.png");
        assert_eq!(stored.thumb_path.as_deref(), Some(thumb.as_str()));
        assert!(library.root().join(&thumb).exists());

        // 会话结束后重复的结束事件被忽略：不再恢复窗口、不再落盘。
        fixture
            .coordinator
            .on_overlay_event(OverlayEvent::Cancelled)
            .unwrap();
        assert_eq!(fixture.visibility.shows(), 1);
        assert_eq!(library.list_items(None, 10).unwrap().len(), 1);
    }

    #[test]
    fn cancelled_event_restores_windows_once() {
        let fixture = fixture(
            FakeGrabber::new(false, false),
            FakeOverlay::new(false),
            MIN_FREE_BYTES,
        );
        fixture
            .coordinator
            .start(CaptureTrigger::Hotkey)
            .expect("start session");

        fixture
            .coordinator
            .on_overlay_event(OverlayEvent::Cancelled)
            .unwrap();
        assert_eq!(fixture.visibility.shows(), 1);
        assert!(fixture.sink.events().is_empty());

        // 重复 `Cancelled` 属于同一会话的迟到事件：不再恢复第二次。
        fixture
            .coordinator
            .on_overlay_event(OverlayEvent::Cancelled)
            .unwrap();
        assert_eq!(fixture.visibility.shows(), 1);
        assert!(fixture.sink.events().is_empty());
    }

    #[test]
    fn cancelled_session_adds_no_item() {
        // Esc 取消不产生记录：items 行数只随成功提交增长（报告统计口径的证据）。
        let fixture = fixture(
            FakeGrabber::new(false, false),
            FakeOverlay::new(false),
            MIN_FREE_BYTES,
        );
        fixture.coordinator.start(CaptureTrigger::Hotkey).unwrap();
        fixture
            .coordinator
            .on_overlay_event(OverlayEvent::Cancelled)
            .unwrap();

        let library = fixture.coordinator.library.clone();
        assert!(library.list_items(None, 10).unwrap().is_empty());
        assert!(fixture.sink.events().is_empty());
    }

    /// I1：会话进行中再次触发必须被忽略（不隐藏、不抓帧、不投递覆盖层）。
    #[test]
    fn second_start_while_session_active_is_ignored() {
        let fixture = fixture(
            FakeGrabber::new(false, false),
            FakeOverlay::new(false),
            MIN_FREE_BYTES,
        );

        fixture.coordinator.start(CaptureTrigger::Hotkey).unwrap();
        let ignored = fixture.coordinator.start(CaptureTrigger::BallClick);
        assert!(
            ignored.is_ok(),
            "second trigger must be ignored, got {ignored:?}"
        );

        assert_eq!(fixture.visibility.hides(), 1);
        assert_eq!(fixture.grabber.calls(), 1);
        assert_eq!(fixture.overlay.shows(), 1);
        assert!(fixture.sink.events().is_empty());

        // 会话结束后可以开新会话。
        fixture
            .coordinator
            .on_overlay_event(OverlayEvent::Cancelled)
            .unwrap();
        assert_eq!(fixture.visibility.shows(), 1);
        fixture.coordinator.start(CaptureTrigger::Hotkey).unwrap();
        assert_eq!(fixture.visibility.hides(), 2);
        assert_eq!(fixture.overlay.shows(), 2);
    }

    /// I1：没有活动会话时的结束事件（旧会话迟到/重复事件）不得恢复窗口或落盘。
    #[test]
    fn terminal_events_without_active_session_are_ignored() {
        let fixture = fixture(
            FakeGrabber::new(false, false),
            FakeOverlay::new(false),
            MIN_FREE_BYTES,
        );

        fixture
            .coordinator
            .on_overlay_event(OverlayEvent::Cancelled)
            .unwrap();
        assert_eq!(fixture.visibility.shows(), 0);

        let image = CapturedImage {
            width: 2,
            height: 1,
            bgra: vec![0, 0, 255, 255, 0, 255, 0, 255],
        };
        fixture
            .coordinator
            .on_overlay_event(OverlayEvent::Committed {
                image,
                global_rect: PhysicalRect::new(0, 0, 2, 1),
                foreground: FakeProbe.foreground().unwrap(),
            })
            .unwrap();
        assert_eq!(fixture.visibility.shows(), 0);
        assert!(fixture.sink.events().is_empty());

        let library = fixture.coordinator.library.clone();
        assert!(library.list_items(None, 10).unwrap().is_empty());
    }

    /// I1：失败路径必须清除会话标志，否则捕获功能会被重入保护永久挡住。
    #[test]
    fn failed_start_clears_session_flag() {
        let grabber = FakeGrabber::new(true, false);
        let fixture = fixture(grabber.clone(), FakeOverlay::new(false), MIN_FREE_BYTES);

        let err = fixture
            .coordinator
            .start(CaptureTrigger::Hotkey)
            .unwrap_err();
        assert!(matches!(err, AppError::CaptureFailed(_)));
        assert_eq!(fixture.visibility.shows(), 1);

        // 抓帧恢复后能再次开始会话（标志已清）。
        grabber.set_fail(false);
        fixture.coordinator.start(CaptureTrigger::Hotkey).unwrap();
        assert_eq!(fixture.overlay.shows(), 1);
        assert_eq!(fixture.visibility.hides(), 2);
        assert_eq!(fixture.visibility.shows(), 1);
    }

    /// 覆盖层 show 失败同样清除标志。
    #[test]
    fn overlay_failure_clears_session_flag() {
        let overlay = FakeOverlay::new(true);
        let fixture = fixture(
            FakeGrabber::new(false, false),
            overlay.clone(),
            MIN_FREE_BYTES,
        );

        fixture
            .coordinator
            .start(CaptureTrigger::Hotkey)
            .unwrap_err();
        overlay.set_fail(false);
        fixture.coordinator.start(CaptureTrigger::Hotkey).unwrap();
        assert_eq!(overlay.shows(), 1);
    }

    /// Minor：捕获线程的 panic 兜底恢复窗口并清会话，正常返回值不受影响。
    #[test]
    fn panicking_step_restores_windows_and_clears_session() {
        let fixture = fixture(
            FakeGrabber::new(false, false),
            FakeOverlay::new(false),
            MIN_FREE_BYTES,
        );

        assert_eq!(guard_session(&fixture.coordinator, || 7), Some(7));

        fixture.coordinator.start(CaptureTrigger::Hotkey).unwrap();
        // `catch_unwind` 会打印 panic 信息；测试只关心兜底结果。
        let outcome: Option<()> = guard_session(&fixture.coordinator, || panic!("boom"));
        assert!(outcome.is_none());
        assert_eq!(fixture.visibility.shows(), 1);

        // 会话已结束：后续结束事件被忽略，不会再次恢复窗口。
        fixture
            .coordinator
            .on_overlay_event(OverlayEvent::Cancelled)
            .unwrap();
        assert_eq!(fixture.visibility.shows(), 1);
    }

    /// Minor：入库失败时删除已 rename 的 PNG，避免孤儿文件。
    #[test]
    fn database_failure_removes_orphan_image() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("orphan.png");
        fs::write(&target, b"png").unwrap();

        let err = remove_orphan(&target, AppError::Database("locked".into()));
        assert!(matches!(err, AppError::Database(_)));
        assert!(!target.exists());

        // 文件已不存在时不 panic。
        let again = remove_orphan(&target, AppError::Database("locked".into()));
        assert!(matches!(again, AppError::Database(_)));
    }
}
