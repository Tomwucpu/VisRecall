//! pipeline 宿主与 Provider 注册表（design §6.5、§7）。
//!
//! 职责：
//! 1. 构造 [`PipelineDeps`]（OCR 链 / 嵌入 / routed VLM / 文本 Provider / tokio handle）；
//! 2. 启动 2 个 worker、注册四个 handler、注册元数据钩子（bump_revision + 入队 index）；
//! 3. 启动自检修复 + `pending` 重试轮 + 嵌入预热（事件驱动恢复语义通道）；
//! 4. Provider 主备路由：主不健康用备，配置（401/403）**不切备**、不重试。
//!
//! 密钥纪律：`api_key` 只在 `OpenAiCompatible` 内部用于 `Authorization` 头，本模块不打印、
//! 不回传、不落盘。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use visrecall_ai::{keyring, KeySlot, OpenAiCompatible};
use visrecall_domain::providers::{
    EmbeddingProvider, TextInsight, TextProvider, VisionInsight, VisionProvider,
};
use visrecall_domain::AppError;
use visrecall_library::{Library, ModelConfig, ProviderEntry};
use visrecall_pipeline::{
    with_builtin_handlers, Pipeline, PipelineConfig, PipelineDeps, PipelineEvents, PipelineHandle,
    PRIORITY_NEW, STAGE_INDEX, STAGE_OCR,
};

/// 主槽位的稳定标识（写入 `insights.model_provider`）。
pub const PRIMARY_SLOT: &str = "vlm-primary";
/// 备槽位的稳定标识。
pub const BACKUP_SLOT: &str = "vlm-backup";

/// 云端 Provider 的主备注册表：健康状态只有这一处真相（design §7.3）。
#[derive(Default)]
pub struct ProviderRegistry {
    primary: RwLock<Option<Arc<OpenAiCompatible>>>,
    backup: RwLock<Option<Arc<OpenAiCompatible>>>,
    primary_healthy: AtomicBool,
    config: RwLock<ModelConfig>,
}

impl std::fmt::Debug for ProviderRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderRegistry")
            .field("primary", &self.primary().is_some())
            .field("backup", &self.backup().is_some())
            .field(
                "primary_healthy",
                &self.primary_healthy.load(Ordering::Relaxed),
            )
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
    pub fn reload(&self, config: &ModelConfig) {
        let primary = build_provider(PRIMARY_SLOT, config.primary.as_ref(), KeySlot::Primary);
        let backup = build_provider(BACKUP_SLOT, config.backup.as_ref(), KeySlot::Backup);
        if let Ok(mut slot) = self.primary.write() {
            *slot = primary;
        }
        if let Ok(mut slot) = self.backup.write() {
            *slot = backup;
        }
        if let Ok(mut slot) = self.config.write() {
            *slot = config.clone();
        }
        self.primary_healthy.store(true, Ordering::SeqCst);
    }

    pub fn config(&self) -> ModelConfig {
        self.config.read().map(|c| c.clone()).unwrap_or_default()
    }

    pub fn primary(&self) -> Option<Arc<OpenAiCompatible>> {
        self.primary.read().ok().and_then(|slot| slot.clone())
    }

    pub fn backup(&self) -> Option<Arc<OpenAiCompatible>> {
        self.backup.read().ok().and_then(|slot| slot.clone())
    }

    pub fn has_any(&self) -> bool {
        self.primary().is_some() || self.backup().is_some()
    }

    pub fn primary_is_healthy(&self) -> bool {
        self.primary_healthy.load(Ordering::SeqCst)
    }

    pub fn set_primary_healthy(&self, healthy: bool) {
        self.primary_healthy.store(healthy, Ordering::SeqCst);
    }

    /// 当前生效槽位（主健康优先；主缺失或不健康时用备）。
    pub fn active(&self) -> Option<Arc<OpenAiCompatible>> {
        if self.primary_is_healthy() {
            if let Some(provider) = self.primary() {
                return Some(provider);
            }
        }
        self.backup().or_else(|| self.primary())
    }

    /// 当前槽位的稳定标识（`insights.model_provider`）。
    pub fn active_slot(&self) -> &'static str {
        match self.active() {
            Some(provider) if provider.slot() == BACKUP_SLOT => BACKUP_SLOT,
            Some(_) => PRIMARY_SLOT,
            None => PRIMARY_SLOT,
        }
    }
}

fn build_provider(
    slot: &'static str,
    entry: Option<&ProviderEntry>,
    key_slot: KeySlot,
) -> Option<Arc<OpenAiCompatible>> {
    let entry = entry?;
    if entry.validate().is_err() {
        tracing::warn!(slot, base_url = %entry.base_url, "provider entry is invalid; ignored");
        return None;
    }
    let api_key = match keyring::load_key(key_slot) {
        Ok(key) => key,
        Err(err) => {
            tracing::warn!(slot, %err, "failed to load api key from credential store");
            None
        }
    };
    if api_key.is_none() {
        tracing::info!(slot, "no api key configured; provider slot disabled");
    }
    Some(Arc::new(OpenAiCompatible::new(
        slot,
        entry.base_url.clone(),
        entry.model.clone(),
        api_key,
    )))
}

/// 把一个槽位的输入应用到配置（校验地址 → 写/清密钥 → 更新条目）。
///
/// 语义：地址与模型名都为空 = 清空槽位（连同密钥）；`api_key = Some("")` 只清密钥；
/// `api_key = None` 保持密钥不变。密钥只在写凭据管理器时出现在内存里（P3-R11）。
pub fn apply_entry(
    target: &mut Option<ProviderEntry>,
    input: &crate::app::ProviderEntryInput,
    key_slot: KeySlot,
) -> Result<(), AppError> {
    let base_url = input.base_url.trim().to_string();
    let model = input.model.trim().to_string();

    if base_url.is_empty() && model.is_empty() {
        keyring::delete_key(key_slot)?;
        *target = None;
        return Ok(());
    }

    let entry = ProviderEntry { base_url, model };
    entry.validate()?;
    match input.api_key.as_deref() {
        Some("") => keyring::delete_key(key_slot)?,
        Some(key) => keyring::save_key(key_slot, key)?,
        None => {}
    }
    *target = Some(entry);
    Ok(())
}

/// 提交成功后入队第一阶段任务（图片 `ocr`、文本/链接 `index`）。
///
/// **只入队第一阶段**：`index` 与 `vlm` 由 handler 接力入队，保证阶段一先完成（P3-R04）。
/// 入队失败只记 `warn`——捕获主链路不为索引让路，素材可由启动自检补索引。
pub fn enqueue_initial_indexing(library: &Library, item: &visrecall_domain::Item) {
    let stage = if item.kind == visrecall_domain::ItemKind::Image {
        STAGE_OCR
    } else {
        STAGE_INDEX
    };
    if let Err(err) = library.enqueue_stage(&item.id, stage, PRIORITY_NEW) {
        tracing::warn!(item = %item.id, stage, %err, "failed to enqueue initial indexing");
    }
}

/// 主备路由的视觉 Provider。
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
        let primary = self.registry.primary();
        let backup = self.registry.backup();
        let Some(primary) = primary else {
            return try_backup_vision(&self.registry, backup, png).await;
        };
        match primary.analyze(png).await {
            Ok(insight) => {
                self.registry.set_primary_healthy(true);
                Ok(insight)
            }
            // 配置错误：不重试、不切备（同一份用户的地址/密钥假设），交给 UI 提示
            Err(err) if err.is_config_error() => {
                self.registry.set_primary_healthy(false);
                Err(err)
            }
            Err(err) => {
                tracing::warn!(%err, "primary vision provider failed; falling back");
                self.registry.set_primary_healthy(false);
                match try_backup_vision(&self.registry, backup, png).await {
                    Ok(insight) => Ok(insight),
                    Err(_) => Err(err),
                }
            }
        }
    }
}

async fn try_backup_vision(
    registry: &Arc<ProviderRegistry>,
    backup: Option<Arc<OpenAiCompatible>>,
    png: &[u8],
) -> Result<VisionInsight, AppError> {
    match backup {
        Some(provider) => provider.analyze(png).await,
        None => {
            registry.set_primary_healthy(false);
            Err(AppError::ProviderConfig(
                "no vision provider configured (settings → 模型)".to_string(),
            ))
        }
    }
}

/// 主备路由的文本 Provider。
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

    async fn summarize(&self, text: &str) -> Result<TextInsight, AppError> {
        let primary = self.registry.primary();
        let backup = self.registry.backup();
        let Some(primary) = primary else {
            return match backup {
                Some(provider) => provider.summarize(text).await,
                None => Err(AppError::ProviderConfig(
                    "no text provider configured (settings → 模型)".to_string(),
                )),
            };
        };
        match primary.summarize(text).await {
            Ok(insight) => {
                self.registry.set_primary_healthy(true);
                Ok(insight)
            }
            Err(err) if err.is_config_error() => {
                self.registry.set_primary_healthy(false);
                Err(err)
            }
            Err(err) => {
                self.registry.set_primary_healthy(false);
                match backup {
                    Some(provider) => provider.summarize(text).await,
                    None => Err(err),
                }
            }
        }
    }
}

/// 把 pipeline 事件适配成前端事件（避免 pipeline 依赖 app-core）。
pub struct EventBridge {
    sink: Arc<dyn crate::events::EventSink>,
}

impl EventBridge {
    pub fn new(sink: Arc<dyn crate::events::EventSink>) -> Self {
        Self { sink }
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

/// pipeline 启动结果（日志与 `AppStatus` 用）。
pub struct StartedPipeline {
    pub handle: PipelineHandle,
    pub repaired: usize,
    pub pending_retried: usize,
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
        text: Arc::new(RoutedText::new(registry)),
        runtime,
    });

    let events: Arc<dyn PipelineEvents> = Arc::new(EventBridge::new(sink));
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
        pending_retried: report.pending_retried,
    })
}
