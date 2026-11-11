//! pipeline 宿主与 Provider 注册表（design §6.5、§7）。
//!
//! 职责：
//! 1. 构造 [`PipelineDeps`]（OCR 链 / 嵌入 / 云端 VLM / 文本 Provider / tokio handle）；
//! 2. 启动 2 个 worker、注册四个 handler、注册元数据钩子（bump_revision + 入队 index）；
//! 3. 启动自检修复 + `pending` 重试轮 + 嵌入预热（事件驱动恢复语义通道）；
//! 4. Provider 单模型直连：选用哪个模型就用哪个，失败原样上抛并经 [`EventBridge`]
//!    以 `job_failed` 事件提示用户，**不做主备降级**。
//!
//! 密钥纪律：`api_key` 只在 `OpenAiCompatible` 内部用于 `Authorization` 头，本模块不打印、
//! 不回传、不落盘。

use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use visrecall_ai::{keyring, KeySlot, OpenAiCompatible};
use visrecall_domain::providers::{
    EmbeddingProvider, TextInsight, TextProvider, VisionInsight, VisionProvider,
};
use visrecall_domain::AppError;
use visrecall_library::{Library, ModelConfig, ModelEntry};
use visrecall_pipeline::{
    with_builtin_handlers, Pipeline, PipelineConfig, PipelineDeps, PipelineEvents, PipelineHandle,
    PRIORITY_NEW, STAGE_INDEX, STAGE_OCR, STAGE_VLM,
};

/// 槽位的稳定标识（写入 `insights.model_provider`）。
pub const PRIMARY_SLOT: &str = "vlm-primary";

/// 云端 Provider 注册表：当前启用模型只有这一处真相（保存即热更新）。
#[derive(Default)]
pub struct ProviderRegistry {
    active: RwLock<Option<Arc<OpenAiCompatible>>>,
    config: RwLock<ModelConfig>,
}

impl std::fmt::Debug for ProviderRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderRegistry")
            .field("active", &self.active().is_some())
            .finish()
    }
}

impl ProviderRegistry {
    /// 从配置构造（顺手从凭据管理器取密钥；取不到就当作「无 Key」）。
    pub fn from_config(config: &ModelConfig) -> Arc<Self> {
        let registry = Arc::new(Self::default());
        registry.reload(config);
        registry
    }

    /// 热更新：配置保存后调用，无需重启（P3-R11）。
    ///
    /// 选用哪个模型就用哪个：active 模型独占唯一槽位，失败不切备，
    /// `insights.model_provider` 仍是稳定的 `vlm-primary`。
    pub fn reload(&self, config: &ModelConfig) {
        let mut normalized = config.clone();
        normalized.normalize();
        let active = build_provider(PRIMARY_SLOT, normalized.active_entry());
        if let Ok(mut slot) = self.active.write() {
            *slot = active;
        }
        if let Ok(mut slot) = self.config.write() {
            *slot = normalized;
        }
    }

    pub fn config(&self) -> ModelConfig {
        self.config.read().map(|c| c.clone()).unwrap_or_default()
    }

    /// 当前启用的模型 Provider；`None` = 未配置。
    pub fn active(&self) -> Option<Arc<OpenAiCompatible>> {
        self.active.read().ok().and_then(|slot| slot.clone())
    }

    /// 槽位稳定标识（`insights.model_provider`，恒为 `vlm-primary`）。
    pub fn active_slot(&self) -> &'static str {
        PRIMARY_SLOT
    }
}

fn build_provider(
    slot: &'static str,
    entry: Option<&ModelEntry>,
) -> Option<Arc<OpenAiCompatible>> {
    let entry = entry?;
    if entry.validate().is_err() {
        tracing::warn!(slot, base_url = %entry.base_url, "model entry is invalid; ignored");
        return None;
    }
    let key_slot = KeySlot::for_model(&entry.id);
    let api_key = match keyring::load_key(&key_slot) {
        Ok(key) => key,
        Err(err) => {
            tracing::warn!(slot, %err, "failed to load api key from credential store");
            None
        }
    };
    if api_key.is_none() {
        tracing::info!(slot, "no api key configured; provider slot disabled");
    }
    Some(Arc::new(
        OpenAiCompatible::new(slot, entry.base_url.clone(), entry.model.clone(), api_key)
            .with_protocol(&entry.protocol, &entry.protocol_id),
    ))
}

/// 目录批量导入时云端任务的优先级：让新捕获先跑，批量扫描慢慢补。
pub const PRIORITY_BULK_CLOUD: i64 = 100;

/// 文本/片段是否应该进入阶段二：`上传云端解析` 开关打开且来源允许云端。
///
/// 图片恒为 `false`（由 `ocr` handler 接力 `vlm`，不受该开关约束）；
/// 链接没有本地原文，保持原状不在这里放开。
pub fn cloud_text_ai_pending(library: &Library, item: &visrecall_domain::Item) -> bool {
    item.kind == visrecall_domain::ItemKind::Text
        && library.cloud_text_ai_enabled()
        && library.cloud_ai_allowed(&item.id).unwrap_or(false)
}

/// 提交成功后入队第一阶段任务（图片 `ocr`、文本/链接 `index`）。
///
/// **顺序不变量**：文本先入队 `index`（≤3s 可检索），再入队 `vlm`；同一优先级下按 rowid
/// 先进先出，因此阶段一必然先被 claim（P3-R04）。`index` handler 不接力 `vlm`（否则成环），
/// 所以文本的第二阶段只能在这里显式入队——是否入队由 [`cloud_text_ai_pending`] 决定。
///
/// 入队失败只记 `warn`——捕获主链路不为索引让路，素材可由启动自检补索引。
pub fn enqueue_initial_indexing(library: &Library, item: &visrecall_domain::Item) {
    enqueue_indexing_with(library, item, PRIORITY_NEW);
}

/// 目录引用（批量扫描）用：阶段二降到 [`PRIORITY_BULK_CLOUD`]，避免一次扫描灌满云端队列。
pub fn enqueue_bulk_indexing(library: &Library, item: &visrecall_domain::Item) {
    enqueue_indexing_with(library, item, PRIORITY_BULK_CLOUD);
}

fn enqueue_indexing_with(library: &Library, item: &visrecall_domain::Item, cloud_priority: i64) {
    let stage = if item.kind == visrecall_domain::ItemKind::Image {
        STAGE_OCR
    } else {
        STAGE_INDEX
    };
    if let Err(err) = library.enqueue_stage(&item.id, stage, PRIORITY_NEW) {
        tracing::warn!(item = %item.id, stage, %err, "failed to enqueue initial indexing");
    }
    if cloud_text_ai_pending(library, item) {
        if let Err(err) = library.enqueue_stage(&item.id, STAGE_VLM, cloud_priority) {
            tracing::warn!(item = %item.id, %err, "failed to enqueue cloud text analysis");
        }
    }
}

/// 当前启用模型的视觉 Provider：每次调用从注册表现取，配置保存即热生效；
/// 失败原样上抛（重试由 worker 按错误分类处理），提示用户由 [`EventBridge`] 负责。
#[derive(Debug, Clone)]
pub struct RoutedVision {
    registry: Arc<ProviderRegistry>,
}

impl RoutedVision {
    pub fn new(registry: Arc<ProviderRegistry>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl VisionProvider for RoutedVision {
    fn id(&self) -> &str {
        self.registry.active_slot()
    }

    async fn analyze(&self, png: &[u8]) -> Result<VisionInsight, AppError> {
        match self.registry.active() {
            Some(provider) => provider.analyze(png).await,
            None => Err(AppError::ProviderConfig(
                "no vision provider configured (settings → 模型)".to_string(),
            )),
        }
    }
}

/// 当前启用模型的文本 Provider（同 [`RoutedVision`]：单模型直连，不降级）。
#[derive(Debug, Clone)]
pub struct RoutedText {
    registry: Arc<ProviderRegistry>,
}

impl RoutedText {
    pub fn new(registry: Arc<ProviderRegistry>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl TextProvider for RoutedText {
    fn id(&self) -> &str {
        self.registry.active_slot()
    }

    /// 未配置模型 / 无 Key 时 `summarize` 必然失败；提前声明不可用，
    /// 让 `vlm` handler 直接跳过（文本素材保持 ready，不进「待重试」）。
    fn ready(&self) -> bool {
        self.registry.active().is_some()
    }

    async fn summarize(&self, text: &str) -> Result<TextInsight, AppError> {
        match self.registry.active() {
            Some(provider) => provider.summarize(text).await,
            None => Err(AppError::ProviderConfig(
                "no text provider configured (settings → 模型)".to_string(),
            )),
        }
    }
}

/// 把 pipeline 事件适配成前端事件（避免 pipeline 依赖 app-core）。
pub struct EventBridge {
    sink: Arc<dyn crate::events::EventSink>,
    registry: Arc<ProviderRegistry>,
}

impl EventBridge {
    pub fn new(sink: Arc<dyn crate::events::EventSink>, registry: Arc<ProviderRegistry>) -> Self {
        Self { sink, registry }
    }
}

impl PipelineEvents for EventBridge {
    fn pipeline_status(&self, queued: u64, running: u64) {
        self.sink.emit(crate::events::AppEvent::PipelineStatus(
            crate::events::PipelineStatusEvent { queued, running },
        ));
    }

    fn item_indexed(&self, item_id: &str) {
        self.sink.emit(crate::events::AppEvent::ItemIndexed(
            crate::events::ItemEvent {
                id: item_id.to_string(),
            },
        ));
    }

    fn item_enriched(&self, item_id: &str) {
        self.sink.emit(crate::events::AppEvent::ItemEnriched(
            crate::events::ItemEvent {
                id: item_id.to_string(),
            },
        ));
    }

    fn job_failed(&self, job: &visrecall_library::Job, error: &AppError) {
        tracing::warn!(job = %job.id, stage = %job.stage, %error, "pipeline job failed");
        // vlm 阶段在「未配置云端模型」时每条素材都会失败，属预期状态（UI 已注明
        // 「仅本地 OCR」），不打扰；配置了模型才需要把失败明确提示给用户。
        let model = if job.stage == STAGE_VLM {
            let model = self.registry.active().map(|p| p.model().to_string());
            if model.is_none() {
                return;
            }
            model
        } else {
            None
        };
        let (code, message) = error_code_and_message(error);
        self.sink.emit(crate::events::AppEvent::JobFailed(
            crate::events::JobFailedEvent {
                item_id: job.item_id.clone(),
                stage: job.stage.clone(),
                code: code.to_string(),
                message,
                model,
            },
        ));
    }

    fn item_pending(&self, item_id: &str) {
        self.sink.emit(crate::events::AppEvent::ItemUpdated(
            crate::events::ItemUpdatedEvent {
                id: item_id.to_string(),
                fields: vec!["status".to_string()],
            },
        ));
    }
}

/// `AppError` → 稳定错误码 + 细节消息：错误码与 `src-tauri` 的 IPC 映射同口径；
/// 细节消息去掉 Display 前缀（如 `provider unavailable: `），语境由前端文案组装。
fn error_code_and_message(error: &AppError) -> (&'static str, String) {
    let code = match error {
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
        AppError::Busy(_) => "BUSY",
        AppError::Io(_) => "IO",
        AppError::Database(_) => "DATABASE",
        AppError::Internal(_) => "INTERNAL",
        AppError::ModelNotReady(_) => "MODEL_NOT_READY",
        AppError::Provider(_) => "PROVIDER",
        AppError::ProviderConfig(_) => "PROVIDER_CONFIG",
        AppError::SchemaViolation(_) => "SCHEMA_VIOLATION",
    };
    let message = match error {
        AppError::ModelNotReady(message)
        | AppError::Provider(message)
        | AppError::ProviderConfig(message)
        | AppError::SchemaViolation(message) => message.clone(),
        other => other.to_string(),
    };
    (code, message)
}

/// pipeline 启动结果（日志与 `AppStatus` 用）。
pub struct StartedPipeline {
    pub handle: PipelineHandle,
    pub repaired: usize,
}

/// 构造完整的 [`PipelineDeps`] 并启动 worker。
///
/// 顺序（design §7.5）：`recover`（发生在 `Pipeline::spawn` 内）→ 启动自检修复 →
/// 嵌入预热（由调用方触发）→ 检索立即可用（未就绪时语义通道跳过）。
pub fn start_pipeline(
    library: Arc<Library>,
    ocr: Arc<dyn visrecall_domain::providers::OcrProvider>,
    embed: Arc<dyn EmbeddingProvider>,
    registry: Arc<ProviderRegistry>,
    sink: Arc<dyn crate::events::EventSink>,
    runtime: tokio::runtime::Handle,
    config: PipelineConfig,
) -> Result<StartedPipeline, AppError> {
    let deps = Arc::new(PipelineDeps {
        ocr,
        embed,
        vision: Arc::new(RoutedVision::new(Arc::clone(&registry))),
        text: Arc::new(RoutedText::new(Arc::clone(&registry))),
        runtime,
    });

    let events: Arc<dyn PipelineEvents> = Arc::new(EventBridge::new(sink, registry));
    let pipeline = with_builtin_handlers(Pipeline::new(
        Arc::clone(&library),
        deps,
        Arc::clone(&events),
    ))
    .with_config(config);

    let handle = pipeline.spawn()?;

    // 元数据钩子：备注/标签变更 → bump_revision + 入队 index（同事务，design §7.6）
    let hook_library = Arc::clone(&library);
    library.set_metadata_hook(Some(Arc::new(move |ids: &[String]| {
        for id in ids {
            if let Err(err) = hook_library.bump_revision_and_enqueue(id, STAGE_INDEX, PRIORITY_NEW)
            {
                tracing::warn!(item = %id, %err, "failed to enqueue reindex after metadata change");
            }
        }
    })));

    let report = visrecall_pipeline::lifecycle::run_startup_checks(&library)?;
    Ok(StartedPipeline {
        handle,
        repaired: report.repaired,
    })
}
