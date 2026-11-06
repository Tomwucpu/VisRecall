//! 应用编排核心：持有库句柄、配置、捕获运行时与事件发送端。
//!
//! 本模块不依赖 Tauri；`src-tauri` 只负责把命令参数/返回值映射成 DTO。
//! 库打开失败不会 panic：错误保存在 [`AppCore::library`] 里，由命令层转成
//! `CONFIG_MISSING` 等稳定错误码。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;

use crossbeam_channel::{select, Receiver, Sender};
use visrecall_capture::{GdiFrameGrabber, Win32WindowProbe};
use visrecall_domain::drop::DropContent;
use visrecall_domain::storage::{
    PruneReport, RetentionPreview, StorageUsage, MAX_RETENTION_DAYS, MIN_RETENTION_DAYS,
};
use visrecall_domain::tag::ItemTag;
use visrecall_domain::{now_millis, AppError, ItemKind};
use visrecall_library::paths::AppPaths;
use visrecall_library::{AppConfig, Library};
use visrecall_shell::ball::{
    spawn_ball_thread, BallConfig, BallHandle, BallSettings, BALL_SIZE_OPTIONS,
};
use visrecall_shell::hotkey::{spawn_hotkey_thread, HotkeyHandle, HotkeySpec};
use visrecall_shell::overlay::{spawn_overlay_thread, OverlayHandle};
use visrecall_shell::{OverlayEvent, ShellEvent};

use crate::capture_flow::{
    guard_session, CaptureCoordinator, NoopWindowVisibility, WindowVisibility,
};
use crate::detail::{load_detail, ItemDetail};
use crate::events::{
    AppEvent, AppRecoveredEvent, CaptureFailedEvent, EventSink, HotkeyConflictEvent, ItemEvent,
    ItemsPrunedEvent,
};
use crate::import_flow::ImportFlow;

/// 设置补丁：`None` 表示「不改这个字段」。
///
/// 用结构体而不是 5 个 `Option` 参数：调用点只有一处（`ipc.rs`），但字段会继续增加
/// （阶段 03 的模型配置），逐个位置参数化会让调用方难以看出漏了哪个字段。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SettingsPatchInput {
    pub ball_enabled: Option<bool>,
    pub ball_size: Option<u32>,
    pub capture_hotkey: Option<String>,
    pub ball_edge_hide: Option<bool>,
    pub hide_in_fullscreen: Option<bool>,
}

/// Tauri 托管的应用状态。
pub struct AppCore {
    paths: AppPaths,
    /// 库打开失败时保留错误；后续命令据此返回错误码。
    library: Result<Arc<Library>, AppError>,
    /// 配置在 `Mutex` 后共享：捕获循环（悬浮球位置落盘）与命令层都要读写。
    config: Arc<Mutex<AppConfig>>,
    sink: Arc<dyn EventSink>,
    /// 全局热键线程句柄；`start_hotkey` 之后才有值，drop 时自动注销。
    hotkey: Mutex<Option<HotkeyHandle>>,
    /// 捕获运行时；`start_runtime` 之后才有值，drop 时通知线程退出。
    runtime: Mutex<Option<CaptureRuntime>>,
    /// 主窗口（以及将来其他自有窗口）的隐藏/恢复；注入自 `src-tauri`。
    main_visibility: Arc<dyn WindowVisibility>,
    /// 悬浮球线程句柄；`start_runtime` 之后才有值。
    ///
    /// 放在 `Arc` 后共享：[`AppVisibility`] 也要在抓帧期间隐藏/恢复球。
    ball: Arc<Mutex<Option<BallHandle>>>,
    /// 最近一次生效的悬浮球可见性；与 `config.ball.enabled` 保持一致。
    ///
    /// 放 `Arc` 后共享：[`AppVisibility`] 在会话结束恢复球之前要检查它（M-3）。
    ball_enabled: Arc<AtomicBool>,
}

/// 捕获运行时的线程句柄与通道。
///
/// 字段顺序无关紧要；drop 时先丢弃 `shell_tx`（捕获循环退出），再丢弃覆盖层
/// 命令发送端（覆盖层线程退出并清理 D3D 资源）。
struct CaptureRuntime {
    /// 热键/悬浮球线程投递捕获请求的发送端。
    shell_tx: Sender<ShellEvent>,
    /// 只为持有覆盖层线程：命令通道断开后覆盖层线程自行退出。
    _overlay: OverlayHandle,
}

impl AppCore {
    /// 加载配置 → 打开库 → 恢复遗留任务；不涉及真实窗口（测试/无 UI 路径）。
    ///
    /// - 配置缺失/损坏由 [`AppConfig::load_or_default`] 兜底为默认值。
    /// - 库打开失败只记录日志并把错误留在结构体里，主窗口据此显示错误状态。
    /// - 恢复任务数 > 0 时发出 [`AppEvent::AppRecovered`]。
    pub fn open(paths: AppPaths, sink: Arc<dyn EventSink>) -> AppCore {
        Self::open_with_visibility(paths, sink, Arc::new(NoopWindowVisibility))
    }

    /// 同 [`AppCore::open`]，但注入自有窗口的隐藏/恢复实现（`src-tauri` 用主窗口）。
    pub fn open_with_visibility(
        paths: AppPaths,
        sink: Arc<dyn EventSink>,
        visibility: Arc<dyn WindowVisibility>,
    ) -> AppCore {
        let config = AppConfig::load_or_default(&paths.config_file);
        let ball_enabled = config.ball.enabled;
        let library_path = config.library_path.clone();
        tracing::info!(
            config = %paths.config_file.display(),
            library = %library_path.display(),
            "opening app core"
        );

        let library = match Library::open(library_path.clone()) {
            Ok(library) => {
                match library.recover_jobs() {
                    Ok(0) => {}
                    Ok(count) => {
                        tracing::info!(count, "recovered unfinished jobs");
                        sink.emit(AppEvent::AppRecovered(AppRecoveredEvent { count }));
                    }
                    Err(err) => {
                        tracing::warn!(error = %err, "failed to recover jobs");
                    }
                }
                Ok(Arc::new(library))
            }
            Err(err) => {
                tracing::error!(
                    library = %library_path.display(),
                    error = %err,
                    "failed to open library"
                );
                Err(err)
            }
        };

        AppCore {
            paths,
            library,
            config: Arc::new(Mutex::new(config)),
            sink,
            hotkey: Mutex::new(None),
            runtime: Mutex::new(None),
            main_visibility: visibility,
            ball: Arc::new(Mutex::new(None)),
            ball_enabled: Arc::new(AtomicBool::new(ball_enabled)),
        }
    }

    pub fn paths(&self) -> &AppPaths {
        &self.paths
    }

    /// 库句柄；打开失败时返回启动时的错误。
    pub fn library(&self) -> Result<&Library, &AppError> {
        self.library.as_ref().map(Arc::as_ref)
    }

    pub fn library_ok(&self) -> bool {
        self.library.is_ok()
    }

    /// 库目录：优先取已打开的库根目录，库不可用时退回配置里的路径
    /// （便于用户手工检查损坏的库）。
    pub fn library_root(&self) -> PathBuf {
        match &self.library {
            Ok(library) => library.root().to_path_buf(),
            Err(_) => self.config().library_path,
        }
    }

    /// 当前配置快照；命令层只读取，不直接改写。
    pub fn config(&self) -> AppConfig {
        self.lock_config().clone()
    }

    /// 事件发送端，供后续任务（捕获流程）复用。
    pub fn sink(&self) -> &Arc<dyn EventSink> {
        &self.sink
    }

    /// 启动捕获运行时：悬浮球线程 → 覆盖层线程 → ShellEvent 通道 → 捕获循环线程。
    ///
    /// 幂等；失败只返回错误（调用方记录日志），不阻止应用继续驻留托盘。
    /// 覆盖层线程在启动时完成 D3D 设备/窗口/交换链预创建，避免首次捕获超过
    /// 150ms 的入口预算（design §3、P1-R12）。
    ///
    /// 悬浮球线程即使配置为禁用也启动（窗口保持隐藏）：托盘随时可以打开球，
    /// 也避免每次切换都重建 COM/GDI+ 线程。
    pub fn start_runtime(&self) -> Result<(), AppError> {
        if self.lock_runtime().is_some() {
            tracing::debug!("capture runtime already started");
            return Ok(());
        }

        let library = match &self.library {
            Ok(library) => Arc::clone(library),
            Err(err) => {
                tracing::error!(error = %err, "capture runtime not started: library unavailable");
                return Err(AppError::Internal(format!("library unavailable: {err}")));
            }
        };

        let (shell_tx, shell_rx) = crossbeam_channel::unbounded();

        // 悬浮球：位置从配置恢复；可见性单独下发，线程本身不读配置。
        let config = self.config();
        let ball = spawn_ball_thread(
            BallConfig {
                size: config.ball.size,
                positions: config.ball.positions.clone(),
                edge_hide: config.ball.edge_hide,
                hide_in_fullscreen: config.ball.hide_in_fullscreen,
            },
            shell_tx.clone(),
        );
        *self.lock_ball() = Some(ball);
        self.apply_ball_visibility(config.ball.enabled);

        let (overlay_tx, overlay_rx) = crossbeam_channel::unbounded();
        let overlay = match spawn_overlay_thread(overlay_tx) {
            Ok(overlay) => overlay,
            Err(err) => {
                tracing::error!(error = %err, "failed to start overlay thread");
                return Err(err);
            }
        };

        // 抓帧期间同时隐藏主窗口与悬浮球，保证画面里没有自有 UI。
        let visibility: Arc<dyn WindowVisibility> = Arc::new(AppVisibility::new(
            Arc::clone(&self.main_visibility),
            Arc::clone(&self.ball),
            Arc::clone(&self.ball_enabled),
        ));
        let coordinator = CaptureCoordinator::new(
            Arc::clone(&library),
            Arc::new(GdiFrameGrabber::new()),
            Arc::new(Win32WindowProbe::new()),
            overlay.clone(),
            visibility,
            Arc::clone(&self.sink),
        );
        // 拖放导入：`Drop` 事件到达捕获循环后派发到独立工作线程（`spawn_import`），
        // 导入的落盘/转码/联网不会占住捕获事件循环。来源应用用独立探针实例。
        let import = Arc::new(ImportFlow::new(
            library,
            Arc::new(Win32WindowProbe::new()),
            Arc::clone(&self.sink),
        ));

        spawn_capture_loop(
            coordinator,
            import,
            shell_rx,
            overlay_rx,
            Arc::clone(&self.config),
            self.paths.config_file.clone(),
        )?;

        *self.lock_runtime() = Some(CaptureRuntime {
            shell_tx,
            _overlay: overlay,
        });
        tracing::info!("capture runtime started");
        Ok(())
    }

    /// 当前悬浮球是否应当可见（与 `config.ball.enabled` 同步）。
    pub fn ball_enabled(&self) -> bool {
        self.ball_enabled.load(Ordering::SeqCst)
    }

    /// 托盘「显示或隐藏悬浮球」：翻转 `config.ball.enabled` → 落盘 → 切换可见性。
    ///
    /// 返回翻转后的状态。写盘失败时保持原状态（不出现「内存已关、重启又开」的错位）。
    pub fn toggle_ball(&self) -> bool {
        let enabled = {
            let mut guard = self.lock_config();
            let mut next = guard.clone();
            next.ball.enabled = !guard.ball.enabled;
            match next.save_atomic(&self.paths.config_file) {
                Ok(()) => {
                    let enabled = next.ball.enabled;
                    tracing::info!(enabled, "floating ball toggled");
                    *guard = next;
                    enabled
                }
                Err(err) => {
                    tracing::warn!(error = %err, "failed to save ball visibility; keeping state");
                    guard.ball.enabled
                }
            }
        };
        self.apply_ball_visibility(enabled);
        enabled
    }

    /// 把可见性同步到悬浮球线程并记录到 [`AppCore::ball_enabled`]。
    fn apply_ball_visibility(&self, visible: bool) {
        self.ball_enabled.store(visible, Ordering::SeqCst);
        let guard = self.lock_ball();
        match guard.as_ref() {
            Some(ball) => ball.set_visible(visible),
            None => {
                tracing::debug!(
                    visible,
                    "floating ball thread not started; visibility deferred"
                );
            }
        }
    }

    /// 启动全局捕获热键（解析配置 → 注册 → 起事件消费线程）。
    ///
    /// 必须在托盘创建之后调用：注册失败要把「快捷键冲突」立刻显示在托盘提示上。
    /// 失败不阻止应用启动，只上报 [`AppEvent::HotkeyConflict`]；重复调用是空操作。
    ///
    /// 已启动 [`AppCore::start_runtime`] 时，热键事件直接进捕获循环；未启动时
    /// （库不可用等降级场景）退回临时通道，收到请求后只发
    /// `capture_failed { CAPTURE_UNAVAILABLE }`，不触发真实抓帧，也避免事件丢失。
    pub fn start_hotkey(&self) -> Result<(), AppError> {
        if self.lock_hotkey().is_some() {
            tracing::debug!("capture hotkey already started");
            return Ok(());
        }

        let configured = self.config().hotkeys.capture;
        let spec = match HotkeySpec::parse(&configured) {
            Ok(spec) => spec,
            Err(err) => {
                tracing::error!(hotkey = %configured, error = %err, "invalid capture hotkey");
                self.emit_hotkey_conflict(&err);
                return Err(err);
            }
        };

        let handle = match self.spawn_capture_hotkey(spec) {
            Ok(handle) => handle,
            Err(err) => {
                tracing::error!(hotkey = %spec, error = %err, "failed to register capture hotkey");
                self.emit_hotkey_conflict(&err);
                return Err(err);
            }
        };

        *self.lock_hotkey() = Some(handle);
        tracing::info!(hotkey = %spec, "capture hotkey started");
        Ok(())
    }

    /// 注册一个热键线程：有捕获运行时就走它的通道，否则建临时通道 + 降级消费者。
    ///
    /// 启动与热重载共用这一条路径——两处各写一遍必然漂移（例如热重载忘了挂
    /// `CAPTURE_UNAVAILABLE` 消费者，用户就会看到「按了键没反应」）。
    fn spawn_capture_hotkey(&self, spec: HotkeySpec) -> Result<HotkeyHandle, AppError> {
        let (fallback_tx, fallback_rx) = crossbeam_channel::unbounded();
        let runtime_tx = self
            .lock_runtime()
            .as_ref()
            .map(|runtime| runtime.shell_tx.clone());
        let uses_runtime = runtime_tx.is_some();
        let tx = runtime_tx.unwrap_or(fallback_tx);

        let handle = spawn_hotkey_thread(spec, tx)?;
        if !uses_runtime {
            self.spawn_shell_event_consumer(spec, fallback_rx);
        }
        Ok(handle)
    }

    /// 未启动运行时（例如库打不开）的临时消费者：不触发捕获，但把
    /// `capture_failed { CAPTURE_UNAVAILABLE }` 发出去，让托盘/前端能看到捕获不可用。
    fn spawn_shell_event_consumer(&self, spec: HotkeySpec, rx: Receiver<ShellEvent>) {
        let sink = Arc::clone(&self.sink);
        let spawned = thread::Builder::new()
            .name("visrecall-shell-events".into())
            .spawn(move || {
                for event in rx.iter() {
                    match event {
                        ShellEvent::CaptureRequest(trigger) => {
                            tracing::warn!(
                                hotkey = %spec,
                                ?trigger,
                                "capture requested; runtime not started"
                            );
                            sink.emit(AppEvent::CaptureFailed(CaptureFailedEvent {
                                code: "CAPTURE_UNAVAILABLE".into(),
                                message: "capture runtime not started".into(),
                            }));
                        }
                        ShellEvent::BallMoved { display_id, .. } => {
                            // 运行时未启动时悬浮球也不会创建；事件直接丢弃。
                            tracing::debug!(
                                display_id = %display_id,
                                "ball move ignored; runtime not started"
                            );
                        }
                        ShellEvent::Drop(_) => {
                            // 没有库/导入流程时无法落盘，明确告知而不是静默丢弃。
                            tracing::warn!("drop received; runtime not started");
                            sink.emit(AppEvent::CaptureFailed(CaptureFailedEvent {
                                code: "CAPTURE_UNAVAILABLE".into(),
                                message: "drop import needs an open library".into(),
                            }));
                        }
                        ShellEvent::DropRejected(message) => {
                            sink.emit(AppEvent::CaptureFailed(CaptureFailedEvent {
                                code: "DROP_REJECTED".into(),
                                message,
                            }));
                        }
                    }
                }
                tracing::debug!("shell event channel closed");
            });

        if let Err(err) = spawned {
            tracing::error!(error = %err, "failed to spawn shell event consumer");
        }
    }

    fn emit_hotkey_conflict(&self, err: &AppError) {
        self.sink
            .emit(AppEvent::HotkeyConflict(HotkeyConflictEvent {
                message: err.to_string(),
            }));
    }

    /// 全库清空要求的确认词；判断在 Rust 侧，前端不得硬编码。
    pub const PURGE_CONFIRMATION_WORD: &'static str = "删除全部";

    /// 库句柄；库打不开时统一返回 `CONFIG_MISSING`（design §8）。
    fn library_required(&self) -> Result<&Library, AppError> {
        self.library().map_err(|_| AppError::ConfigMissing)
    }

    /// 详情聚合（素材本体 + 原文/原图 URL + 标签）。
    pub fn detail(&self, id: &str) -> Result<ItemDetail, AppError> {
        load_detail(self.library_required()?, id)
    }

    /// 保存备注；成功后只发 `item_note_updated`，不惊动列表。
    pub fn set_note(&self, id: &str, text: &str) -> Result<(), AppError> {
        self.library_required()?.set_note(id, text)?;
        self.sink
            .emit(AppEvent::ItemNoteUpdated(ItemEvent { id: id.to_string() }));
        Ok(())
    }

    /// 贴标签；成功发 `item_tags_updated` + `tags_changed`。
    pub fn add_tag(&self, item_id: &str, name: &str) -> Result<ItemTag, AppError> {
        let chip = self.library_required()?.tag_add(item_id, name)?;
        self.emit_item_tags_changed(item_id);
        Ok(chip)
    }

    /// 解除关联；自动标签会写入抑制记录（库层负责）。
    pub fn detach_tag(&self, item_id: &str, tag_id: &str) -> Result<(), AppError> {
        self.library_required()?.tag_detach(item_id, tag_id)?;
        self.emit_item_tags_changed(item_id);
        Ok(())
    }

    /// 重命名标签；只影响标签集合。
    pub fn rename_tag(&self, tag_id: &str, name: &str) -> Result<(), AppError> {
        self.library_required()?.tag_rename(tag_id, name)?;
        self.sink.emit(AppEvent::TagsChanged);
        Ok(())
    }

    /// 删除标签，返回解除的关联条数。
    pub fn delete_tag(&self, tag_id: &str) -> Result<u64, AppError> {
        let removed = self.library_required()?.tag_delete(tag_id)?;
        self.sink.emit(AppEvent::TagsChanged);
        Ok(removed)
    }

    fn emit_item_tags_changed(&self, item_id: &str) {
        self.sink.emit(AppEvent::ItemTagsUpdated(ItemEvent {
            id: item_id.to_string(),
        }));
        self.sink.emit(AppEvent::TagsChanged);
    }

    /// 单条彻底删除；成功后发 `item_deleted`。
    pub fn delete_item(&self, id: &str) -> Result<(), AppError> {
        self.library_required()?.delete_item(id)?;
        self.sink
            .emit(AppEvent::ItemDeleted(ItemEvent { id: id.to_string() }));
        Ok(())
    }

    /// 全库清空的确认信息：待删条数 + 确认词。
    pub fn purge_preview(&self) -> Result<u64, AppError> {
        self.library_required()?.count_items()
    }

    /// 全库清空；确认词不匹配直接拒绝，不进入任何删除。
    pub fn purge(&self, confirm: &str) -> Result<visrecall_library::PurgeReport, AppError> {
        if confirm != Self::PURGE_CONFIRMATION_WORD {
            return Err(AppError::InvalidInput(
                "confirmation word mismatch".to_string(),
            ));
        }
        let report = self.library_required()?.purge_library()?;
        self.sink.emit(AppEvent::ItemsReset);
        Ok(report)
    }

    /// 用量快照。
    pub fn usage(&self) -> Result<StorageUsage, AppError> {
        self.library_required()?.usage()
    }

    /// 设置容量（软配额，GiB）。
    pub fn set_capacity_gb(&self, gigabytes: u32) -> Result<(), AppError> {
        self.library_required()?.set_capacity_gb(gigabytes)
    }

    /// 保存保留策略；`None` = 永久。
    pub fn set_retention(&self, days: Option<u32>) -> Result<(), AppError> {
        self.library_required()?.set_retention(days)
    }

    /// 清理预估：按给定天数算出会删多少条、释放多少字节。
    ///
    /// `None`（永久保留）返回全 0：永久策略不删任何素材。
    pub fn retention_preview(&self, days: Option<u32>) -> Result<RetentionPreview, AppError> {
        let Some(days) = days else {
            return Ok(RetentionPreview::default());
        };
        if !(MIN_RETENTION_DAYS..=MAX_RETENTION_DAYS).contains(&days) {
            return Err(AppError::InvalidInput(format!(
                "retention must be {MIN_RETENTION_DAYS}..={MAX_RETENTION_DAYS} days"
            )));
        }

        let library = self.library_required()?;
        let cutoff = now_millis() - i64::from(days) * 86_400_000;
        let item_count = library.count_items_before(cutoff)?;
        let mut bytes = 0;
        for item in library.list_expired(cutoff, 500)? {
            bytes += file_len(&library.root().join(&item.original_path));
            if let Some(thumb) = item.thumb_path {
                bytes += file_len(&library.root().join(thumb));
            }
        }
        Ok(RetentionPreview { item_count, bytes })
    }

    /// 按已保存的策略清理过期素材；逐条删除，单条失败不中断其余条目。
    ///
    /// 只在 `deleted > 0` 时发 `items_pruned`（`count` 为成功删除数）。
    pub fn prune_expired(&self) -> PruneReport {
        let Ok(library) = self.library_required() else {
            return PruneReport::default();
        };
        let Some(days) = library.retention().days else {
            return PruneReport::default();
        };

        let cutoff = now_millis() - i64::from(days) * 86_400_000;
        let expired = match library.list_expired(cutoff, 500) {
            Ok(items) => items,
            Err(err) => {
                tracing::warn!(%err, "failed to list expired items");
                return PruneReport::default();
            }
        };

        let mut report = PruneReport::default();
        for item in expired {
            match library.delete_item(&item.id) {
                Ok(_) => report.deleted += 1,
                Err(err) => {
                    report.failed += 1;
                    tracing::warn!(id = %item.id, %err, "failed to prune expired item");
                }
            }
        }

        if report.deleted > 0 {
            self.sink.emit(AppEvent::ItemsPruned(ItemsPrunedEvent {
                count: report.deleted,
            }));
        }
        report
    }

    /// 复制原始内容：图片写 `CF_DIB` + `PNG`，文本/链接写 `CF_UNICODETEXT`。
    ///
    /// 文本读的是**完整原文**（不受详情 20000 字符截断影响）；链接复制的是原始 URL。
    pub fn copy_original(&self, item_id: &str) -> Result<(), AppError> {
        let library = self.library_required()?;
        let item = library.get_item(item_id)?.ok_or(AppError::NotFound)?;

        match item.kind {
            ItemKind::Image => {
                let (bytes, _mime) = crate::thumbnail::load_original_bytes(library, item_id)?;
                visrecall_shell::clipboard::copy_image_png(&bytes)
            }
            ItemKind::Text => {
                let path = library.root().join(&item.original_path);
                let text = fs::read_to_string(&path).map_err(|err| match err.kind() {
                    std::io::ErrorKind::NotFound => {
                        AppError::FileMissing(item.original_path.clone())
                    }
                    _ => AppError::Io(format!("read {}: {err}", item.original_path)),
                })?;
                visrecall_shell::clipboard::copy_text(&text)
            }
            ItemKind::Link => match item.source_url.as_deref() {
                Some(url) if !url.is_empty() => visrecall_shell::clipboard::copy_text(url),
                _ => Err(AppError::FileMissing(item.id)),
            },
        }
    }

    /// 是否需要重启才能用新库。
    ///
    /// 阶段 02 Task 18 把它接到迁移闸门（`CaptureGate::RestartPending`）；在此之前恒为
    /// `false`——没有迁移入口，也就不存在待重启状态。
    pub fn restart_required(&self) -> bool {
        false
    }

    /// 应用设置补丁（design §6.11 的顺序即正确性）。
    ///
    /// 1. 先校验（尺寸三档、快捷键 parse）——不落盘、不改内存；
    /// 2. 若换键：**先起新线程**（注册失败 → `INVALID_INPUT`，旧句柄仍有效）；
    /// 3. 落盘（失败 → 停掉新线程，返回 IO，内存不变）；
    /// 4. 更新内存配置；
    /// 5. 换键成功才停旧线程并存入新句柄，同时发 `HotkeyRestored` 清托盘冲突提示；
    /// 6. 悬浮球：可见性、尺寸、贴边/全屏开关立即生效。
    pub fn update_settings(&self, patch: SettingsPatchInput) -> Result<AppConfig, AppError> {
        // 1. 校验
        let new_spec = match patch.capture_hotkey.as_deref() {
            Some(text) => Some(HotkeySpec::parse(text)?),
            None => None,
        };
        if let Some(size) = patch.ball_size {
            if !BALL_SIZE_OPTIONS.contains(&size) {
                return Err(AppError::InvalidInput(format!(
                    "ball size must be one of {BALL_SIZE_OPTIONS:?}"
                )));
            }
        }

        // 2. 换键：先起新线程。已有线程时两个不同组合键可以同时注册，
        //    因此这一步不会与旧线程冲突；反序会在注册失败瞬间丢掉唯一可用的快捷键。
        let current_spec = self.lock_hotkey().as_ref().map(|handle| handle.spec());
        let mut pending_hotkey = None;
        if let (Some(spec), Some(current)) = (new_spec.as_ref(), current_spec.as_ref()) {
            if spec != current {
                pending_hotkey = Some(self.spawn_capture_hotkey(*spec)?);
            }
        }

        // 3. 落盘：失败要把已经注册的新线程停掉，否则会留下一个「配置文件里没有」的热键。
        let mut next = {
            let guard = self.lock_config();
            let mut next = guard.clone();
            if let Some(enabled) = patch.ball_enabled {
                next.ball.enabled = enabled;
            }
            if let Some(size) = patch.ball_size {
                next.ball.size = size;
            }
            if let Some(edge_hide) = patch.ball_edge_hide {
                next.ball.edge_hide = edge_hide;
            }
            if let Some(hide) = patch.hide_in_fullscreen {
                next.ball.hide_in_fullscreen = hide;
            }
            if new_spec.is_some() {
                // 用规范化后的写法落盘，避免 "ctrl+shift+s" 与 "Ctrl+Shift+S" 交替出现。
                next.hotkeys.capture = new_spec
                    .as_ref()
                    .map(HotkeySpec::format)
                    .unwrap_or_else(|| guard.hotkeys.capture.clone());
            }

            if let Err(err) = next.save_atomic(&self.paths.config_file) {
                drop(pending_hotkey);
                return Err(err);
            }
            next
        };

        // 4. 更新内存（配置锁只在这段持有）
        let previous = {
            let mut guard = self.lock_config();
            let previous = guard.clone();
            *guard = next.clone();
            previous
        };

        // 5. 换键：停旧线程、存新句柄。配置锁已释放，避免与捕获循环的落盘形成反向持锁。
        if let Some(handle) = pending_hotkey {
            let old = self.lock_hotkey().replace(handle);
            drop(old);
            self.sink.emit(AppEvent::HotkeyRestored);
            tracing::info!(hotkey = %next.hotkeys.capture, "capture hotkey reloaded");
        }

        // 6. 悬浮球：可见性、尺寸与显示开关都立即生效
        if next.ball.enabled != previous.ball.enabled {
            self.apply_ball_visibility(next.ball.enabled);
        }
        let ball_changed = next.ball.size != previous.ball.size
            || next.ball.edge_hide != previous.ball.edge_hide
            || next.ball.hide_in_fullscreen != previous.ball.hide_in_fullscreen;
        if ball_changed {
            let settings = BallSettings {
                size: next.ball.size,
                edge_hide: next.ball.edge_hide,
                hide_in_fullscreen: next.ball.hide_in_fullscreen,
            };
            if let Some(ball) = self.lock_ball().as_ref() {
                ball.apply_settings(settings);
            }
        }

        tracing::info!(config = %self.paths.config_file.display(), "settings saved");
        // `next` 在这里已经被克隆进内存；返回值用于命令层回填前端缓存。
        next = self.config();
        Ok(next)
    }

    /// 测试/诊断：当前热键线程的组合键。
    #[doc(hidden)]
    pub fn hotkey_spec_for_test(&self) -> Option<HotkeySpec> {
        self.lock_hotkey().as_ref().map(|handle| handle.spec())
    }

    /// 测试/诊断：悬浮球句柄是否在位（迁移后必须恢复）。
    #[doc(hidden)]
    pub fn ball_present_for_test(&self) -> bool {
        self.lock_ball().is_some()
    }

    fn lock_config(&self) -> MutexGuard<'_, AppConfig> {
        // 持锁期间没有会 panic 的操作；被毒化时也继续使用，避免启动后永久不可用。
        self.config
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    fn lock_ball(&self) -> MutexGuard<'_, Option<BallHandle>> {
        self.ball
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    fn lock_hotkey(&self) -> MutexGuard<'_, Option<HotkeyHandle>> {
        self.hotkey
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    fn lock_runtime(&self) -> MutexGuard<'_, Option<CaptureRuntime>> {
        self.runtime
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

/// 文件字节数；不存在或不可读按 0 计（预估只用于展示）。
fn file_len(path: &Path) -> u64 {
    fs::metadata(path).map(|meta| meta.len()).unwrap_or(0)
}

/// 抓帧期间隐藏/恢复「自有窗口」的组合实现：主窗口 + 悬浮球。
///
/// 主窗口沿用注入的实现（`src-tauri` 的主窗口可见性实现）；悬浮球
/// 只在抓帧前可见时才隐藏，抓帧结束后恢复原状（球被禁用时不误开）。隐藏悬浮球
/// 走 `SendMessageW`，返回时窗口已经不可见，因此 `DwmFlush`/抓帧不会包含它。
///
/// 会话期间用户可能用托盘/设置关掉球：恢复前再看一次共享的 `ball_enabled`（M-3），
/// 避免「会话里关掉的球在会话结束时又冒出来」。
struct AppVisibility {
    main: Arc<dyn WindowVisibility>,
    ball: Arc<Mutex<Option<BallHandle>>>,
    /// 与 [`AppCore::ball_enabled`] 共享的开关。
    ball_enabled: Arc<AtomicBool>,
    /// 抓帧前悬浮球是否可见；`show_own_windows` 只恢复这个状态。
    ball_was_visible: AtomicBool,
}

impl AppVisibility {
    fn new(
        main: Arc<dyn WindowVisibility>,
        ball: Arc<Mutex<Option<BallHandle>>>,
        ball_enabled: Arc<AtomicBool>,
    ) -> Self {
        Self {
            main,
            ball,
            ball_enabled,
            ball_was_visible: AtomicBool::new(false),
        }
    }

    fn lock_ball(&self) -> MutexGuard<'_, Option<BallHandle>> {
        self.ball
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    /// 会话结束是否恢复球：抓帧前可见 **且** 用户没有在会话期间禁用。
    fn should_restore_ball(&self, was_visible: bool, enabled: bool) -> bool {
        was_visible && enabled
    }
}

impl WindowVisibility for AppVisibility {
    fn hide_own_windows(&self) {
        // 先藏球（立即返回），再藏主窗口（可能要等主线程应答）。
        {
            let guard = self.lock_ball();
            if let Some(ball) = guard.as_ref() {
                if ball.is_visible() {
                    ball.set_visible(false);
                    if !ball.is_visible() {
                        self.ball_was_visible.store(true, Ordering::SeqCst);
                    }
                }
            }
        }
        self.main.hide_own_windows();
    }

    fn show_own_windows(&self) {
        let was_visible = self.ball_was_visible.swap(false, Ordering::SeqCst);
        if was_visible && self.should_restore_ball(true, self.ball_enabled.load(Ordering::SeqCst)) {
            let guard = self.lock_ball();
            if let Some(ball) = guard.as_ref() {
                ball.set_visible(true);
            }
        } else if was_visible {
            tracing::info!("floating ball disabled during the capture session; leaving it hidden");
        }
        self.main.show_own_windows();
    }
}

/// 记录悬浮球拖动后的位置（归一化中心坐标）并原子落盘。
///
/// 悬浮球线程只发 [`ShellEvent::BallMoved`]，配置写在这里完成（design §9.1）。
/// 写盘失败只记录日志，不能让捕获循环退出或丢事件。
fn store_ball_position(
    config: &Mutex<AppConfig>,
    config_file: &Path,
    display_id: &str,
    nx: f32,
    ny: f32,
) -> Result<(), AppError> {
    let mut guard = config.lock().unwrap_or_else(|poison| poison.into_inner());
    let mut next = guard.clone();
    next.ball.positions.insert(display_id.to_string(), (nx, ny));
    next.save_atomic(config_file)?;
    *guard = next;
    Ok(())
}

/// 捕获循环单步结果：`Continue` 继续等事件，`Exit` 两个通道都断开（正常退出）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CaptureStep {
    Continue,
    Exit,
}

/// 在独立工作线程上执行一次拖放导入（design §9.2、I3）。
///
/// 捕获循环只调用本函数派发，不等待导入完成：链接标题抓取（最长 5s）与批量图片转码
/// 都在工作线程里跑，捕获请求/覆盖层事件因此不会排队。失败不会让进程退出；`ingest`
/// 自己会发 `capture_failed`，成功的素材在这里补 `item_created`。
fn spawn_import(import: Arc<ImportFlow>, content: DropContent) -> Result<(), AppError> {
    let spawned = thread::Builder::new()
        .name("visrecall-import".into())
        .spawn(move || match import.ingest(content) {
            Ok(items) => import.emit_created(&items),
            Err(err) => {
                // `ingest` 已经发出 `capture_failed`，这里只记日志。
                tracing::warn!(error = %err, "drop import failed");
            }
        });

    spawned
        .map(|_| ())
        .map_err(|err| AppError::Internal(format!("spawn import worker: {err}")))
}

/// 捕获循环线程：串行处理快捷键/悬浮球触发、悬浮球位置与覆盖层结果。
///
/// 抓帧与落盘都是同步 IO/CPU 工作，放在专用线程（design §3 的 capture worker），
/// 不占主线程；两个通道都断开（`AppCore` 释放）时退出。
///
/// 拖放导入**不在本线程执行**：`ShellEvent::Drop` 只派发到独立的导入工作线程
/// （[`spawn_import`]），保证 5s 的 URL 标题抓取/批量转码不阻塞捕获事件循环。
///
/// 每步都包在 [`guard_session`] 里：某一步 panic 时记录 error、恢复自有窗口
/// （否则主窗口可能永久隐藏），然后继续等后续事件；通道断开的正常退出不受影响。
fn spawn_capture_loop(
    coordinator: CaptureCoordinator,
    import: Arc<ImportFlow>,
    shell: Receiver<ShellEvent>,
    overlay: Receiver<OverlayEvent>,
    config: Arc<Mutex<AppConfig>>,
    config_file: PathBuf,
) -> Result<(), AppError> {
    let spawned = thread::Builder::new()
        .name("visrecall-capture-loop".into())
        .spawn(move || {
            loop {
                let step = guard_session(&coordinator, || {
                    select! {
                        recv(shell) -> message => match message {
                            Ok(ShellEvent::CaptureRequest(trigger)) => {
                                tracing::info!(?trigger, "capture requested");
                                if let Err(err) = coordinator.start(trigger) {
                                    tracing::warn!(error = %err, "capture start failed");
                                }
                                CaptureStep::Continue
                            }
                            Ok(ShellEvent::BallMoved { display_id, nx, ny }) => {
                                tracing::info!(
                                    display_id = %display_id,
                                    nx,
                                    ny,
                                    "floating ball position reported"
                                );
                                if let Err(err) = store_ball_position(
                                    &config,
                                    &config_file,
                                    &display_id,
                                    nx,
                                    ny,
                                ) {
                                    tracing::warn!(
                                        display_id = %display_id,
                                        error = %err,
                                        "failed to persist the floating ball position"
                                    );
                                }
                                CaptureStep::Continue
                            }
                            Ok(ShellEvent::Drop(content)) => {
                                // I3：导入（落盘/转码/URL 标题 5s 超时）跑在独立的导入
                                // 工作线程上，捕获/覆盖层事件不会排队。这里只负责派发。
                                tracing::info!("drop content received");
                                if let Err(err) = spawn_import(Arc::clone(&import), content) {
                                    tracing::warn!(error = %err, "failed to start the drop import");
                                    import.emit_failed("INTERNAL", &err.to_string());
                                }
                                CaptureStep::Continue
                            }
                            Ok(ShellEvent::DropRejected(message)) => {
                                tracing::warn!(message = %message, "drop rejected by the shell");
                                import.emit_rejected(&message);
                                CaptureStep::Continue
                            }
                            Err(_) => CaptureStep::Exit,
                        },
                        recv(overlay) -> message => match message {
                            Ok(event) => {
                                if let Err(err) = coordinator.on_overlay_event(event) {
                                    tracing::warn!(error = %err, "overlay event handling failed");
                                }
                                CaptureStep::Continue
                            }
                            Err(_) => {
                                // 覆盖层线程已退出（初始化后崩溃/命令通道断开）：先结束会话并
                                // 恢复自有窗口，再退出捕获循环；否则主窗口会永久隐藏、会话标志
                                // 也会残留，之后的快捷键全部被重入保护忽略。
                                tracing::warn!(
                                    "overlay channel closed; releasing the capture session before exit"
                                );
                                coordinator.release_windows();
                                CaptureStep::Exit
                            }
                        },
                    }
                });

                if step == Some(CaptureStep::Exit) {
                    break;
                }
            }
            tracing::debug!("capture loop exited");
        });

    spawned
        .map(|_| ())
        .map_err(|err| AppError::Internal(format!("spawn capture loop: {err}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use visrecall_domain::ItemKind;
    use visrecall_library::AppConfig;

    use crate::events::RecordingSink;

    fn paths_in(dir: &std::path::Path) -> AppPaths {
        AppPaths {
            config_file: dir.join("config.json"),
            default_library_root: dir.join("library"),
        }
    }

    /// 写入指向 `library` 的配置，避免测试依赖真实用户目录。
    fn seed_config(paths: &AppPaths) -> AppConfig {
        let config = AppConfig {
            library_path: paths.default_library_root.clone(),
            ..AppConfig::default()
        };
        config
            .save_atomic(&paths.config_file)
            .expect("write test config");
        config
    }

    fn open(dir: &std::path::Path) -> (AppCore, Arc<RecordingSink>) {
        let paths = paths_in(dir);
        seed_config(&paths);
        let sink = Arc::new(RecordingSink::default());
        (AppCore::open(paths, sink.clone()), sink)
    }

    #[test]
    fn open_creates_library_and_reads_config() {
        let dir = tempfile::tempdir().unwrap();
        let (core, sink) = open(dir.path());

        assert!(core.library_ok());
        assert!(core.library().is_ok());
        assert_eq!(
            core.library_root(),
            dir.path().join("library").to_path_buf()
        );
        assert!(core.config().library_path.ends_with("library"));
        assert!(sink.events().is_empty());
    }

    #[test]
    fn open_recovers_unfinished_jobs_and_emits_event() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        seed_config(&paths);

        {
            let library = Library::open(paths.default_library_root.clone()).unwrap();
            let item = library
                .insert_item(visrecall_domain::NewItem {
                    kind: ItemKind::Text,
                    captured_at: visrecall_domain::now_millis(),
                    source_app: None,
                    source_title: None,
                    source_url: None,
                    width: None,
                    height: None,
                    char_count: Some(3),
                    original_path: "texts/a.txt".into(),
                    content_hash: "deadbeef".into(),
                })
                .unwrap();
            library.enqueue_job(&item.id, "thumbnail", 1).unwrap();
            library
                .mark_job_running_for_test(&item.id, "thumbnail")
                .unwrap();
        }

        let sink = Arc::new(RecordingSink::default());
        let core = AppCore::open(paths, sink.clone());
        assert!(core.library_ok());

        let events = sink.events();
        assert_eq!(events.len(), 1);
        assert!(matches!(
            events[0],
            AppEvent::AppRecovered(AppRecoveredEvent { count: 1 })
        ));
    }

    #[test]
    fn open_keeps_error_when_library_path_is_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        let blocked = dir.path().join("blocked");
        std::fs::write(&blocked, b"not a directory").unwrap();

        let config = AppConfig {
            library_path: blocked,
            ..AppConfig::default()
        };
        config.save_atomic(&paths.config_file).unwrap();

        let core = AppCore::open(paths, Arc::new(RecordingSink::default()));
        assert!(!core.library_ok());
        assert!(matches!(core.library(), Err(AppError::Io(_))));
        // 库不可用时命令层仍能拿到路径用于展示。
        assert!(core.library_root().ends_with("blocked"));
    }

    #[test]
    fn update_settings_persists_ball_fields() {
        let dir = tempfile::tempdir().unwrap();
        let (core, _sink) = open(dir.path());

        let updated = core
            .update_settings(SettingsPatchInput {
                ball_enabled: Some(false),
                ball_size: Some(56),
                ..SettingsPatchInput::default()
            })
            .unwrap();
        assert!(!updated.ball.enabled);
        assert_eq!(updated.ball.size, 56);

        let reloaded = AppConfig::load_or_default(&core.paths().config_file);
        assert!(!reloaded.ball.enabled);
        assert_eq!(reloaded.ball.size, 56);
    }

    /// 热键线程未启动时（库不可用/未 start_hotkey）改键只写配置，等下次 `start_hotkey` 生效。
    #[test]
    fn update_settings_persists_hotkey_when_no_thread_is_running() {
        let dir = tempfile::tempdir().unwrap();
        let (core, _sink) = open(dir.path());
        assert!(core.hotkey_spec_for_test().is_none());

        let updated = core
            .update_settings(SettingsPatchInput {
                ball_enabled: Some(false),
                ball_size: Some(56),
                capture_hotkey: Some("Ctrl+Alt+X".into()),
                ..SettingsPatchInput::default()
            })
            .unwrap();
        assert_eq!(updated.hotkeys.capture, "Ctrl+Alt+X");
        assert!(!updated.ball.enabled);

        let reloaded = AppConfig::load_or_default(&core.paths().config_file);
        assert_eq!(reloaded.hotkeys.capture, "Ctrl+Alt+X");
    }

    #[test]
    fn update_settings_accepts_unchanged_hotkey() {
        let dir = tempfile::tempdir().unwrap();
        let (core, _sink) = open(dir.path());
        let hotkey = core.config().hotkeys.capture;

        let updated = core
            .update_settings(SettingsPatchInput {
                ball_enabled: Some(true),
                ball_size: Some(56),
                capture_hotkey: Some(hotkey),
                ..SettingsPatchInput::default()
            })
            .unwrap();
        assert_eq!(updated.ball.size, 56);
    }

    /// `ball_size` 只接受三档（P2-R10）；其他值整体拒绝且不落盘。
    #[test]
    fn update_settings_rejects_unsupported_ball_size() {
        let dir = tempfile::tempdir().unwrap();
        let (core, _sink) = open(dir.path());
        let before = core.config();

        for size in [0, 1, 16, 41, 256, 4096] {
            let err = core
                .update_settings(SettingsPatchInput {
                    ball_size: Some(size),
                    ..SettingsPatchInput::default()
                })
                .unwrap_err();
            assert!(
                matches!(err, AppError::InvalidInput(_)),
                "size {size} should be rejected, got {err:?}"
            );
        }

        // 拒绝时内存与磁盘都保持原值。
        assert_eq!(core.config().ball.size, before.ball.size);
        let reloaded = AppConfig::load_or_default(&core.paths().config_file);
        assert_eq!(reloaded.ball.size, before.ball.size);

        // 三档都可用。
        for size in BALL_SIZE_OPTIONS {
            let updated = core
                .update_settings(SettingsPatchInput {
                    ball_size: Some(size),
                    ..SettingsPatchInput::default()
                })
                .unwrap();
            assert_eq!(updated.ball.size, size);
        }
    }

    /// 记录调用次数的假主窗口可见性实现。
    #[derive(Default)]
    struct RecordingVisibility {
        hidden: std::sync::atomic::AtomicUsize,
        shown: std::sync::atomic::AtomicUsize,
    }

    impl WindowVisibility for RecordingVisibility {
        fn hide_own_windows(&self) {
            self.hidden.fetch_add(1, Ordering::SeqCst);
        }
        fn show_own_windows(&self) {
            self.shown.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// Task 15 fix（M-3）：会话期间把球禁用后，会话结束不再恢复球，主窗口照常恢复。
    #[test]
    fn session_end_does_not_restore_a_ball_disabled_during_capture() {
        let main = Arc::new(RecordingVisibility::default());
        let enabled = Arc::new(AtomicBool::new(true));
        let visibility = AppVisibility::new(
            Arc::clone(&main) as Arc<dyn WindowVisibility>,
            Arc::new(Mutex::new(None)),
            Arc::clone(&enabled),
        );

        // 模拟「抓帧前球是可见的」（测试里没有真实球线程）。
        visibility.ball_was_visible.store(true, Ordering::SeqCst);
        // 会话期间用户用托盘/设置关掉球：共享开关被更新。
        enabled.store(false, Ordering::SeqCst);

        visibility.show_own_windows();

        assert!(
            !visibility.ball_was_visible.load(Ordering::SeqCst),
            "the was-visible flag must be consumed even when the ball stays hidden"
        );
        assert_eq!(
            main.shown.load(Ordering::SeqCst),
            1,
            "main window is restored regardless"
        );

        // 仍启用时走恢复路径（没有球句柄时只清标志，不 panic）。
        visibility.ball_was_visible.store(true, Ordering::SeqCst);
        visibility.show_own_windows();
        assert!(!visibility.ball_was_visible.load(Ordering::SeqCst));
        assert_eq!(main.shown.load(Ordering::SeqCst), 2);
    }

    /// 恢复判定只看「抓帧前可见」+「当前仍启用」两个条件。
    #[test]
    fn restore_decision_requires_visible_and_enabled() {
        let visibility = AppVisibility::new(
            Arc::new(NoopWindowVisibility),
            Arc::new(Mutex::new(None)),
            Arc::new(AtomicBool::new(true)),
        );
        assert!(visibility.should_restore_ball(true, true));
        assert!(!visibility.should_restore_ball(true, false));
        assert!(!visibility.should_restore_ball(false, true));
        assert!(!visibility.should_restore_ball(false, false));
    }

    /// Task 15：`update_settings` 的 ball_enabled 立即可见（没有悬浮球线程时只更新状态）。
    #[test]
    fn update_settings_syncs_ball_visibility_state() {
        let dir = tempfile::tempdir().unwrap();
        let (core, _sink) = open(dir.path());
        assert!(core.ball_enabled());

        core.update_settings(SettingsPatchInput {
            ball_enabled: Some(false),
            ..SettingsPatchInput::default()
        })
        .unwrap();
        assert!(!core.ball_enabled());
        assert!(!core.config().ball.enabled);

        core.update_settings(SettingsPatchInput {
            ball_enabled: Some(true),
            ..SettingsPatchInput::default()
        })
        .unwrap();
        assert!(core.ball_enabled());
    }

    /// Task 15：托盘切换悬浮球 = 翻转配置 + 落盘；没有线程时不 panic。
    #[test]
    fn toggle_ball_persists_the_new_state() {
        let dir = tempfile::tempdir().unwrap();
        let (core, _sink) = open(dir.path());
        assert!(core.ball_enabled());

        assert!(!core.toggle_ball());
        assert!(!core.ball_enabled());
        let reloaded = AppConfig::load_or_default(&core.paths().config_file);
        assert!(!reloaded.ball.enabled);

        assert!(core.toggle_ball());
        assert!(core.ball_enabled());
        let reloaded = AppConfig::load_or_default(&core.paths().config_file);
        assert!(reloaded.ball.enabled);
    }

    /// Task 15：悬浮球拖动事件写回 `config.ball.positions` 并原子落盘。
    #[test]
    fn ball_move_updates_positions_and_saves_config() {
        let dir = tempfile::tempdir().unwrap();
        let (core, _sink) = open(dir.path());

        store_ball_position(
            &core.config,
            &core.paths().config_file,
            "\\\\.\\DISPLAY1",
            0.25,
            0.75,
        )
        .expect("store ball position");

        let positions = core.config().ball.positions;
        assert_eq!(positions.get("\\\\.\\DISPLAY1"), Some(&(0.25, 0.75)));

        let reloaded = AppConfig::load_or_default(&core.paths().config_file);
        assert_eq!(
            reloaded.ball.positions.get("\\\\.\\DISPLAY1"),
            Some(&(0.25, 0.75))
        );

        // 同一台显示器再次拖动只更新该条目。
        store_ball_position(
            &core.config,
            &core.paths().config_file,
            "\\\\.\\DISPLAY1",
            0.1,
            0.2,
        )
        .expect("update ball position");
        let positions = core.config().ball.positions;
        assert_eq!(positions.len(), 1);
        assert_eq!(positions.get("\\\\.\\DISPLAY1"), Some(&(0.1, 0.2)));
    }

    #[test]
    fn start_hotkey_reports_invalid_configuration() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        let mut config = seed_config(&paths);
        config.hotkeys.capture = "S".into();
        config.save_atomic(&paths.config_file).unwrap();

        let sink = Arc::new(RecordingSink::default());
        let core = AppCore::open(paths, sink.clone());

        let err = core.start_hotkey().unwrap_err();
        assert!(matches!(err, AppError::InvalidInput(_)));

        let events = sink.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].name(), "hotkey_conflict");
        assert!(events[0].payload()["message"]
            .as_str()
            .unwrap()
            .contains("invalid hotkey"));
    }

    #[test]
    fn start_hotkey_registers_configured_combination_once() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        let mut config = seed_config(&paths);
        // 避开默认的 Ctrl+Shift+S，测试可能与真实运行的应用共存。
        config.hotkeys.capture = "Ctrl+Alt+Shift+F22".into();
        config.save_atomic(&paths.config_file).unwrap();

        let sink = Arc::new(RecordingSink::default());
        let core = AppCore::open(paths, sink.clone());

        core.start_hotkey().expect("register test hotkey");
        // 幂等：重复启动不再注册，也不产出冲突事件。
        core.start_hotkey().expect("second call is a no-op");
        assert!(sink.events().is_empty());
    }

    /// 覆盖层通道断开（覆盖层线程退出）时，捕获循环必须结束会话、恢复自有窗口再退出。
    ///
    /// 不释放的话主窗口会永久隐藏、会话标志残留，之后的快捷键全部被重入保护忽略。
    #[test]
    fn closed_overlay_channel_releases_windows_before_the_loop_exits() {
        use std::time::{Duration, Instant};

        let dir = tempfile::tempdir().unwrap();
        let library = Arc::new(Library::open(dir.path().join("loop-lib")).unwrap());
        let sink = Arc::new(RecordingSink::default());

        // 覆盖层事件通道立刻断开：等价于覆盖层线程已退出。
        let (overlay_tx, overlay_rx) = crossbeam_channel::unbounded::<OverlayEvent>();
        drop(overlay_tx);
        let (shell_tx, shell_rx) = crossbeam_channel::unbounded::<ShellEvent>();

        let visibility = Arc::new(RecordingVisibility::default());
        let coordinator = CaptureCoordinator::with_overlay(
            Arc::clone(&library),
            Arc::new(NoopGrabber),
            Arc::new(FakeProbe),
            Arc::new(NoopOverlay),
            Arc::clone(&visibility) as Arc<dyn WindowVisibility>,
            Arc::clone(&sink) as Arc<dyn EventSink>,
        );
        let import = Arc::new(ImportFlow::new(
            Arc::clone(&library),
            Arc::new(FakeProbe),
            Arc::clone(&sink) as Arc<dyn EventSink>,
        ));

        spawn_capture_loop(
            coordinator,
            import,
            shell_rx,
            overlay_rx,
            Arc::new(Mutex::new(AppConfig::default())),
            dir.path().join("config.json"),
        )
        .expect("spawn capture loop");

        // 循环必须在通道断开后恢复一次自有窗口并退出。
        let deadline = Instant::now() + Duration::from_secs(2);
        while visibility.shown.load(Ordering::SeqCst) == 0 {
            assert!(
                Instant::now() < deadline,
                "capture loop did not release the own windows"
            );
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            visibility.shown.load(Ordering::SeqCst),
            1,
            "release must happen exactly once"
        );
        // 退出后不再处理事件：shell 通道保持打开但循环已结束。
        drop(shell_tx);
        thread::sleep(Duration::from_millis(50));
        assert_eq!(visibility.shown.load(Ordering::SeqCst), 1);
    }

    /// 不触碰真实桌面的窗口探针（导入与捕获循环测试共用）。
    struct FakeProbe;
    impl visrecall_capture::WindowProbe for FakeProbe {
        fn foreground(&self) -> Result<visrecall_domain::capture::ForegroundInfo, AppError> {
            Ok(visrecall_domain::capture::ForegroundInfo {
                hwnd: 0,
                pid: 7,
                process_name: "chrome.exe".into(),
                display_name: "Chrome".into(),
                title: "Example page".into(),
                bounds: visrecall_domain::geometry::PhysicalRect::new(0, 0, 100, 100),
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

    /// 不会真的抓帧的抓帧器（只用于不触发捕获的路径）。
    struct NoopGrabber;

    impl visrecall_capture::FrameGrabber for NoopGrabber {
        fn grab(
            &self,
            _displays: &[visrecall_domain::capture::DisplayInfo],
        ) -> Result<Vec<visrecall_domain::capture::RawFrame>, AppError> {
            Ok(Vec::new())
        }
    }

    /// 不会真的显示覆盖层的投递实现。
    struct NoopOverlay;

    impl crate::capture_flow::OverlayPresenter for NoopOverlay {
        fn show(
            &self,
            _frames: Vec<visrecall_domain::capture::RawFrame>,
            _displays: Vec<visrecall_domain::capture::DisplayInfo>,
            _foreground: visrecall_domain::capture::ForegroundInfo,
        ) -> Result<(), AppError> {
            Ok(())
        }
    }

    /// I3：拖放导入在独立工作线程执行——派发立即返回，入库（含 `item_created`）
    /// 由工作线程完成，捕获循环不会被 5s 的 URL 抓取或批量转码占住。
    #[test]
    fn spawn_import_dispatches_to_a_worker_thread() {
        use std::time::{Duration, Instant};

        let dir = tempfile::tempdir().unwrap();
        let library = Arc::new(Library::open(dir.path().join("import-lib")).unwrap());
        let sink = Arc::new(RecordingSink::default());
        let import = Arc::new(ImportFlow::new(
            Arc::clone(&library),
            Arc::new(FakeProbe),
            Arc::clone(&sink) as Arc<dyn EventSink>,
        ));

        let started = Instant::now();
        spawn_import(
            Arc::clone(&import),
            DropContent::Text {
                plain: "后台线程导入".into(),
                html: None,
            },
        )
        .expect("spawn import worker");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "dispatch must not wait for the import to finish"
        );

        // 入库在工作线程完成：`item_created` 与库里的行都会出现。
        let deadline = Instant::now() + Duration::from_secs(5);
        while sink.events().is_empty() {
            assert!(
                Instant::now() < deadline,
                "item_created must arrive from the import worker within 5s"
            );
            thread::sleep(Duration::from_millis(10));
        }
        assert!(matches!(sink.events()[0], AppEvent::ItemCreated(_)));
        assert_eq!(library.list_items(None, 10).unwrap().len(), 1);
    }

    /// I3：运行时未启动（库不可用等降级场景）时按快捷键不能只写日志，
    /// 要让托盘/前端看到 `CAPTURE_UNAVAILABLE`。
    #[test]
    fn runtime_less_capture_request_reports_capture_unavailable() {
        use visrecall_shell::CaptureTrigger;

        let dir = tempfile::tempdir().unwrap();
        let (core, sink) = open(dir.path());
        let spec = HotkeySpec::parse("Ctrl+Shift+S").expect("parse hotkey");

        let (tx, rx) = crossbeam_channel::unbounded();
        core.spawn_shell_event_consumer(spec, rx);
        tx.send(ShellEvent::CaptureRequest(CaptureTrigger::Hotkey))
            .unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if let Some(AppEvent::CaptureFailed(payload)) = sink.events().first() {
                assert_eq!(payload.code, "CAPTURE_UNAVAILABLE");
                assert_eq!(payload.message, "capture runtime not started");
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "no capture_failed event within 2s"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        drop(tx);
    }
}
