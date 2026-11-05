//! 应用编排核心：持有库句柄、配置、捕获运行时与事件发送端。
//!
//! 本模块不依赖 Tauri；`src-tauri` 只负责把命令参数/返回值映射成 DTO。
//! 库打开失败不会 panic：错误保存在 [`AppCore::library`] 里，由命令层转成
//! `CONFIG_MISSING` 等稳定错误码。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;

use crossbeam_channel::{select, Receiver, Sender};
use visrecall_capture::{GdiFrameGrabber, Win32WindowProbe};
use visrecall_domain::AppError;
use visrecall_library::paths::AppPaths;
use visrecall_library::{AppConfig, Library};
use visrecall_shell::ball::{
    spawn_ball_thread, BallConfig, BallHandle, MAX_BALL_SIZE, MIN_BALL_SIZE,
};
use visrecall_shell::hotkey::{spawn_hotkey_thread, HotkeyHandle, HotkeySpec};
use visrecall_shell::overlay::{spawn_overlay_thread, OverlayHandle};
use visrecall_shell::{OverlayEvent, ShellEvent};

use crate::capture_flow::{
    guard_session, CaptureCoordinator, NoopWindowVisibility, WindowVisibility,
};
use crate::events::{
    AppEvent, AppRecoveredEvent, CaptureFailedEvent, EventSink, HotkeyConflictEvent,
};

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
            library,
            Arc::new(GdiFrameGrabber::new()),
            Arc::new(Win32WindowProbe::new()),
            overlay.clone(),
            visibility,
            Arc::clone(&self.sink),
        );

        spawn_capture_loop(
            coordinator,
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

        let (fallback_tx, fallback_rx) = crossbeam_channel::unbounded();
        let runtime_tx = self
            .lock_runtime()
            .as_ref()
            .map(|runtime| runtime.shell_tx.clone());
        let uses_runtime = runtime_tx.is_some();
        let tx = runtime_tx.unwrap_or(fallback_tx);

        let handle = match spawn_hotkey_thread(spec, tx) {
            Ok(handle) => handle,
            Err(err) => {
                tracing::error!(hotkey = %spec, error = %err, "failed to register capture hotkey");
                self.emit_hotkey_conflict(&err);
                return Err(err);
            }
        };

        if !uses_runtime {
            self.spawn_shell_event_consumer(spec, fallback_rx);
        }
        *self.lock_hotkey() = Some(handle);
        tracing::info!(hotkey = %spec, "capture hotkey started");
        Ok(())
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

    /// 应用设置补丁：先写盘再更新内存，避免写失败后内存与磁盘不一致。
    ///
    /// 阶段 01 不支持快捷键热重载：`capture_hotkey` 与当前值不同时整体拒绝
    /// （不保存任何字段，保留旧值）。`ball_enabled` 会立即同步到悬浮球线程；
    /// `ball_size` 必须在壳层支持的 `16..=256` 内，越界整体拒绝（不落盘）；
    /// 范围内的变更需要重启才能生效（阶段 01 不重建窗口）。
    pub fn update_settings(
        &self,
        ball_enabled: Option<bool>,
        ball_size: Option<u32>,
        capture_hotkey: Option<String>,
    ) -> Result<AppConfig, AppError> {
        let mut guard = self.lock_config();

        if let Some(hotkey) = capture_hotkey.as_deref() {
            if hotkey != guard.hotkeys.capture {
                return Err(AppError::InvalidInput("restart required".into()));
            }
        }
        if let Some(size) = ball_size {
            // 与壳层的夹取范围一致：越界时宁可拒绝，也不要「存下来但显示不同」。
            if !(MIN_BALL_SIZE..=MAX_BALL_SIZE).contains(&size) {
                return Err(AppError::InvalidInput(format!(
                    "ball size must be between {MIN_BALL_SIZE} and {MAX_BALL_SIZE}"
                )));
            }
        }

        let mut next = guard.clone();
        if let Some(enabled) = ball_enabled {
            next.ball.enabled = enabled;
        }
        if let Some(size) = ball_size {
            next.ball.size = size;
        }

        let changed_visibility = ball_enabled.is_some() && next.ball.enabled != guard.ball.enabled;
        let changed_size = next.ball.size != guard.ball.size;

        next.save_atomic(&self.paths.config_file)?;
        *guard = next.clone();
        // 先释放配置锁再碰悬浮球线程，避免两把锁的持有顺序与捕获循环相反。
        drop(guard);

        if changed_visibility {
            self.apply_ball_visibility(next.ball.enabled);
        }
        if changed_size {
            tracing::warn!(
                size = next.ball.size,
                "floating ball size change requires a restart (phase 01 does not rebuild it)"
            );
        }

        tracing::info!(config = %self.paths.config_file.display(), "settings saved");
        Ok(next)
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

/// 捕获循环线程：串行处理快捷键/悬浮球触发、悬浮球位置与覆盖层结果。
///
/// 抓帧与落盘都是同步 IO/CPU 工作，放在专用线程（design §3 的 capture worker），
/// 不占主线程；两个通道都断开（`AppCore` 释放）时退出。
///
/// 每步都包在 [`guard_session`] 里：某一步 panic 时记录 error、恢复自有窗口
/// （否则主窗口可能永久隐藏），然后继续等后续事件；通道断开的正常退出不受影响。
fn spawn_capture_loop(
    coordinator: CaptureCoordinator,
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
                            Err(_) => CaptureStep::Exit,
                        },
                        recv(overlay) -> message => match message {
                            Ok(event) => {
                                if let Err(err) = coordinator.on_overlay_event(event) {
                                    tracing::warn!(error = %err, "overlay event handling failed");
                                }
                                CaptureStep::Continue
                            }
                            Err(_) => CaptureStep::Exit,
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

        let updated = core.update_settings(Some(false), Some(64), None).unwrap();
        assert!(!updated.ball.enabled);
        assert_eq!(updated.ball.size, 64);

        let reloaded = AppConfig::load_or_default(&core.paths().config_file);
        assert!(!reloaded.ball.enabled);
        assert_eq!(reloaded.ball.size, 64);
    }

    #[test]
    fn update_settings_rejects_hotkey_change_without_saving() {
        let dir = tempfile::tempdir().unwrap();
        let (core, _sink) = open(dir.path());
        let before = core.config();

        let err = core
            .update_settings(Some(false), Some(64), Some("Ctrl+Alt+X".into()))
            .unwrap_err();
        assert!(matches!(err, AppError::InvalidInput(_)));
        assert_eq!(err.to_string(), "invalid input: restart required");

        // 整个补丁被拒绝：球的字段也没有写入。
        let after = core.config();
        assert_eq!(after.ball, before.ball);
        let reloaded = AppConfig::load_or_default(&core.paths().config_file);
        assert_eq!(reloaded.ball, before.ball);
    }

    #[test]
    fn update_settings_accepts_unchanged_hotkey() {
        let dir = tempfile::tempdir().unwrap();
        let (core, _sink) = open(dir.path());
        let hotkey = core.config().hotkeys.capture;

        let updated = core
            .update_settings(Some(true), Some(56), Some(hotkey))
            .unwrap();
        assert_eq!(updated.ball.size, 56);
    }

    /// Task 15 fix：`ball_size` 必须在壳层支持的 `16..=256` 内，越界整体拒绝且不落盘。
    #[test]
    fn update_settings_rejects_out_of_range_ball_size() {
        let dir = tempfile::tempdir().unwrap();
        let (core, _sink) = open(dir.path());
        let before = core.config();

        for size in [0, 1, MIN_BALL_SIZE - 1, MAX_BALL_SIZE + 1, 4096] {
            let err = core.update_settings(None, Some(size), None).unwrap_err();
            assert!(
                matches!(err, AppError::InvalidInput(_)),
                "size {size} should be rejected, got {err:?}"
            );
            assert_eq!(
                err.to_string(),
                format!(
                    "invalid input: ball size must be between {MIN_BALL_SIZE} and {MAX_BALL_SIZE}"
                )
            );
        }

        // 拒绝时内存与磁盘都保持原值。
        assert_eq!(core.config().ball.size, before.ball.size);
        let reloaded = AppConfig::load_or_default(&core.paths().config_file);
        assert_eq!(reloaded.ball.size, before.ball.size);

        // 边界值可用。
        let updated = core
            .update_settings(None, Some(MIN_BALL_SIZE), None)
            .unwrap();
        assert_eq!(updated.ball.size, MIN_BALL_SIZE);
        let updated = core
            .update_settings(None, Some(MAX_BALL_SIZE), None)
            .unwrap();
        assert_eq!(updated.ball.size, MAX_BALL_SIZE);
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

        core.update_settings(Some(false), None, None).unwrap();
        assert!(!core.ball_enabled());
        assert!(!core.config().ball.enabled);

        core.update_settings(Some(true), None, None).unwrap();
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
