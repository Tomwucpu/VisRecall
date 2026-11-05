//! 捕获编排与落盘：快捷键/托盘触发 → 记录来源 → 隐藏自有窗口 → 抓帧 → 覆盖层，
//! 以及覆盖层提交后的 PNG 编码、临时文件 → `rename` → 入库（design §5、§10.2）。
//!
//! 线程模型（design §3）：本模块的方法在 app-core 的捕获循环线程上同步执行；
//! 抓帧与窗口探针通过 trait 注入，覆盖层通过 [`OverlayPresenter`]（生产实现为
//! [`OverlayHandle`]）投递，便于用假实现测试失败分支。
//!
//! 已知限制（Task 14 范围内不实现）：抓帧失败后的**实时模式**（透明覆盖层 +
//! 提交时现场抓选区）尚未实现，当前行为是重试一次后报 `CAPTURE_FAILED`。

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use image::codecs::png::PngEncoder;
use image::{ExtendedColorType, ImageEncoder};
use sha2::{Digest, Sha256};
use tracing::{info, warn};
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
    /// 失败时恢复自有窗口并发 `capture_failed`；成功时窗口保持隐藏，等
    /// [`CaptureCoordinator::on_overlay_event`] 的结束路径再恢复。
    pub fn start(&self, trigger: CaptureTrigger) -> Result<(), AppError> {
        info!(?trigger, "capture starting");

        if let Some(free) = self.check_free_space() {
            if free < MIN_FREE_BYTES {
                warn!(free_bytes = free, "free space below capture threshold");
                let err = AppError::StorageFull;
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
                self.fail_after_hide(&err);
                return Err(err);
            }
            Err(err) => {
                self.fail_after_hide(&err);
                return Err(err);
            }
        };

        let frames = match self.grab_with_retry(&displays) {
            Ok(frames) if !frames.is_empty() => frames,
            Ok(_) => {
                let err = AppError::CaptureFailed("frame grabber returned no frames".into());
                self.fail_after_hide(&err);
                return Err(err);
            }
            Err(err) => {
                self.fail_after_hide(&err);
                return Err(err);
            }
        };

        // 全黑帧通常是 DRM 保护窗口或抓帧失败：丢弃，不显示覆盖层（P1-R03）。
        if frames.iter().any(is_blank_frame) {
            let err = AppError::DrmProtected;
            warn!("captured frame is all black; suspected DRM protection");
            self.fail_after_hide(&err);
            return Err(err);
        }

        if let Err(err) = self.overlay.show(frames, displays, foreground) {
            warn!(error = %err, "overlay show failed");
            self.fail_after_hide(&err);
            return Err(err);
        }

        info!("frozen frames handed to the overlay");
        Ok(())
    }

    /// 处理覆盖层结果：提交则落盘 + 入库 + `item_created`，取消则只恢复窗口。
    ///
    /// 每个结束路径都恢复自有窗口；`show_own_windows` 必须幂等（覆盖层对重复
    /// `Cancelled` 幂等，这里同样只依赖幂等恢复）。
    pub fn on_overlay_event(&self, event: OverlayEvent) -> Result<(), AppError> {
        match event {
            OverlayEvent::Committed {
                image,
                global_rect,
                foreground,
            } => {
                info!(
                    width = image.width,
                    height = image.height,
                    x = global_rect.x,
                    y = global_rect.y,
                    "overlay committed; persisting capture"
                );
                let result = persist_capture(image, foreground, &self.library);
                self.visibility.show_own_windows();
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
                info!("overlay cancelled; own windows restored");
                self.visibility.show_own_windows();
                Ok(())
            }
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

    /// 隐藏之后的失败路径：恢复自有窗口并发 `capture_failed`。
    fn fail_after_hide(&self, err: &AppError) {
        self.visibility.show_own_windows();
        self.emit_failed(err);
    }

    fn emit_failed(&self, err: &AppError) {
        self.sink.emit(AppEvent::CaptureFailed(CaptureFailedEvent {
            code: capture_failed_code(err).to_string(),
            message: err.to_string(),
        }));
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
/// 任一步失败清理临时文件；数据库只存相对库根的路径。
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

    let item = library.insert_item_with_id(
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
    )?;

    info!(
        id = %item.id,
        path = %item.original_path,
        width,
        height,
        "captured image persisted"
    );
    Ok(item)
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

    /// 抓帧假实现：可配置「连续失败」与返回的帧内容，并记录调用次数。
    struct FakeGrabber {
        calls: AtomicUsize,
        fail: bool,
        blank: bool,
    }

    impl FakeGrabber {
        fn new(fail: bool, blank: bool) -> Arc<Self> {
            Arc::new(Self {
                calls: AtomicUsize::new(0),
                fail,
                blank,
            })
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl FrameGrabber for FakeGrabber {
        fn grab(&self, _displays: &[DisplayInfo]) -> Result<Vec<RawFrame>, AppError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail {
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
        fail: bool,
    }

    impl FakeOverlay {
        fn new(fail: bool) -> Arc<Self> {
            Arc::new(Self {
                shows: AtomicUsize::new(0),
                fail,
            })
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
            if self.fail {
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

        let codes = event_codes(&fixture.sink);
        assert_eq!(codes.len(), 1);
        assert_eq!(codes[0].0, "STORAGE_FULL");
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
    }

    #[test]
    fn cancelled_event_only_restores_windows() {
        let fixture = fixture(
            FakeGrabber::new(false, false),
            FakeOverlay::new(false),
            MIN_FREE_BYTES,
        );

        fixture
            .coordinator
            .on_overlay_event(OverlayEvent::Cancelled)
            .unwrap();
        assert_eq!(fixture.visibility.shows(), 1);
        assert!(fixture.sink.events().is_empty());

        // 重复 `Cancelled` 幂等：恢复再来一次，但不产生记录。
        fixture
            .coordinator
            .on_overlay_event(OverlayEvent::Cancelled)
            .unwrap();
        assert_eq!(fixture.visibility.shows(), 2);
        assert!(fixture.sink.events().is_empty());
    }
}
