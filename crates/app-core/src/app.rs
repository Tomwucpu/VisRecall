//! 应用编排核心：持有库句柄、配置、捕获运行时与事件发送端。
//!
//! 本模块不依赖 Tauri；`src-tauri` 只负责把命令参数/返回值映射成 DTO。
//! 库打开失败不会 panic：错误保存在 [`AppCore::library`] 里，由命令层转成
//! `CONFIG_MISSING` 等稳定错误码。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
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
use visrecall_library::config::StartupMode;
use visrecall_library::paths::AppPaths;
use visrecall_library::{AppConfig, Library, ModelConfig};
use visrecall_shell::ball::{
    spawn_ball_thread, BallConfig, BallFeedback, BallFeedbackHandle, BallHandle, BallSettings,
    BALL_MAX_SIZE, BALL_MIN_SIZE,
};
use visrecall_shell::hotkey::{spawn_hotkey_thread_with_gate, HotkeyHandle, HotkeySpec};
use visrecall_shell::overlay::{spawn_overlay_thread, OverlayHandle};
use visrecall_shell::{OverlayEvent, ShellEvent};

use crate::capture_flow::{
    guard_session, CaptureCoordinator, NoopWindowVisibility, WindowVisibility,
};
use crate::detail::{load_detail, ItemDetail};
use crate::events::{
    AppEvent, AppRecoveredEvent, CaptureFailedEvent, ContentEvent, EventSink, HotkeyConflictEvent,
    ItemEvent, ItemUpdatedEvent, ItemsDeletedEvent,
};
use crate::import_flow::ImportFlow;

/// 长任务闸门：迁移与「迁移完成待重启」期间拒绝捕获。
///
/// 用 `AtomicU8` 存：`restart_required` 直接由它派生，不另设布尔量，避免两个状态漂移。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureGate {
    Idle = 0,
    Relocating = 1,
    RestartPending = 2,
}

impl CaptureGate {
    pub fn from_u8(value: u8) -> CaptureGate {
        match value {
            1 => CaptureGate::Relocating,
            2 => CaptureGate::RestartPending,
            _ => CaptureGate::Idle,
        }
    }

    pub fn as_u8(self) -> u8 {
        self as u8
    }
}

/// 闸门拒绝捕获时的唯一文案来源（捕获循环与 `preflight` 共用，不复制文案）。
pub fn gate_message(gate: CaptureGate) -> Option<&'static str> {
    match gate {
        CaptureGate::Idle => None,
        CaptureGate::Relocating => Some("库正在迁移，捕获已暂停"),
        CaptureGate::RestartPending => Some("库已迁移，请重启后再捕获"),
    }
}

/// 闸门判定：`None` 表示放行，`Some` 是可直接上报的 `Busy`。
///
/// 捕获循环（主检查）、`CaptureCoordinator`（纵深防御）与集成测试共用这一处，
/// 保证「什么状态下拒绝、拒绝时说什么」只有一份定义。
pub fn gate_rejection(gate: &AtomicU8) -> Option<AppError> {
    gate_message(CaptureGate::from_u8(gate.load(Ordering::SeqCst)))
        .map(|message| AppError::Busy(message.to_string()))
}

/// 迁移结果：`restart_required` 恒为 `true`（不实现热切换）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelocateResult {
    pub restart_required: bool,
    pub new_root: String,
    pub files_copied: u64,
    pub bytes_copied: u64,
}

/// 设置补丁：`None` 表示「不改这个字段」。
///
/// 用结构体而不是 5 个 `Option` 参数：调用点只有一处（`ipc.rs`），但字段会继续增加
/// （阶段 03 的模型配置），逐个位置参数化会让调用方难以看出漏了哪个字段。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SettingsPatchInput {
    pub startup_mode: Option<StartupMode>,
    pub ball_enabled: Option<bool>,
    pub ball_size: Option<u32>,
    pub capture_enabled: Option<bool>,
    pub capture_hotkey: Option<String>,
    pub ball_edge_hide: Option<bool>,
    pub hide_in_fullscreen: Option<bool>,
}

/// 一个自定义模型的输入（`api_key` 只在内存里出现一次，随后写入凭据管理器）。
#[derive(Clone, Default, PartialEq, Eq)]
pub struct ModelEntryInput {
    pub id: String,
    pub name: String,
    pub model: String,
    pub base_url: String,
    pub protocol: String,
    pub protocol_id: String,
    /// `None` = 不改密钥；`Some("")` = 清除该模型密钥；`Some(key)` = 覆盖写。
    pub api_key: Option<String>,
}

impl std::fmt::Debug for ModelEntryInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelEntryInput")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("protocol", &self.protocol)
            // 密钥绝不出现在日志/错误链里（P3-R11）
            .field("api_key", &self.api_key.as_ref().map(|_| "***"))
            .finish()
    }
}

/// 模型配置整表提交：新建 / 修改 / 删除 / 启用都提交同一份列表，
/// 与原型「一次保存整份 models」的语义一致。
#[derive(Clone, Debug, Default)]
pub struct ModelConfigPatch {
    pub entries: Vec<ModelEntryInput>,
    pub active_id: Option<String>,
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
    /// 串行化开关、换键、启动和停止；原生线程不会持有此锁。
    capture_lifecycle: Mutex<()>,
    /// 与已保存配置同步，供输入线程和捕获循环读取。
    capture_enabled: Arc<AtomicBool>,
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
    /// 长任务闸门（迁移 / 待重启）；捕获循环与 `preflight` 都读它。
    gate: Arc<AtomicU8>,
    /// 阶段 03：云端模型注册表（热更新；选用哪个就用哪个，不降级）。
    providers: Arc<crate::pipeline_host::ProviderRegistry>,
    /// 阶段 03：索引管线 worker（`start_pipeline` 之后才有值）。
    pipeline: Mutex<Option<visrecall_pipeline::PipelineHandle>>,
    /// 阶段 03：VLM HTTP 与查询嵌入用的 tokio runtime；与 `pipeline` 同生命周期。
    pipeline_runtime: Mutex<Option<tokio::runtime::Runtime>>,
    /// 阶段 03：查询嵌入、语义就绪状态与降级标注。
    search: Arc<crate::search_flow::SearchFlow>,
    /// 阶段 03：暴力 int8 向量索引（检索时读取活跃向量表）。
    vectors: Option<Arc<visrecall_library::BruteForceInt8>>,
}

/// 捕获运行时的线程句柄与通道。
///
/// 字段顺序无关紧要；drop 时先丢弃 `shell_tx`（捕获循环退出），再丢弃覆盖层
/// 命令发送端（覆盖层线程退出并清理 D3D 资源）。
struct CaptureRuntime {
    /// 热键/悬浮球线程投递捕获请求的发送端。
    shell_tx: Sender<ShellEvent>,
    cancel_capture: Sender<Sender<()>>,
    /// 只为持有覆盖层线程：命令通道断开后覆盖层线程自行退出。
    _overlay: OverlayHandle,
}

impl AppCore {
    /// 将安装时选择持久化到应用配置；返回是否可以清除注册表里的待处理值。
    pub fn prepare_installer_library(
        paths: &AppPaths,
        install_dir: &Path,
        pending_path: impl FnOnce() -> Result<Option<PathBuf>, AppError>,
    ) -> Result<bool, AppError> {
        crate::installer_library::prepare(paths, install_dir, pending_path)
    }

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
        let capture_enabled = config.capture.enabled;
        let library_path = config.library_path.clone();
        tracing::info!(
            config = %paths.config_file.display(),
            library = %library_path.display(),
            "opening app core"
        );

        let library = match Library::open(library_path.clone()) {
            Ok(library) => {
                // 异常退出留下的导入任务直接丢弃：导入历史不入库，重启后也不自动
                // 扫描或触发云端分析。
                if let Err(err) = library.discard_unfinished_import_tasks() {
                    tracing::warn!(error = %err, "failed to discard unfinished import tasks");
                }
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

        // 阶段 03 的运行时：VLM 走 HTTP（async），本地推理在 worker 线程同步跑。
        let pipeline_runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .thread_name("visrecall-async")
            .build()
            .ok();
        let providers = crate::pipeline_host::ProviderRegistry::from_config(&config.models);
        let embed: Arc<dyn visrecall_domain::providers::EmbeddingProvider> =
            match visrecall_ai::models::resolve_model_dir() {
                Ok(dir) => Arc::new(visrecall_ai::BgeEmbedder::new(dir)),
                Err(err) => {
                    tracing::warn!(%err, "model dir unresolved; embedding disabled until models are present");
                    Arc::new(visrecall_ai::BgeEmbedder::new("models"))
                }
            };
        if pipeline_runtime.is_none() {
            tracing::error!("failed to build tokio runtime; semantic search stays degraded");
        }
        let search = crate::search_flow::SearchFlow::new(
            Arc::clone(&embed),
            pipeline_runtime
                .as_ref()
                .map(|runtime| runtime.handle().clone()),
            sink.clone(),
        );
        let vectors = library
            .as_ref()
            .ok()
            .map(|library| Arc::new(visrecall_library::BruteForceInt8::new(Arc::clone(library))));

        AppCore {
            paths,
            library,
            config: Arc::new(Mutex::new(config)),
            sink,
            hotkey: Mutex::new(None),
            capture_lifecycle: Mutex::new(()),
            capture_enabled: Arc::new(AtomicBool::new(capture_enabled)),
            runtime: Mutex::new(None),
            main_visibility: visibility,
            ball: Arc::new(Mutex::new(None)),
            ball_enabled: Arc::new(AtomicBool::new(ball_enabled)),
            gate: Arc::new(AtomicU8::new(CaptureGate::Idle.as_u8())),
            providers,
            pipeline: Mutex::new(None),
            pipeline_runtime: Mutex::new(pipeline_runtime),
            search,
            vectors,
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
        // 迁移成功但还没重启时，界面展示的是新路径；「打开库目录」必须与之一致，
        // 否则用户会打开一个已经不再使用的旧目录（P2-R09）。
        if self.gate() == CaptureGate::RestartPending {
            return self.config().library_path;
        }
        match &self.library {
            Ok(library) => library.root().to_path_buf(),
            Err(_) => self.config().library_path,
        }
    }

    /// 当前长任务闸门。
    pub fn gate(&self) -> CaptureGate {
        CaptureGate::from_u8(self.gate.load(Ordering::SeqCst))
    }

    /// 设置闸门；`RestartPending` 只能由迁移成功路径写入。
    pub fn set_gate(&self, gate: CaptureGate) {
        self.gate.store(gate.as_u8(), Ordering::SeqCst);
    }

    /// 闸门句柄：捕获循环与 `preflight` 共享同一个 `AtomicU8`。
    pub fn gate_handle(&self) -> Arc<AtomicU8> {
        Arc::clone(&self.gate)
    }

    /// 停止捕获运行时：运行时线程 → 热键线程 → 悬浮球线程。
    ///
    /// 三条都要停，原因各不相同（design §6.9.1）：
    /// - `CaptureRuntime::Drop` 断开 `shell_tx`，捕获循环退出；
    /// - 热键线程在 spawn 时就捕获了旧的 `Sender`，通道一断，事件永远投不进新循环；
    /// - 球线程同理，且它的 `Sender` 是 clone，不停会留下一个「能点但投递失败」的球。
    ///
    /// 恢复顺序必须是 `start_runtime()` → `start_hotkey()`（后者依赖前者建好的通道）。
    pub fn stop_runtime(&self) {
        let _lifecycle = self.lock_capture_lifecycle();
        self.lock_runtime().take();
        let hotkey = self.lock_hotkey().take();
        if let Some(hotkey) = hotkey {
            hotkey.stop_and_wait();
        }
        self.lock_ball().take();
    }

    /// 当前配置快照；命令层只读取，不直接改写。
    pub fn config(&self) -> AppConfig {
        self.lock_config().clone()
    }

    /// 事件发送端，供后续任务（捕获流程）复用。
    pub fn sink(&self) -> &Arc<dyn EventSink> {
        &self.sink
    }

    /// 长时间导入任务需要在线程间共享同一个库连接。
    pub fn library_arc(&self) -> Result<Arc<Library>, AppError> {
        match &self.library {
            Ok(library) => Ok(Arc::clone(library)),
            Err(_) => Err(AppError::ConfigMissing),
        }
    }

    fn manual_import_flow(&self) -> Result<ImportFlow, AppError> {
        Ok(ImportFlow::new(
            self.library_arc()?,
            Arc::new(Win32WindowProbe),
            Arc::clone(&self.sink),
        ))
    }

    pub fn import_files(&self, paths: &[PathBuf]) -> Result<crate::FileImportReport, AppError> {
        crate::ImportService::new(Arc::new(self.manual_import_flow()?)).import_files(paths)
    }

    pub fn inspect_import_files(paths: &[PathBuf]) -> Vec<crate::ImportFileSummary> {
        crate::ImportService::inspect(paths)
    }

    pub fn create_text_item(
        &self,
        title: Option<&str>,
        body: &str,
        tags: &[String],
    ) -> Result<visrecall_domain::Item, AppError> {
        self.manual_import_flow()?
            .create_text_item(title, body, tags)
    }

    pub fn directory_imports(&self) -> Result<crate::DirectoryImportService, AppError> {
        Ok(crate::DirectoryImportService::new(
            self.library_arc()?,
            Arc::clone(&self.sink),
        ))
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
        let _lifecycle = self.lock_capture_lifecycle();
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
            Arc::clone(&self.capture_enabled),
        );
        let ball_feedback = ball.feedback_handle();
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
        )
        // 纵深防御：主检查在捕获循环的事件分发处，这里再挡一次（防新入口绕过循环）。
        .with_gate(self.gate_handle())
        .with_capture_enabled(Arc::clone(&self.capture_enabled));
        // 拖放导入：`Drop` 事件到达捕获循环后派发到独立工作线程（`spawn_import`），
        // 导入的落盘/转码/联网不会占住捕获事件循环。来源应用用独立探针实例。
        let import = Arc::new(ImportFlow::new(
            library,
            Arc::new(Win32WindowProbe::new()),
            Arc::clone(&self.sink),
        ));

        let (cancel_capture, cancel_rx) = crossbeam_channel::unbounded();
        spawn_capture_loop(
            coordinator,
            import,
            self.gate_handle(),
            CaptureLoopEvents {
                shell: shell_rx,
                overlay: overlay_rx,
                cancel: cancel_rx,
            },
            Arc::clone(&self.config),
            self.paths.config_file.clone(),
            Some(ball_feedback),
        )?;

        *self.lock_runtime() = Some(CaptureRuntime {
            shell_tx,
            cancel_capture,
            _overlay: overlay,
        });
        tracing::info!("capture runtime started");
        // 保留策略在绑定启动之后跑一次：清理失败只记 warn，不阻止启动（P2-R08）。
        self.spawn_startup_prune();
        // 旧 PNG 缩略图在后台迁移为 WebP：幂等，失败不影响启动。
        self.spawn_legacy_thumb_migration();
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

    /// 截图开启时启动全局捕获热键（解析配置 → 注册 → 起事件消费线程）。
    ///
    /// 必须在托盘创建之后调用：注册失败要把「快捷键冲突」立刻显示在托盘提示上。
    /// 失败不阻止应用启动，只上报 [`AppEvent::HotkeyConflict`]；重复调用是空操作。
    ///
    /// 已启动 [`AppCore::start_runtime`] 时，热键事件直接进捕获循环；未启动时
    /// （库不可用等降级场景）退回临时通道，收到请求后只发
    /// `capture_failed { CAPTURE_UNAVAILABLE }`，不触发真实抓帧，也避免事件丢失。
    pub fn start_hotkey(&self) -> Result<(), AppError> {
        let _lifecycle = self.lock_capture_lifecycle();
        if !self.capture_enabled.load(Ordering::SeqCst) {
            return Ok(());
        }
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

        let handle =
            spawn_hotkey_thread_with_gate(spec, tx, Some(Arc::clone(&self.capture_enabled)))?;
        if !uses_runtime {
            self.spawn_shell_event_consumer(spec, fallback_rx);
        }
        Ok(handle)
    }

    /// 未启动运行时（例如库打不开）的临时消费者：不触发捕获，但把
    /// `capture_failed { CAPTURE_UNAVAILABLE }` 发出去，让托盘/前端能看到捕获不可用。
    fn spawn_shell_event_consumer(&self, spec: HotkeySpec, rx: Receiver<ShellEvent>) {
        let sink = Arc::clone(&self.sink);
        let capture_enabled = Arc::clone(&self.capture_enabled);
        let spawned = thread::Builder::new()
            .name("visrecall-shell-events".into())
            .spawn(move || {
                for event in rx.iter() {
                    match event {
                        ShellEvent::CaptureRequest(trigger) => {
                            if !capture_enabled.load(Ordering::SeqCst) {
                                continue;
                            }
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

    /// 保存用户编辑的 AI 描述；成功发 `item_content_updated { field: "summary" }`。
    pub fn set_summary(&self, id: &str, text: &str) -> Result<(), AppError> {
        self.library_required()?.set_summary(id, text)?;
        self.emit_content_updated(id, "summary");
        Ok(())
    }

    /// 保存用户编辑的 OCR 文本；成功发 `item_content_updated { field: "ocrText" }`。
    pub fn set_ocr_text(&self, id: &str, text: &str) -> Result<(), AppError> {
        self.library_required()?.set_ocr_text(id, text)?;
        self.emit_content_updated(id, "ocrText");
        Ok(())
    }

    /// 保存文本素材正文；成功发 `item_content_updated { field: "bodyText" }`。
    pub fn set_body_text(&self, id: &str, text: &str) -> Result<(), AppError> {
        self.library_required()?.set_body_text(id, text)?;
        self.emit_content_updated(id, "bodyText");
        Ok(())
    }

    fn emit_content_updated(&self, id: &str, field: &str) {
        self.sink.emit(AppEvent::ItemContentUpdated(ContentEvent {
            id: id.to_string(),
            field: field.to_string(),
        }));
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
    ///
    /// 引用素材（目录导入）只删库内记录、派生缓存与缩略图，外部原文件不动；同时清掉该路径
    /// 的排除记录，保证下次扫描同一目录时还能重新导入删除过的文件。
    pub fn delete_item(&self, id: &str) -> Result<(), AppError> {
        let library = self.library_required()?;
        if let Some(asset) = library.external_asset_for_item(id)? {
            library.clear_source_path_exclusion(&asset.source_id, &asset.relative_path)?;
        }
        library.delete_item(id)?;
        self.sink
            .emit(AppEvent::ItemDeleted(ItemEvent { id: id.to_string() }));
        Ok(())
    }

    /// 保存用户标题（`None` / 空白 = 恢复显示来源标题），发 `item_renamed`。
    ///
    /// 返回更新后的素材，IPC 层据此回填卡片与详情，不必再等一次列表刷新。
    pub fn rename_item(
        &self,
        id: &str,
        title: Option<&str>,
    ) -> Result<visrecall_domain::Item, AppError> {
        let library = self.library_required()?;
        library.set_item_title(id, title)?;
        let item = library.get_item(id)?.ok_or(AppError::NotFound)?;
        self.sink
            .emit(AppEvent::ItemRenamed(ItemEvent { id: id.to_string() }));
        Ok(item)
    }

    /// 批量彻底删除；返回成功/失败条数，并只发一次 `items_deleted`。
    ///
    /// 语义：
    /// - 去重后逐条执行，单条失败不影响其余条目（不回滚整批）；
    /// - `NotFound` 计入「已删除」——用户看到的结果是它没了，重复提交也应幂等；
    /// - 引用素材同样清掉路径排除记录，保证下次扫描还能重新导入。
    pub fn delete_items(&self, ids: &[String]) -> Result<ItemsDeletedEvent, AppError> {
        let library = self.library_required()?;
        let mut seen = std::collections::HashSet::new();
        let mut deleted = 0u64;
        let mut failed = 0u64;

        for id in ids {
            let id = id.trim();
            if id.is_empty() || !seen.insert(id.to_string()) {
                continue;
            }

            let result = (|| -> Result<(), AppError> {
                if let Some(asset) = library.external_asset_for_item(id)? {
                    library.clear_source_path_exclusion(&asset.source_id, &asset.relative_path)?;
                }
                library.delete_item(id).map(|_| ())
            })();

            match result {
                Ok(()) | Err(AppError::NotFound) => deleted += 1,
                Err(err) => {
                    failed += 1;
                    tracing::warn!(item_id = id, %err, "batch delete failed");
                }
            }
        }

        let report = ItemsDeletedEvent { deleted, failed };
        self.sink.emit(AppEvent::ItemsDeleted(report));
        Ok(report)
    }

    /// 重新解析：本地链路始终重跑，云端阶段按当前设置决定，成功后发 `item_updated`。
    ///
    /// `bump_revision` 让进行中的旧任务作废（worker 的 stale 检查），点击立即生效；
    /// 图片从 `ocr` 重跑（handler 视开关接力 index + vlm），文本/链接显式入队 index，
    /// 文本在开关允许时再入队 vlm（`index` handler 不接力 vlm）。旧结果无需清理：
    /// ocr/index/insights 都是覆盖写，vlm 的 content_hash 复用排除了素材自身。
    pub fn reparse_item(&self, id: &str) -> Result<(), AppError> {
        let library = self.library_required()?;
        let item = library.get_item(id)?.ok_or(AppError::NotFound)?;
        // 状态回 parsing：卡片/详情立即出现「解析中」反馈，由本地收口或 vlm 结果再翻 Ready/Pending
        library.update_status(id, visrecall_domain::ItemStatus::Parsing, None)?;
        if item.kind == ItemKind::Image {
            library.bump_revision_and_enqueue(
                id,
                visrecall_pipeline::STAGE_OCR,
                visrecall_pipeline::PRIORITY_NEW,
            )?;
        } else {
            let revision = library.bump_revision_and_enqueue(
                id,
                visrecall_pipeline::STAGE_INDEX,
                visrecall_pipeline::PRIORITY_NEW,
            )?;
            if item.kind == ItemKind::Text && library.cloud_parse_allowed(&item)? {
                library.enqueue_stage_at(
                    id,
                    visrecall_pipeline::STAGE_VLM,
                    revision,
                    visrecall_pipeline::PRIORITY_NEW,
                )?;
            }
        }
        self.sink.emit(AppEvent::ItemUpdated(ItemUpdatedEvent {
            id: id.to_string(),
            fields: vec!["status".to_string()],
        }));
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

    /// 图片是否允许上传云端解析（库内策略，默认关闭）。
    pub fn set_cloud_image_ai(&self, enabled: bool) -> Result<(), AppError> {
        self.library_required()?.set_cloud_image_ai(enabled)
    }

    /// 文本/片段是否允许上传云端解析（库内策略，默认关闭）。
    ///
    /// 两个开关分别约束图片与文本；均只影响之后的解析，不回溯历史素材。
    pub fn set_cloud_text_ai(&self, enabled: bool) -> Result<(), AppError> {
        self.library_required()?.set_cloud_text_ai(enabled)
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
        crate::storage_flow::prune_expired(library, self.sink.as_ref())
    }

    /// 启动清理：绑定启动完成后在后台线程跑一次保留策略。
    ///
    /// 失败只记 `warn`，绝不阻止启动；`days == None` 时函数立刻返回，不会碰库。
    fn spawn_startup_prune(&self) {
        let Ok(library) = self.library_required() else {
            return;
        };
        if library.retention().days.is_none() {
            return;
        }
        let library = Arc::clone(match &self.library {
            Ok(library) => library,
            Err(_) => return,
        });
        let sink = Arc::clone(&self.sink);
        let spawned = thread::Builder::new()
            .name("visrecall-retention".into())
            .spawn(move || {
                let report = crate::storage_flow::prune_expired(&library, sink.as_ref());
                if report.failed > 0 {
                    tracing::warn!(
                        deleted = report.deleted,
                        failed = report.failed,
                        "startup retention prune finished with failures"
                    );
                }
            });
        if let Err(err) = spawned {
            tracing::warn!(error = %err, "failed to spawn startup retention prune");
        }
    }

    /// 缩略图格式迁移：绑定启动完成后在后台线程跑一次（幂等，失败只记 warn）。
    fn spawn_legacy_thumb_migration(&self) {
        let library = match &self.library {
            Ok(library) => Arc::clone(library),
            Err(_) => return,
        };
        if let Err(err) = crate::thumbnail::spawn_legacy_thumb_migration(library) {
            tracing::warn!(error = %err, "failed to spawn legacy thumbnail migration");
        }
    }

    /// 复制原始内容：图片写 `CF_DIB` + `PNG`，文本/链接写 `CF_UNICODETEXT`。
    ///
    /// 文本读的是**完整原文**（不受详情 20000 字符截断影响）；链接复制的是原始 URL。
    /// 片段若带 HTML（富文本拖拽入库），额外写 `CF_HTML`，粘贴时图片与格式得以还原。
    pub fn copy_original(&self, item_id: &str) -> Result<(), AppError> {
        let library = self.library_required()?;
        let item = library.get_item(item_id)?.ok_or(AppError::NotFound)?;

        match item.kind {
            ItemKind::Image => {
                let (bytes, _mime) = crate::thumbnail::load_original_bytes(library, item_id)?;
                visrecall_shell::clipboard::copy_image_png(&bytes)
            }
            ItemKind::Text => {
                let text = visrecall_library::AssetReader::new(library)
                    .read_text(item_id, crate::import_flow::MAX_TEXT_BYTES)?;
                match library.get_snippet_html(item_id)? {
                    // 旧素材没有 HTML 记录：保持纯文本复制。
                    Some(html) if !html.trim().is_empty() => {
                        visrecall_shell::clipboard::copy_html(&html, &text)
                    }
                    _ => visrecall_shell::clipboard::copy_text(&text),
                }
            }
            ItemKind::Link => match item.source_url.as_deref() {
                Some(url) if !url.is_empty() => visrecall_shell::clipboard::copy_text(url),
                _ => Err(AppError::FileMissing(item.id)),
            },
        }
    }

    /// 是否需要重启才能用新库：直接由闸门派生，不另设布尔量。
    pub fn restart_required(&self) -> bool {
        self.gate() == CaptureGate::RestartPending
    }

    /// 迁移库位置（design §6.9 的顺序即正确性）。
    ///
    /// 成功：写 `config.json` → 闸门置 `RestartPending` → 恢复运行时与热键 → 返回
    /// `restart_required = true`；失败：**不动 config、不删源目录**，闸门回 `Idle`，
    /// 恢复运行时与热键后返回结构化错误。
    pub fn relocate_library(&self, target: PathBuf) -> Result<RelocateResult, AppError> {
        // 1. 闸门：Idle → Relocating（CAS；并发调用返回 Busy）
        if self
            .gate
            .compare_exchange(
                CaptureGate::Idle.as_u8(),
                CaptureGate::Relocating.as_u8(),
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_err()
        {
            return Err(AppError::Busy("another long task is running".into()));
        }

        let result = self.relocate_inner(&target);

        match &result {
            Ok(_) => {
                // 成功：闸门留在 RestartPending，恢复球与热键（捕获仍被闸门拒绝）。
                self.set_gate(CaptureGate::RestartPending);
                if let Err(err) = self.start_runtime() {
                    tracing::warn!(error = %err, "capture runtime not restored after relocation");
                }
                if let Err(err) = self.start_hotkey() {
                    tracing::warn!(error = %err, "hotkey not restored after relocation");
                }
            }
            Err(err) => {
                tracing::warn!(error = %err, "library relocation failed; keeping the current library");
                self.set_gate(CaptureGate::Idle);
                if let Err(err) = self.start_runtime() {
                    tracing::warn!(error = %err, "capture runtime not restored after failed relocation");
                }
                if let Err(err) = self.start_hotkey() {
                    tracing::warn!(error = %err, "hotkey not restored after failed relocation");
                }
            }
        }

        result
    }

    fn relocate_inner(&self, target: &Path) -> Result<RelocateResult, AppError> {
        let library = self.library_required()?;
        let source_root = library.root().to_path_buf();
        let usage = library.usage()?;

        // 2. 目标校验（包含「不在源内/源不在目标内」「非空目录」「空间 ≥ 1.1×」）
        visrecall_library::relocate::validate_target(&source_root, target, usage.used_bytes)?;

        // 3. 停止运行时：先停写入，再 checkpoint。
        self.stop_runtime();

        // 4. checkpoint：`busy != 0` 即失败，绝不进入复制（会复制出一个缺帧的库）。
        library.checkpoint()?;

        // 5. 复制（逐文件进度）
        let progress = |phase: &str, done: u64, total: u64| {
            self.sink.emit(AppEvent::StorageMigrationProgress(
                crate::events::MigrationProgressEvent::new(phase, done, total),
            ));
        };
        let report = visrecall_library::relocate::copy_library(&source_root, target, &progress)?;

        // 6. 校验：源库的行数与 library_id 用现有连接读出（不新开连接），
        //    校验连接只对目标库打开。
        self.sink.emit(AppEvent::StorageMigrationProgress(
            crate::events::MigrationProgressEvent::new("verify", 0, 0),
        ));
        let items = library.count_items()?;
        let library_id = read_library_id(&source_root)?;
        visrecall_library::relocate::verify(target, items, &library_id)?;

        // 7. 写 config（原子替换）。到这一步才改配置：失败不影响原库。
        {
            let mut guard = self.lock_config();
            let mut next = guard.clone();
            next.library_path = target.to_path_buf();
            next.save_atomic(&self.paths.config_file)?;
            *guard = next;
        }

        self.sink.emit(AppEvent::StorageMigrationProgress(
            crate::events::MigrationProgressEvent::new("done", 0, 0),
        ));

        Ok(RelocateResult {
            restart_required: true,
            new_root: target.display().to_string(),
            files_copied: report.files_copied,
            bytes_copied: report.bytes_copied,
        })
    }

    /// 应用设置补丁（design §6.11 的顺序即正确性）。
    ///
    /// 开启截图或换键时先试注册，再提交配置。关闭时先停止接受请求，再注销热键并
    /// 取消选区。悬浮球设置立即生效，启动方式在下次冷启动时使用。
    pub fn update_settings(&self, patch: SettingsPatchInput) -> Result<AppConfig, AppError> {
        let new_spec = match patch.capture_hotkey.as_deref() {
            Some(text) => Some(HotkeySpec::parse(text)?),
            None => None,
        };
        if let Some(size) = patch.ball_size {
            if !(BALL_MIN_SIZE..=BALL_MAX_SIZE).contains(&size) {
                return Err(AppError::InvalidInput(format!(
                    "ball size must be between {BALL_MIN_SIZE} and {BALL_MAX_SIZE}"
                )));
            }
        }

        let _lifecycle = self.lock_capture_lifecycle();
        if (patch.capture_enabled.is_some() || new_spec.is_some())
            && self.gate() == CaptureGate::Relocating
        {
            return Err(AppError::Busy("库正在迁移，请稍后修改截图设置".into()));
        }

        // 新组合键可以与旧组合键同时注册；注册失败时旧句柄和配置仍然有效。
        let current = self.config();
        let enable = patch.capture_enabled.unwrap_or(current.capture.enabled);
        let current_spec = self.lock_hotkey().as_ref().map(HotkeyHandle::spec);
        let pending_hotkey = if enable && (!current.capture.enabled || new_spec.is_some()) {
            let spec = match new_spec {
                Some(spec) => spec,
                None => HotkeySpec::parse(&current.hotkeys.capture)?,
            };
            if current_spec == Some(spec) {
                None
            } else {
                Some(self.spawn_capture_hotkey(spec)?)
            }
        } else {
            None
        };

        // 合并、落盘和更新内存保持在同一次加锁内，避免并发写入覆盖其它设置。
        let (previous, next) = {
            let mut guard = self.lock_config();
            let mut next = guard.clone();
            if let Some(mode) = patch.startup_mode {
                next.startup_mode = mode;
            }
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
            if let Some(enabled) = patch.capture_enabled {
                next.capture.enabled = enabled;
            }
            if let Some(spec) = new_spec {
                next.hotkeys.capture = spec.format();
            }

            if let Err(err) = next.save_atomic(&self.paths.config_file) {
                drop(guard);
                if let Some(handle) = pending_hotkey {
                    handle.stop_and_wait();
                }
                return Err(err);
            }
            let previous = std::mem::replace(&mut *guard, next.clone());
            self.capture_enabled
                .store(next.capture.enabled, Ordering::SeqCst);
            (previous, next)
        };

        // 配置锁已释放，后台等待注销或取消时允许捕获循环完成自己的工作。
        if previous.capture.enabled && !next.capture.enabled {
            let old = self.lock_hotkey().take();
            if let Some(old) = old {
                old.stop_and_wait();
            }
            self.cancel_pending_capture();
            self.sink.emit(AppEvent::HotkeyRestored);
        } else if let Some(handle) = pending_hotkey {
            let old = self.lock_hotkey().replace(handle);
            if let Some(old) = old {
                old.stop_and_wait();
            }
            self.sink.emit(AppEvent::HotkeyRestored);
        }

        // 悬浮球的可见性、尺寸与显示开关都立即生效。
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
        // 返回最新配置快照，供命令层回填前端缓存。
        Ok(self.config())
    }

    fn cancel_pending_capture(&self) {
        let cancel = self
            .lock_runtime()
            .as_ref()
            .map(|runtime| runtime.cancel_capture.clone());
        if let Some(cancel) = cancel {
            let (done, confirmed) = crossbeam_channel::bounded(1);
            if cancel.send(done).is_err() || confirmed.recv().is_err() {
                tracing::warn!("capture loop stopped before acknowledging cancellation");
            }
        }
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

    // ----- 阶段 03：索引管线、模型配置、语义就绪 -----

    /// 启动索引管线（幂等）：worker + handlers + 元数据钩子 + 启动自检 + 嵌入预热。
    ///
    /// 顺序（design §7.5）：`Library::open`（已发生）→ `Pipeline::spawn`（内部先恢复
    /// `running→queued`）→ 自检修复入队 → 嵌入预热。检索在预热完成前也可用（降级语义）。
    pub fn start_pipeline(&self) -> Result<(), AppError> {
        if self.lock_pipeline().is_some() {
            tracing::debug!("indexing pipeline already running");
            return Ok(());
        }
        let library = match &self.library {
            Ok(library) => Arc::clone(library),
            Err(err) => return Err(AppError::Internal(format!("library unavailable: {err}"))),
        };
        let runtime = self
            .lock_pipeline_runtime()
            .as_ref()
            .map(|runtime| runtime.handle().clone())
            .ok_or_else(|| AppError::Internal("async runtime unavailable".to_string()))?;

        let ocr: Arc<dyn visrecall_domain::providers::OcrProvider> =
            Arc::new(visrecall_ai::OcrChain::bootstrap());
        let started = crate::pipeline_host::start_pipeline(
            library,
            ocr,
            self.search.provider(),
            Arc::clone(&self.providers),
            self.sink.clone(),
            runtime,
            visrecall_pipeline::PipelineConfig::default(),
        )?;
        tracing::info!(repaired = started.repaired, "indexing pipeline started");
        *self.lock_pipeline() = Some(started.handle);
        self.search.warm_up();
        Ok(())
    }

    /// 停止索引管线（迁移闸门/退出用）；未启动时是空操作。
    pub fn stop_pipeline(&self) {
        if let Some(handle) = self.lock_pipeline().take() {
            handle.shutdown();
        }
    }

    pub fn pipeline_running(&self) -> bool {
        self.lock_pipeline().is_some()
    }

    /// 语义通道是否就绪（`AppStatus.semanticReady`）。
    pub fn semantic_ready(&self) -> bool {
        self.search.semantic_ready()
    }

    /// 查询嵌入；未就绪返回 `None`（检索降级为 FTS + 过滤）。
    pub fn embed_query(&self, text: &str) -> Option<Vec<f32>> {
        self.search.embed_query(text)
    }

    /// 检索用的嵌入 Provider（`embedding_rebuild` 需要它的 id 与维度）。
    pub fn embedding_provider(&self) -> Arc<dyn visrecall_domain::providers::EmbeddingProvider> {
        self.search.provider()
    }

    /// 执行一次检索（`search_items` 的后端实现）。
    ///
    /// 嵌入在编排内部完成（未就绪自动降级）；`limit` 由 `execute` clamp 到 1..=200。
    pub fn search(
        &self,
        raw: &str,
        limit: u32,
        filters: &visrecall_domain::ItemFilters,
    ) -> Result<visrecall_search::SearchOutcome, AppError> {
        let library = match &self.library {
            Ok(library) => library.as_ref(),
            Err(err) => return Err(AppError::Internal(format!("library unavailable: {err}"))),
        };
        let vectors = self
            .vectors
            .as_ref()
            .ok_or_else(|| AppError::Internal("vector index unavailable".to_string()))?;
        self.search
            .run(library, vectors.as_ref(), raw, limit, filters)
    }

    /// 当前模型配置（**不含密钥明文**）。
    pub fn model_config(&self) -> ModelConfig {
        self.providers.config()
    }

    /// 保存模型列表：显示名称 / 模型 ID / 地址 / 协议写 `config.json`，
    /// 密钥按模型 id 写凭据管理器，保存即热生效。
    ///
    /// `api_key = Some("")` 清除该模型密钥；`None` 保持不变。
    /// 从列表里移除的模型会顺带清掉它的凭据，避免留下孤儿条目。
    pub fn set_model_config(&self, patch: ModelConfigPatch) -> Result<ModelConfig, AppError> {
        let previous = self.lock_config().models.clone();

        let mut entries = Vec::with_capacity(patch.entries.len());
        let mut ids = std::collections::HashSet::new();
        for input in &patch.entries {
            let protocol = input.protocol.trim();
            let entry = visrecall_library::ModelEntry {
                id: input.id.trim().to_string(),
                name: input.name.trim().to_string(),
                model: input.model.trim().to_string(),
                base_url: input.base_url.trim().to_string(),
                protocol: if protocol.is_empty() {
                    "openai".to_string()
                } else {
                    protocol.to_string()
                },
                protocol_id: input.protocol_id.trim().trim_matches('/').to_string(),
            };
            entry.validate()?;
            if !ids.insert(entry.id.clone()) {
                return Err(AppError::InvalidInput(format!(
                    "duplicate model id: {}",
                    entry.id
                )));
            }
            entries.push(entry);
        }

        for (entry, input) in entries.iter().zip(patch.entries.iter()) {
            let key_slot = visrecall_ai::KeySlot::for_model(&entry.id);
            match input.api_key.as_deref() {
                Some("") => visrecall_ai::delete_key(&key_slot)?,
                Some(key) => visrecall_ai::save_key(&key_slot, key)?,
                None => {}
            }
        }
        for removed in previous
            .entries
            .iter()
            .filter(|entry| !ids.contains(&entry.id))
        {
            visrecall_ai::delete_key(&visrecall_ai::KeySlot::for_model(&removed.id))?;
        }

        let active_id = match patch.active_id.map(|id| id.trim().to_string()) {
            Some(id) if ids.contains(&id) => Some(id),
            _ => entries.first().map(|entry| entry.id.clone()),
        };
        let next = ModelConfig {
            entries,
            active_id,
            ..ModelConfig::default()
        };

        {
            let mut guard = self.lock_config();
            let mut config = guard.clone();
            config.models = next.clone();
            config.save_atomic(&self.paths.config_file)?;
            *guard = config;
        }

        // 热生效：注册表重建，下一轮任务立刻用新模型（无需重启）
        self.providers.reload(&next);
        tracing::info!(models = next.entries.len(), "model configuration updated");
        Ok(next)
    }

    /// 探测指定模型 id（连通性 + 视觉能力 + 延迟），协议与地址取**已保存**配置。
    pub fn probe_model(&self, id: &str) -> Result<visrecall_ai::ProbeOutcome, AppError> {
        let config = self.lock_config().models.clone();
        let entry = config.find(id).cloned().ok_or(AppError::ConfigMissing)?;
        entry.validate()?;
        let api_key =
            visrecall_ai::load_key(&visrecall_ai::KeySlot::for_model(&entry.id)).unwrap_or(None);
        let runtime = self
            .lock_pipeline_runtime()
            .as_ref()
            .map(|runtime| runtime.handle().clone())
            .ok_or_else(|| AppError::Internal("async runtime unavailable".to_string()))?;
        Ok(runtime.block_on(visrecall_ai::probe_with(
            &entry.base_url,
            &entry.model,
            api_key,
            &entry.protocol,
            &entry.protocol_id,
        )))
    }

    /// 触发向量蓝绿重建（管理/评测用，无 UI）。重建期间检索零中断。
    pub fn rebuild_embeddings(
        &self,
        model_id: &str,
        dim: u32,
    ) -> Result<visrecall_pipeline::RebuildReport, AppError> {
        let library = match &self.library {
            Ok(library) => Arc::clone(library),
            Err(err) => return Err(AppError::Internal(format!("library unavailable: {err}"))),
        };
        let runtime = self
            .lock_pipeline_runtime()
            .as_ref()
            .map(|runtime| runtime.handle().clone())
            .ok_or_else(|| AppError::Internal("async runtime unavailable".to_string()))?;
        let embed = self.search.provider();
        visrecall_pipeline::rebuild_embeddings(&library, embed.as_ref(), &runtime, model_id, dim)
    }

    fn lock_pipeline(&self) -> MutexGuard<'_, Option<visrecall_pipeline::PipelineHandle>> {
        self.pipeline
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    fn lock_pipeline_runtime(&self) -> MutexGuard<'_, Option<tokio::runtime::Runtime>> {
        self.pipeline_runtime
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
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

    fn lock_capture_lifecycle(&self) -> MutexGuard<'_, ()> {
        self.capture_lifecycle
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

/// 读源库的 `library_id`：迁移校验要求目标库与源库是同一个库。
fn read_library_id(root: &Path) -> Result<String, AppError> {
    let path = root.join(visrecall_library::paths::LIBRARY_META_FILE);
    let text = fs::read_to_string(&path)
        .map_err(|err| AppError::Io(format!("read {}: {err}", path.display())))?;
    let meta: visrecall_library::LibraryMeta = serde_json::from_str(&text)
        .map_err(|err| AppError::Io(format!("parse {}: {err}", path.display())))?;
    Ok(meta.library_id)
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
fn spawn_import(
    import: Arc<ImportFlow>,
    content: DropContent,
    feedback: Option<BallFeedbackHandle>,
) -> Result<(), AppError> {
    let spawned = thread::Builder::new()
        .name("visrecall-import".into())
        .spawn(move || {
            let mut report = |done: u32, total: u32| {
                if let Some(handle) = &feedback {
                    let progress = if total == 0 {
                        None
                    } else {
                        Some(((done as f32 / total as f32) * 99.0).round() as u8)
                    };
                    handle.send(BallFeedback::Busy { progress });
                }
            };
            match import.ingest_with_progress(content, &mut report) {
                Ok(items) => {
                    import.emit_created(&items);
                    if let Some(handle) = &feedback {
                        handle.send(BallFeedback::Success);
                    }
                }
                Err(err) => {
                    // `ingest` 已经发出 `capture_failed`，这里只记日志并复位悬浮球。
                    tracing::warn!(error = %err, "drop import failed");
                    if let Some(handle) = &feedback {
                        handle.send(BallFeedback::Failure);
                    }
                }
            }
        });

    spawned
        .map(|_| ())
        .map_err(|err| AppError::Internal(format!("spawn import worker: {err}")))
}

/// 捕获循环的事件分发点：处理**一条** shell 事件。
///
/// 这里是迁移闸门的**主检查**，因为 `CaptureRequest`（快捷键/悬浮球）与 `Drop`
/// （拖放到悬浮球）是两条互相独立的写入口：`Drop` 由导入流程处理，不经过
/// `CaptureCoordinator::start`，只查一处会让「迁移成功后往悬浮球拖一张图」照样写进旧库。
///
/// 抽成公开函数是为了让集成测试能在不启动真实窗口线程的前提下断言闸门行为
/// （`tests/relocate_flow.rs`）。
pub fn handle_shell_event(
    coordinator: &CaptureCoordinator,
    import: Arc<ImportFlow>,
    gate: &AtomicU8,
    config: &Mutex<AppConfig>,
    config_file: &Path,
    feedback: Option<&BallFeedbackHandle>,
    event: ShellEvent,
) {
    match event {
        ShellEvent::CaptureRequest(trigger) => {
            if !coordinator.capture_enabled() {
                return;
            }
            if let Some(err) = gate_rejection(gate) {
                tracing::warn!(?trigger, error = %err, "capture request rejected by the gate");
                // 用 `Busy` 上报 → `capture_failed { code: "BUSY" }`。
                coordinator.emit_failed(&err);
                return;
            }
            tracing::info!(?trigger, "capture requested");
            if let Err(err) = coordinator.start(trigger) {
                tracing::warn!(error = %err, "capture start failed");
            }
        }
        ShellEvent::BallMoved { display_id, nx, ny } => {
            tracing::info!(
                display_id = %display_id,
                nx,
                ny,
                "floating ball position reported"
            );
            if let Err(err) = store_ball_position(config, config_file, &display_id, nx, ny) {
                tracing::warn!(
                    display_id = %display_id,
                    error = %err,
                    "failed to persist the floating ball position"
                );
            }
        }
        ShellEvent::Drop(content) => {
            if let Some(err) = gate_rejection(gate) {
                tracing::warn!(error = %err, "drop rejected by the gate");
                // 导入流程没有 `AppError` 通道，按错误码直传；与 `Busy` 的映射保持一致。
                import.emit_failed("BUSY", &err.to_string());
                if let Some(handle) = feedback {
                    handle.send(BallFeedback::Failure);
                }
                return;
            }
            // I3：导入（落盘/转码/URL 标题 5s 超时）跑在独立的导入
            // 工作线程上，捕获/覆盖层事件不会排队。这里只负责派发。
            tracing::info!("drop content received");
            if let Some(handle) = feedback {
                handle.send(BallFeedback::Busy { progress: None });
            }
            if let Err(err) = spawn_import(Arc::clone(&import), content, feedback.cloned()) {
                tracing::warn!(error = %err, "failed to start the drop import");
                import.emit_failed("INTERNAL", &err.to_string());
                if let Some(handle) = feedback {
                    handle.send(BallFeedback::Failure);
                }
            }
        }
        ShellEvent::DropRejected(message) => {
            tracing::warn!(message = %message, "drop rejected by the shell");
            import.emit_rejected(&message);
            if let Some(handle) = feedback {
                handle.send(BallFeedback::Failure);
            }
        }
    }
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
// 捕获循环的输入由启动流程逐项拼装；再抽一层结构体只会增加间接层。
#[allow(clippy::too_many_arguments)]
struct CaptureLoopEvents {
    shell: Receiver<ShellEvent>,
    overlay: Receiver<OverlayEvent>,
    cancel: Receiver<Sender<()>>,
}

fn spawn_capture_loop(
    coordinator: CaptureCoordinator,
    import: Arc<ImportFlow>,
    gate: Arc<AtomicU8>,
    events: CaptureLoopEvents,
    config: Arc<Mutex<AppConfig>>,
    config_file: PathBuf,
    feedback: Option<BallFeedbackHandle>,
) -> Result<(), AppError> {
    let spawned = thread::Builder::new()
        .name("visrecall-capture-loop".into())
        .spawn(move || {
            loop {
                let step = guard_session(&coordinator, || {
                    select! {
                        recv(events.shell) -> message => match message {
                            Ok(event) => {
                                handle_shell_event(
                                    &coordinator,
                                    Arc::clone(&import),
                                    &gate,
                                    &config,
                                    &config_file,
                                    feedback.as_ref(),
                                    event,
                                );
                                CaptureStep::Continue
                            }
                            Err(_) => CaptureStep::Exit,
                        },
                        recv(events.cancel) -> message => match message {
                            Ok(done) => {
                                if let Err(err) = coordinator.cancel_pending_capture(&events.overlay) {
                                    tracing::warn!(error = %err, "overlay cancellation failed");
                                }
                                // 两个原生入口已停止投递截图。清空切换前排队的请求，
                                // 在关闭状态下分发，保留同一通道里的移动和导入事件。
                                for event in events.shell.try_iter() {
                                    handle_shell_event(
                                        &coordinator,
                                        Arc::clone(&import),
                                        &gate,
                                        &config,
                                        &config_file,
                                        feedback.as_ref(),
                                        event,
                                    );
                                }
                                let _ = done.send(());
                                CaptureStep::Continue
                            }
                            Err(_) => {
                                coordinator.release_windows();
                                CaptureStep::Exit
                            }
                        },
                        recv(events.overlay) -> message => match message {
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
    fn startup_mode_persists_even_when_library_is_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        // 用文件占住库路径，验证应用偏好不依赖素材库。
        fs::write(&paths.default_library_root, b"blocked").unwrap();
        seed_config(&paths);
        let core = AppCore::open(paths, Arc::new(RecordingSink::default()));
        assert!(!core.library_ok());
        assert_eq!(core.config().startup_mode, StartupMode::Silent);

        let updated = core
            .update_settings(SettingsPatchInput {
                startup_mode: Some(StartupMode::ShowWindow),
                ..SettingsPatchInput::default()
            })
            .unwrap();
        assert_eq!(updated.startup_mode, StartupMode::ShowWindow);
        let other_update = core
            .update_settings(SettingsPatchInput {
                ball_size: Some(80),
                ..SettingsPatchInput::default()
            })
            .unwrap();
        assert_eq!(other_update.startup_mode, StartupMode::ShowWindow);
        drop(core);

        let reopened = AppCore::open(paths_in(dir.path()), Arc::new(RecordingSink::default()));
        assert_eq!(reopened.config().startup_mode, StartupMode::ShowWindow);
        assert_eq!(reopened.config().ball.size, 80);
    }

    #[test]
    fn startup_mode_write_failure_keeps_old_memory_and_disk_values() {
        let dir = tempfile::tempdir().unwrap();
        let (core, _sink) = open(dir.path());
        let path = &core.paths().config_file;
        let old = fs::read(path).unwrap();
        // 只阻断原子写入过程，不动原来的配置文件。
        fs::create_dir(path.with_extension("json.tmp")).unwrap();

        let result = core.update_settings(SettingsPatchInput {
            startup_mode: Some(StartupMode::ShowWindow),
            ..SettingsPatchInput::default()
        });
        assert!(matches!(result, Err(AppError::Io(_))));
        assert_eq!(core.config().startup_mode, StartupMode::Silent);
        assert_eq!(fs::read(path).unwrap(), old);
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

    /// `ball_size` 只接受 50..=150；越界值整体拒绝且不落盘。
    #[test]
    fn update_settings_enforces_the_ball_size_range() {
        let dir = tempfile::tempdir().unwrap();
        let (core, _sink) = open(dir.path());
        let before = core.config();

        for size in [0, 1, 16, 49, 151, 256, 4096] {
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

        for size in [BALL_MIN_SIZE, 100, BALL_MAX_SIZE] {
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
        config.capture.enabled = true;
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
        config.capture.enabled = true;
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

        let (_cancel_tx, cancel_rx) = crossbeam_channel::unbounded();
        spawn_capture_loop(
            coordinator,
            import,
            Arc::new(AtomicU8::new(CaptureGate::Idle.as_u8())),
            CaptureLoopEvents {
                shell: shell_rx,
                overlay: overlay_rx,
                cancel: cancel_rx,
            },
            Arc::new(Mutex::new(AppConfig::default())),
            dir.path().join("config.json"),
            None,
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
        fn cancel_and_wait(&self) -> Result<(), AppError> {
            Ok(())
        }

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
    fn drop_dispatches_to_an_import_worker_when_capture_is_disabled() {
        use std::time::{Duration, Instant};

        let dir = tempfile::tempdir().unwrap();
        let library = Arc::new(Library::open(dir.path().join("import-lib")).unwrap());
        let sink = Arc::new(RecordingSink::default());
        let import = Arc::new(ImportFlow::new(
            Arc::clone(&library),
            Arc::new(FakeProbe),
            Arc::clone(&sink) as Arc<dyn EventSink>,
        ));

        let coordinator = CaptureCoordinator::with_overlay(
            Arc::clone(&library),
            Arc::new(NoopGrabber),
            Arc::new(FakeProbe),
            Arc::new(NoopOverlay),
            Arc::new(NoopWindowVisibility),
            sink.clone(),
        )
        .with_capture_enabled(Arc::new(AtomicBool::new(false)));
        let config = Mutex::new(AppConfig::default());
        let gate = AtomicU8::new(CaptureGate::Idle.as_u8());
        let started = Instant::now();
        handle_shell_event(
            &coordinator,
            import,
            &gate,
            &config,
            &dir.path().join("config.json"),
            None,
            ShellEvent::Drop(DropContent::Text {
                plain: "后台线程导入".into(),
                html: None,
            }),
        );
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
        core.capture_enabled.store(true, Ordering::SeqCst);

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

    /// 删除引用素材：外部原文件不动，库内记录与排除记录都清掉，重扫同一目录还能再导入。
    #[test]
    fn deleting_reference_item_keeps_original_and_allows_rescan() {
        use visrecall_library::NewImportSource;

        let dir = tempfile::tempdir().unwrap();
        let (core, _sink) = open(dir.path());
        let source_dir = tempfile::tempdir().unwrap();
        let file = source_dir.path().join("note.txt");
        std::fs::write(&file, "外部原文").unwrap();

        let library = core.library().unwrap();
        let source = library
            .create_import_source(NewImportSource {
                root_path: source_dir.path().display().to_string(),
                include_subdirectories: true,
                include_images: true,
                include_text: true,
            })
            .unwrap();
        let item = library
            .insert_reference_item(
                &source.id,
                "note.txt",
                12,
                1,
                visrecall_domain::NewItem {
                    kind: ItemKind::Text,
                    captured_at: 1,
                    source_app: Some("VisRecall".into()),
                    source_title: Some("note".into()),
                    source_url: None,
                    width: None,
                    height: None,
                    char_count: Some(4),
                    original_path: String::new(),
                    content_hash: "hash".into(),
                },
            )
            .unwrap();

        core.delete_item(&item.id).unwrap();

        assert!(library.get_item(&item.id).unwrap().is_none());
        assert!(library.external_asset_for_item(&item.id).unwrap().is_none());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "外部原文");

        // 旧实现会在这里写入排除记录，导致重扫 total_count 为 0。
        let scan = core
            .directory_imports()
            .unwrap()
            .scan(source_dir.path(), crate::DirectoryScanOptions::default())
            .unwrap();
        assert_eq!(scan.total_count, 1);
    }
}
