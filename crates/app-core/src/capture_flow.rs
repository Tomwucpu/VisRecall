//! 捕获编排与落盘：快捷键/托盘触发 → 记录来源 → 隐藏自有窗口 → 抓帧 → 覆盖层，
//! 以及覆盖层提交后的 PNG 编码、临时文件 → `rename` → 入库（design §5、§10.2）。
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
use std::sync::atomic::{AtomicBool, Ordering};
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

/// 开始捕获所需的最低可用空间（design §12：<2GB 拒绝新捕获，保留浏览）。
pub const MIN_FREE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

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
            session_active: AtomicBool::new(false),
        }
    }

    /// 替换可用空间查询（测试专用）。
    #[doc(hidden)]
    pub fn with_free_space_probe(mut self, probe: FreeSpaceProbe) -> Self {
        self.free_space = probe;
        self
    }

    /// 开始一次捕获：空间检查 → 前台窗口 → 隐藏自有窗口 + `DwmFlush` → 抓帧 →
    /// 全黑帧检查 → 覆盖层显示冻结帧。
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

        let displays = match enumerate_displays() {
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

        // 全黑帧通常是 DRM 保护窗口或抓帧失败：丢弃，不显示覆盖层（P1-R03）。
        if frames.iter().any(is_blank_frame) {
            let err = AppError::DrmProtected;
            warn!("captured frame is all black; suspected DRM protection");
            self.abort_session(&err);
            return Err(err);
        }

        if let Err(err) = self.overlay.show(frames, displays, foreground) {
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

    /// panic 兜底：清除会话标志并恢复自有窗口，避免捕获线程 panic 后窗口永久隐藏。
    ///
    /// 由 [`guard_session`] 在捕获循环里调用；`show_own_windows` 幂等，
    /// 即使 panic 发生在结束路径之后也不会重复显示窗口。
    pub fn recover_after_panic(&self) {
        self.clear_session();
        self.visibility.show_own_windows();
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

    fn emit_failed(&self, err: &AppError) {
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
            coordinator.recover_after_panic();
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
        AppError::Io(_) => "IO",
        AppError::Database(_) => "DATABASE",
        AppError::Internal(_) => "INTERNAL",
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

    let mut png = Vec::new();
    PngEncoder::new(&mut png)
        .write_image(&rgba, image.width, image.height, ExtendedColorType::Rgba8)
        .map_err(|err| AppError::Internal(format!("png encode failed: {err}")))?;
    Ok(png)
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// 目标路径：绝对路径 + 相对库根的路径（`originals/images/YYYY/MM/<id>.png`）。
///
/// 日期取本地时区（用户按本地月份翻找素材）；读不到本地偏移时回退 UTC。
fn image_target(root: &Path, id: &str) -> (String, PathBuf) {
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
    let tmp_dir = root.join("tmp");
    fs::create_dir_all(&tmp_dir)
        .map_err(|err| AppError::Io(format!("create dir {}: {err}", tmp_dir.display())))?;
    let tmp = tmp_dir.join(format!("{id}.tmp"));

    let write = || -> Result<(), AppError> {
        let mut file = fs::File::create(&tmp)
            .map_err(|err| AppError::Io(format!("create {}: {err}", tmp.display())))?;
        file.write_all(png)
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

    use visrecall_domain::geometry::PhysicalRect;
    use visrecall_domain::ItemStatus;

    use super::*;
    use crate::events::RecordingSink;

    /// 抓帧假实现：可配置「失败」与返回的帧内容，并记录调用次数。
    struct FakeGrabber {
        calls: AtomicUsize,
        fail: AtomicBool,
        blank: bool,
    }

    impl FakeGrabber {
        fn new(fail: bool, blank: bool) -> Arc<Self> {
            Arc::new(Self {
                calls: AtomicUsize::new(0),
                fail: AtomicBool::new(fail),
                blank,
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
        fn grab(&self, _displays: &[DisplayInfo]) -> Result<Vec<RawFrame>, AppError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail.load(Ordering::SeqCst) {
                return Err(AppError::CaptureFailed("fake grab failure".into()));
            }
            // `is_blank_frame` 要求 BGRA 四个通道全为 0（DRM 保护窗口的典型结果）。
            let pixel = if self.blank {
                [0, 0, 0, 0]
            } else {
                [0x7F, 0x7F, 0x7F, 0xFF]
            };
            Ok(vec![RawFrame {
                display_id: "fake-display".into(),
                width: 1,
                height: 1,
                stride: 4,
                bgra: pixel.to_vec(),
            }])
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

    /// 覆盖层假实现：只在成功时记录一次 `show`。
    struct FakeOverlay {
        shows: AtomicUsize,
        fail: AtomicBool,
    }

    impl FakeOverlay {
        fn new(fail: bool) -> Arc<Self> {
            Arc::new(Self {
                shows: AtomicUsize::new(0),
                fail: AtomicBool::new(fail),
            })
        }

        fn set_fail(&self, fail: bool) {
            self.fail.store(fail, Ordering::SeqCst);
        }

        fn shows(&self) -> usize {
            self.shows.load(Ordering::SeqCst)
        }
    }

    impl OverlayPresenter for FakeOverlay {
        fn show(
            &self,
            _frames: Vec<RawFrame>,
            _displays: Vec<DisplayInfo>,
            _foreground: ForegroundInfo,
        ) -> Result<(), AppError> {
            if self.fail.load(Ordering::SeqCst) {
                return Err(AppError::Internal("fake overlay failure".into()));
            }
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
        .with_free_space_probe(Arc::new(move |_path: &Path| Ok(free)));
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
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].name(), "item_created");

        let library = fixture.coordinator.library.clone();
        let items = library.list_items(None, 10).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].status, ItemStatus::Parsing);
        assert_eq!(items[0].width, Some(2));
        assert_eq!(items[0].height, Some(1));
        assert!(items[0].original_path.starts_with("originals/images/"));
        assert!(Path::new(&items[0].original_path).is_relative());

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
