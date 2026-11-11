//! Tauri 命令、IPC DTO 与事件发送端。
//!
//! DTO 定义在这里而不是 domain，避免领域层依赖 specta/Tauri。

use serde::{Deserialize, Serialize};
use tauri::{Emitter, Manager};
use visrecall_app_core::{AppCore, AppError, EventSink, Library};
use visrecall_domain::storage::{OrphanStats, PruneReport, RetentionPreview, StorageUsage};
use visrecall_domain::tag::{ItemTag, TagSource};

use visrecall_app_core::AppEvent;

/// IPC 错误 DTO；`code` 为稳定错误码，前端据此分支，`message` 面向日志/提示。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct ApiError {
    pub code: String,
    pub message: String,
}

impl ApiError {
    pub(crate) fn new(code: &str, message: impl Into<String>) -> ApiError {
        ApiError {
            code: code.to_string(),
            message: message.into(),
        }
    }
}

/// [`AppError`] → 稳定错误码。用引用实现是因为 `AppError` 存在
/// `AppCore::library` 的 `Result` 里，不能 clone。
impl From<&AppError> for ApiError {
    fn from(err: &AppError) -> ApiError {
        let (code, message) = match err {
            AppError::ConfigMissing => ("CONFIG_MISSING", err.to_string()),
            AppError::InvalidInput(detail) => ("INVALID_INPUT", detail.clone()),
            AppError::NotFound => ("NOT_FOUND", err.to_string()),
            AppError::StorageFull => ("STORAGE_FULL", err.to_string()),
            AppError::CaptureFailed(detail) => ("CAPTURE_FAILED", detail.clone()),
            AppError::CaptureCancelled => ("CAPTURE_CANCELLED", err.to_string()),
            AppError::DrmProtected => ("DRM_PROTECTED", err.to_string()),
            AppError::FileMissing(detail) => ("FILE_MISSING", detail.clone()),
            AppError::ClipboardBusy => ("CLIPBOARD_BUSY", err.to_string()),
            AppError::ClipboardFailed(detail) => ("CLIPBOARD_FAILED", detail.clone()),
            AppError::Busy(detail) => ("BUSY", detail.clone()),
            AppError::Io(detail) => ("IO", detail.clone()),
            AppError::Database(detail) => ("DATABASE", detail.clone()),
            AppError::Internal(detail) => ("INTERNAL", detail.clone()),
            AppError::ModelNotReady(detail) => ("MODEL_NOT_READY", detail.clone()),
            AppError::Provider(detail) => ("PROVIDER", detail.clone()),
            AppError::ProviderConfig(detail) => ("PROVIDER_CONFIG", detail.clone()),
            AppError::SchemaViolation(detail) => ("SCHEMA_VIOLATION", detail.clone()),
        };
        ApiError::new(code, message)
    }
}

/// 列表/详情共用的素材 DTO；`thumb_url` 只在存在缩略图时由后端拼接。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct ItemDto {
    pub id: String,
    pub kind: String,
    pub status: String,
    pub captured_at: i64,
    pub source_app: Option<String>,
    pub source_title: Option<String>,
    /// 用户可编辑标题；`null` 表示未改名，界面回落到 `source_title`。
    pub title: Option<String>,
    pub source_url: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub char_count: Option<u32>,
    pub note: String,
    pub thumb_url: Option<String>,
    /// 主色（`insights.primary_color`；未解析或未给色为 `null`）。P3-R10 的卡片色点用。
    pub primary_color: Option<String>,
    /// 卡片缩略区的文本预览（文本原文 / 链接 URL 的前 160 字符）；图片或读不到为 `null`。
    pub preview: Option<String>,
}

impl From<visrecall_domain::Item> for ItemDto {
    fn from(item: visrecall_domain::Item) -> ItemDto {
        let thumb_url = item
            .thumb_path
            .as_ref()
            .map(|_| format!("visrecall://thumb/{}?rev={}", item.id, item.revision));
        ItemDto {
            id: item.id,
            kind: kind_str(item.kind).to_string(),
            status: status_str(item.status).to_string(),
            captured_at: item.captured_at,
            source_app: item.source_app,
            source_title: item.source_title,
            title: item.title,
            source_url: item.source_url,
            width: item.width,
            height: item.height,
            char_count: item.char_count,
            note: item.note,
            thumb_url,
            primary_color: item.primary_color,
            // 预览要读原文文件，`From<Item>` 拿不到库；由返回卡片的命令用
            // [`dto_with_preview`] 补填。
            preview: None,
        }
    }
}

/// 给列表/检索卡片补上缩略区预览（`read_preview` 需要读 `texts/` 下的原文头部）。
fn dto_with_preview(library: &Library, item: visrecall_domain::Item) -> ItemDto {
    let preview = visrecall_app_core::read_preview(library, &item);
    let mut dto = ItemDto::from(item);
    dto.preview = preview;
    dto
}

// ----- 阶段 03：检索相关 DTO（design §5.2） -----

/// 一个 OCR 包围盒（归一化到 `[0,1]`，前端按百分比定位）。
#[derive(Debug, Clone, PartialEq, Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct OcrBoxDto {
    pub text: String,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl From<visrecall_domain::OcrBox> for OcrBoxDto {
    fn from(obox: visrecall_domain::OcrBox) -> Self {
        Self {
            text: obox.text,
            x: obox.x,
            y: obox.y,
            w: obox.w,
            h: obox.h,
        }
    }
}

/// 时间范围（毫秒，左闭右开）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct TimeRangeDto {
    pub from: i64,
    pub to: i64,
}

/// 解析后的查询（前端 chips 与降级标注用）。
#[derive(Debug, Clone, PartialEq, Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct ParsedQueryDto {
    pub time: Option<TimeRangeDto>,
    pub app: Option<String>,
    pub color: Vec<String>,
    pub chart: Option<String>,
    pub content_kind: Option<String>,
    pub has_formula: Option<bool>,
    pub tags: Vec<String>,
    pub semantic_text: String,
    /// 人类可读的条件标签（后端 `ParsedQuery::chips`）。
    pub chips: Vec<String>,
}

impl From<visrecall_domain::search::ParsedQuery> for ParsedQueryDto {
    fn from(parsed: visrecall_domain::search::ParsedQuery) -> Self {
        let chips = parsed.chips();
        Self {
            time: parsed.time.map(|time| TimeRangeDto {
                from: time.from,
                to: time.to,
            }),
            app: parsed.app,
            color: parsed.color.unwrap_or_default(),
            chart: parsed.chart,
            content_kind: parsed.content_kind.map(|kind| kind.as_str().to_string()),
            has_formula: parsed.has_formula,
            tags: parsed.tags,
            semantic_text: parsed.semantic_text,
            chips,
        }
    }
}

/// 界面筛选条的结构化条件；组内 OR、组间 AND，空数组表示该维度不限制。
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct ItemFiltersDto {
    #[serde(default)]
    pub kinds: Vec<String>,
    #[serde(default)]
    pub statuses: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub time_from: Option<i64>,
    #[serde(default)]
    pub time_to: Option<i64>,
}

impl ItemFiltersDto {
    /// 转成领域类型：未知枚举值直接忽略，不让一个脏值把整次检索打成错误。
    fn to_domain(&self) -> visrecall_domain::ItemFilters {
        visrecall_domain::ItemFilters {
            kinds: self
                .kinds
                .iter()
                .filter_map(|raw| visrecall_domain::ItemKind::parse(raw))
                .collect(),
            statuses: self
                .statuses
                .iter()
                .filter_map(|raw| visrecall_domain::ItemStatus::parse(raw))
                .collect(),
            tags: self
                .tags
                .iter()
                .map(|tag| tag.trim().to_string())
                .filter(|tag| !tag.is_empty())
                .collect(),
            time_from: self.time_from,
            time_to: self.time_to,
        }
    }
}

/// 命中解释（ExplainBar 用）。
#[derive(Debug, Clone, PartialEq, Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct ExplanationDto {
    pub filters: Vec<String>,
    pub terms: Vec<String>,
    pub semantic: bool,
    pub highlight_boxes: Vec<OcrBoxDto>,
}

/// 一条检索结果。
#[derive(Debug, Clone, PartialEq, Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct SearchHitDto {
    pub item: ItemDto,
    pub score: f32,
    pub explanation: ExplanationDto,
}

/// `search_items` 的响应。
#[derive(Debug, Clone, PartialEq, Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct SearchResponseDto {
    pub query_id: String,
    pub parsed: ParsedQueryDto,
    pub semantic_used: bool,
    pub took_ms: u64,
    /// `limit` 截断前的命中数。
    pub total: usize,
    pub hits: Vec<SearchHitDto>,
}

/// 模型探测结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct ProbeOutcomeDto {
    pub ok: bool,
    pub latency_ms: u32,
    pub vision: bool,
    pub message: Option<String>,
}

impl From<visrecall_ai::ProbeOutcome> for ProbeOutcomeDto {
    fn from(outcome: visrecall_ai::ProbeOutcome) -> Self {
        Self {
            ok: outcome.ok,
            latency_ms: outcome.latency_ms,
            vision: outcome.vision,
            message: outcome.message,
        }
    }
}

fn kind_str(kind: visrecall_domain::ItemKind) -> &'static str {
    match kind {
        visrecall_domain::ItemKind::Image => "image",
        visrecall_domain::ItemKind::Text => "text",
        visrecall_domain::ItemKind::Link => "link",
    }
}

fn status_str(status: visrecall_domain::ItemStatus) -> &'static str {
    match status {
        visrecall_domain::ItemStatus::Parsing => "parsing",
        visrecall_domain::ItemStatus::Ready => "ready",
        visrecall_domain::ItemStatus::Pending => "pending",
        visrecall_domain::ItemStatus::Failed => "failed",
    }
}

/// `get_app_status` 的返回值；库打不开时 `library_ok = false`，路径仍返回给界面对账。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct AppStatus {
    pub version: String,
    pub library_path: String,
    pub library_ok: bool,
    /// 库已迁移、需要重启才能沿用新库；为真期间捕获被 `BUSY` 拒绝。
    pub restart_required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct SettingsDto {
    pub library_path: String,
    pub capture_hotkey: String,
    pub ball_enabled: bool,
    pub ball_size: u32,
    /// 贴边收起开关。
    pub ball_edge_hide: bool,
    /// 全屏自动隐藏开关。
    pub hide_in_fullscreen: bool,
    /// 库里保存的容量（GiB）；库不可用时为默认值。
    pub capacity_gb: u32,
    /// 库里保存的保留天数；`null` 为永久保留。
    pub retention_days: Option<u32>,
    /// 文本/片段是否允许上传云端解析；库不可用时为默认值（关闭）。
    pub cloud_text_ai: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct SettingsPatch {
    pub ball_enabled: Option<bool>,
    pub ball_size: Option<u32>,
    pub capture_hotkey: Option<String>,
    /// 贴边收起；`None` 表示不改。
    pub ball_edge_hide: Option<bool>,
    /// 全屏自动隐藏；`None` 表示不改。
    pub hide_in_fullscreen: Option<bool>,
}

impl SettingsDto {
    /// 配置类字段来自 `config.json`，策略类字段（容量/保留天数）来自库内 `settings` 表
    /// —— 后者属于「这个库」，同一条命令一起返回，设置页才不需要额外的读取命令。
    fn from_config(
        config: &visrecall_app_core::AppConfig,
        library: Option<&Library>,
    ) -> SettingsDto {
        let capacity_gb = library
            .map(|library| (library.capacity_bytes() / (1024 * 1024 * 1024)) as u32)
            .unwrap_or(visrecall_domain::storage::DEFAULT_CAPACITY_GB);
        let retention_days = library.and_then(|library| library.retention().days);
        // 隐私保守：库不可用（读不到策略）时按「不上传」处理。
        let cloud_text_ai = library.is_some_and(Library::cloud_text_ai_enabled);

        SettingsDto {
            library_path: config.library_path.display().to_string(),
            capture_hotkey: config.hotkeys.capture.clone(),
            ball_enabled: config.ball.enabled,
            ball_size: config.ball.size,
            ball_edge_hide: config.ball.edge_hide,
            hide_in_fullscreen: config.ball.hide_in_fullscreen,
            capacity_gb,
            retention_days,
            cloud_text_ai,
        }
    }
}

/// 存储用量快照。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct StorageUsageDto {
    pub used_bytes: u64,
    pub capacity_bytes: u64,
    pub item_count: u64,
}

impl From<StorageUsage> for StorageUsageDto {
    fn from(usage: StorageUsage) -> StorageUsageDto {
        StorageUsageDto {
            used_bytes: usage.used_bytes,
            capacity_bytes: usage.capacity_bytes,
            item_count: usage.item_count,
        }
    }
}

/// 详情面板的标签 chip；`auto` 表示「这次关联是自动标注加的」。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct TagChipDto {
    pub id: String,
    pub name: String,
    /// 关联来源是否为 `auto`；JSON 键就是 `auto`。
    pub auto: bool,
    /// 色令牌名（`ochre`/`slate`/…）；前端只做令牌 → CSS 变量映射。
    pub hue: Option<String>,
    /// 标签云的素材数量；按素材查询时为 0。
    pub item_count: u64,
}

impl From<ItemTag> for TagChipDto {
    fn from(chip: ItemTag) -> TagChipDto {
        TagChipDto {
            id: chip.tag.id,
            name: chip.tag.name,
            auto: chip.origin == TagSource::Auto,
            hue: Some(chip.hue.to_string()),
            item_count: chip.item_count,
        }
    }
}

/// 详情面板的额外内容。
#[derive(Debug, Clone, PartialEq, Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct DetailExtrasDto {
    /// AI 摘要；未解析为 `None`（面板显示占位）。
    pub summary: Option<String>,
    /// 摘要来源：`vlm` / `ocr` / `extractive` / `user` / `none`。
    pub summary_source: String,
    /// OCR 归一化包围盒（图片素材；无文字为空）。
    pub ocr_boxes: Vec<OcrBoxDto>,
    /// 文本素材的原文（前 20000 字符，超出部分已截断）。
    pub body_text: Option<String>,
    /// 图片素材的原图协议 URL；文本/链接为 `None`。
    pub original_url: Option<String>,
    pub tags: Vec<TagChipDto>,
    pub reference: Option<ReferenceInfoDto>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct ReferenceInfoDto {
    pub source_id: String,
    pub source_path: String,
    pub relative_path: String,
    pub status: String,
    pub last_indexed_at: Option<i64>,
}

/// 详情聚合结果。
#[derive(Debug, Clone, PartialEq, Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct ItemDetailDto {
    pub item: ItemDto,
    pub extras: DetailExtrasDto,
}

impl From<visrecall_app_core::ItemDetail> for ItemDetailDto {
    fn from(detail: visrecall_app_core::ItemDetail) -> ItemDetailDto {
        ItemDetailDto {
            item: ItemDto::from(detail.item),
            extras: DetailExtrasDto {
                summary: detail.extras.summary,
                summary_source: detail.extras.summary_source,
                ocr_boxes: detail
                    .extras
                    .ocr_boxes
                    .into_iter()
                    .map(OcrBoxDto::from)
                    .collect(),
                body_text: detail.extras.body_text,
                original_url: detail.extras.original_url,
                tags: detail
                    .extras
                    .tags
                    .into_iter()
                    .map(TagChipDto::from)
                    .collect(),
                reference: None,
            },
        }
    }
}

/// 全库清空的确认信息。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct PurgePreview {
    pub item_count: u64,
    pub confirmation_word: String,
}

/// 清理预估。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct RetentionPreviewDto {
    pub item_count: u64,
    pub bytes: u64,
}

impl From<RetentionPreview> for RetentionPreviewDto {
    fn from(preview: RetentionPreview) -> RetentionPreviewDto {
        RetentionPreviewDto {
            item_count: preview.item_count,
            bytes: preview.bytes,
        }
    }
}

/// 清理结果：成功 N / 失败 M。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct PruneReportDto {
    pub deleted: u64,
    pub failed: u64,
}

impl From<PruneReport> for PruneReportDto {
    fn from(report: PruneReport) -> PruneReportDto {
        PruneReportDto {
            deleted: report.deleted,
            failed: report.failed,
        }
    }
}

/// 孤儿文件与缺失缩略图统计。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct OrphanStatsDto {
    pub files: u64,
    pub bytes: u64,
    pub missing_thumbs: u64,
}

impl From<OrphanStats> for OrphanStatsDto {
    fn from(stats: OrphanStats) -> OrphanStatsDto {
        OrphanStatsDto {
            files: stats.files,
            bytes: stats.bytes,
            missing_thumbs: stats.missing_thumbs,
        }
    }
}

/// 库迁移结果：必须重启才生效。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct RelocateResultDto {
    pub restart_required: bool,
    pub new_root: String,
}

/// 把 `app-core` 的事件转成 Tauri 事件；前端只订阅，不从 Rust 侧读取状态。
pub struct TauriEventSink {
    app: tauri::AppHandle,
}

impl TauriEventSink {
    pub fn new(app: tauri::AppHandle) -> TauriEventSink {
        TauriEventSink { app }
    }
}

impl EventSink for TauriEventSink {
    fn emit(&self, event: AppEvent) {
        // 阶段 01 没有设置页：热键冲突除了发事件，还要在托盘提示上可见。
        if let AppEvent::HotkeyConflict(payload) = &event {
            match self.app.tray_by_id(crate::tray::TRAY_ID) {
                Some(tray) => {
                    let tooltip = format!("目记 VisRecall — 快捷键不可用：{}", payload.message);
                    match tray.set_tooltip(Some(tooltip)) {
                        Ok(()) => {
                            tracing::info!(message = %payload.message, "hotkey conflict shown on tray")
                        }
                        Err(err) => tracing::warn!(error = %err, "failed to update tray tooltip"),
                    }
                }
                None => tracing::warn!("tray icon missing; cannot show hotkey conflict"),
            }
        }

        // 捕获失败同理：阶段 01 还没有前端提示，托盘 tooltip 是唯一的用户可见反馈。
        if let AppEvent::CaptureFailed(payload) = &event {
            match self.app.tray_by_id(crate::tray::TRAY_ID) {
                Some(tray) => {
                    let tooltip = capture_failed_tooltip(&payload.code, &payload.message);
                    match tray.set_tooltip(Some(tooltip)) {
                        Ok(()) => {
                            tracing::info!(code = %payload.code, "capture failure shown on tray")
                        }
                        Err(err) => tracing::warn!(error = %err, "failed to update tray tooltip"),
                    }
                }
                None => tracing::warn!("tray icon missing; cannot show capture failure"),
            }
        }

        // 改键成功后要把上一次的冲突提示从托盘上撤掉，否则托盘会一直停在旧文案。
        if matches!(event, AppEvent::HotkeyRestored) {
            match self.app.tray_by_id(crate::tray::TRAY_ID) {
                Some(tray) => {
                    if let Err(err) = tray.set_tooltip(Some(TRAY_DEFAULT_TOOLTIP)) {
                        tracing::warn!(error = %err, "failed to reset tray tooltip");
                    }
                }
                None => tracing::warn!("tray icon missing; cannot reset tooltip"),
            }
        }

        let name = event.name();
        if let Err(err) = self.app.emit(name, event.payload()) {
            tracing::warn!(event = name, error = %err, "failed to emit event");
        }
    }
}

/// 托盘恢复正常状态时的提示文案（与 `tray::create` 一致）。
const TRAY_DEFAULT_TOOLTIP: &str = "目记 VisRecall";

/// 托盘 tooltip 的字符上限（Windows 通知区域提示约 128 字符，留出余量）。
const TOOLTIP_MAX_CHARS: usize = 120;

/// `capture_failed` 的托盘提示文本：`目记 VisRecall — <message> (<code>)`。
///
/// 错误信息可能很长（例如带完整路径的 IO 错误），超出上限时截断并加省略号，
/// 避免 `set_tooltip` 因长度失败导致反馈丢失。
fn capture_failed_tooltip(code: &str, message: &str) -> String {
    let text = format!("目记 VisRecall — {message} ({code})");
    if text.chars().count() <= TOOLTIP_MAX_CHARS {
        return text;
    }
    let mut truncated: String = text.chars().take(TOOLTIP_MAX_CHARS).collect();
    truncated.push('…');
    truncated
}

#[tauri::command]
#[specta::specta]
pub async fn get_item_ext(app: tauri::AppHandle, id: String) -> Result<ItemDetailDto, ApiError> {
    run_blocking(app, move |core| {
        let mut detail = core
            .detail(&id)
            .map(ItemDetailDto::from)
            .map_err(|err| ApiError::from(&err))?;
        let library = core.library().map_err(ApiError::from)?;
        if let Some(asset) = library
            .external_asset_for_item(&id)
            .map_err(|err| ApiError::from(&err))?
        {
            let source = library
                .get_import_source(&asset.source_id)
                .map_err(|err| ApiError::from(&err))?;
            detail.extras.reference = Some(ReferenceInfoDto {
                source_id: asset.source_id,
                source_path: source.map(|source| source.root_path).unwrap_or_default(),
                relative_path: asset.relative_path,
                status: asset.source_status,
                last_indexed_at: asset.indexed_at,
            });
        }
        Ok(detail)
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn note_set(app: tauri::AppHandle, id: String, text: String) -> Result<(), ApiError> {
    run_blocking(app, move |core| {
        core.set_note(&id, &text)
            .map_err(|err| ApiError::from(&err))
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn tags_list(app: tauri::AppHandle) -> Result<Vec<TagChipDto>, ApiError> {
    run_blocking(app, move |core| {
        let library = core.library().map_err(ApiError::from)?;
        let tags = library.tags_list().map_err(|err| ApiError::from(&err))?;
        Ok(tags.into_iter().map(TagChipDto::from).collect())
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn tag_add(
    app: tauri::AppHandle,
    item_id: String,
    name: String,
) -> Result<TagChipDto, ApiError> {
    run_blocking(app, move |core| {
        core.add_tag(&item_id, &name)
            .map(TagChipDto::from)
            .map_err(|err| ApiError::from(&err))
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn tag_detach(
    app: tauri::AppHandle,
    item_id: String,
    tag_id: String,
) -> Result<(), ApiError> {
    run_blocking(app, move |core| {
        core.detach_tag(&item_id, &tag_id)
            .map_err(|err| ApiError::from(&err))
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn tag_rename(
    app: tauri::AppHandle,
    tag_id: String,
    name: String,
) -> Result<(), ApiError> {
    run_blocking(app, move |core| {
        core.rename_tag(&tag_id, &name)
            .map_err(|err| ApiError::from(&err))
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn tag_delete(app: tauri::AppHandle, tag_id: String) -> Result<u64, ApiError> {
    run_blocking(app, move |core| {
        core.delete_tag(&tag_id).map_err(|err| ApiError::from(&err))
    })
    .await
}

/// 复制原始内容：图片 `CF_DIB` + `PNG`，文本/链接 `CF_UNICODETEXT`。
#[tauri::command]
#[specta::specta]
pub async fn copy_item_original(app: tauri::AppHandle, item_id: String) -> Result<(), ApiError> {
    run_blocking(app, move |core| {
        core.copy_original(&item_id)
            .map_err(|err| ApiError::from(&err))
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn item_delete(app: tauri::AppHandle, id: String) -> Result<(), ApiError> {
    run_blocking(app, move |core| {
        core.delete_item(&id).map_err(|err| ApiError::from(&err))
    })
    .await
}

/// 批量删除结果：成功/失败条数（`NotFound` 计入成功，保证重试幂等）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct BatchDeleteReportDto {
    pub deleted: u64,
    pub failed: u64,
}

#[tauri::command]
#[specta::specta]
pub async fn item_rename(
    app: tauri::AppHandle,
    id: String,
    title: Option<String>,
) -> Result<ItemDto, ApiError> {
    run_blocking(app, move |core| {
        let library = core.library().map_err(ApiError::from)?;
        let item = core
            .rename_item(&id, title.as_deref())
            .map_err(|err| ApiError::from(&err))?;
        Ok(dto_with_preview(library, item))
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn item_delete_many(
    app: tauri::AppHandle,
    ids: Vec<String>,
) -> Result<BatchDeleteReportDto, ApiError> {
    run_blocking(app, move |core| {
        let report = core.delete_items(&ids).map_err(|err| ApiError::from(&err))?;
        Ok(BatchDeleteReportDto {
            deleted: report.deleted,
            failed: report.failed,
        })
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn item_reparse(app: tauri::AppHandle, id: String) -> Result<(), ApiError> {
    run_blocking(app, move |core| {
        core.reparse_item(&id).map_err(|err| ApiError::from(&err))
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn library_purge_preview(app: tauri::AppHandle) -> Result<PurgePreview, ApiError> {
    run_blocking(app, move |core| {
        let item_count = core.purge_preview().map_err(|err| ApiError::from(&err))?;
        Ok(PurgePreview {
            item_count,
            confirmation_word: AppCore::PURGE_CONFIRMATION_WORD.to_string(),
        })
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn library_purge(app: tauri::AppHandle, confirm: String) -> Result<(), ApiError> {
    run_blocking(app, move |core| {
        core.purge(&confirm)
            .map(|_report| ())
            .map_err(|err| ApiError::from(&err))
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn storage_usage_get(app: tauri::AppHandle) -> Result<StorageUsageDto, ApiError> {
    run_blocking(app, move |core| {
        core.usage()
            .map(StorageUsageDto::from)
            .map_err(|err| ApiError::from(&err))
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn storage_capacity_set(app: tauri::AppHandle, gigabytes: u32) -> Result<(), ApiError> {
    run_blocking(app, move |core| {
        core.set_capacity_gb(gigabytes)
            .map_err(|err| ApiError::from(&err))
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn retention_set(app: tauri::AppHandle, days: Option<u32>) -> Result<(), ApiError> {
    run_blocking(app, move |core| {
        core.set_retention(days).map_err(|err| ApiError::from(&err))
    })
    .await
}

/// 文本/片段是否允许上传云端解析；库内策略，默认关闭。
#[tauri::command]
#[specta::specta]
pub async fn cloud_text_ai_set(app: tauri::AppHandle, enabled: bool) -> Result<(), ApiError> {
    run_blocking(app, move |core| {
        core.set_cloud_text_ai(enabled)
            .map_err(|err| ApiError::from(&err))
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn retention_preview(
    app: tauri::AppHandle,
    days: Option<u32>,
) -> Result<RetentionPreviewDto, ApiError> {
    run_blocking(app, move |core| {
        core.retention_preview(days)
            .map(RetentionPreviewDto::from)
            .map_err(|err| ApiError::from(&err))
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn retention_apply(app: tauri::AppHandle) -> Result<PruneReportDto, ApiError> {
    run_blocking(app, move |core| {
        Ok(PruneReportDto::from(core.prune_expired()))
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn storage_orphans_get(app: tauri::AppHandle) -> Result<OrphanStatsDto, ApiError> {
    run_blocking(app, move |core| {
        let library = core.library().map_err(ApiError::from)?;
        visrecall_app_core::storage_flow::orphan_stats(library)
            .map(OrphanStatsDto::from)
            .map_err(|err| ApiError::from(&err))
    })
    .await
}

/// 清理孤儿文件：只删 `originals/`、`texts/` 下未被引用的文件。
///
/// 不发事件（孤儿文件不属于任何素材），用量变化由前端在返回后重新拉取。
#[tauri::command]
#[specta::specta]
pub async fn storage_orphans_cleanup(app: tauri::AppHandle) -> Result<OrphanStatsDto, ApiError> {
    run_blocking(app, move |core| {
        let library = core.library().map_err(ApiError::from)?;
        visrecall_app_core::storage_flow::cleanup_orphans(library)
            .map(OrphanStatsDto::from)
            .map_err(|err| ApiError::from(&err))
    })
    .await
}

/// 库位置迁移：校验目标 → 停运行时 → checkpoint → 复制 → 校验 → 写 config。
///
/// 成功后 `config.json` 已指向新目录，但**当前进程仍用旧库**，必须重启才生效
/// （不实现热切换）；因此在返回 `restartRequired = true` 的同时闸门停在
/// `RestartPending`，期间的捕获/拖放都会被 `BUSY` 拒绝。
///
/// 迁移是分钟级的重 IO，必须离开主线程：`run_blocking` 里做的 `spawn_blocking`。
#[tauri::command]
#[specta::specta]
pub async fn storage_relocate(
    app: tauri::AppHandle,
    path: String,
) -> Result<RelocateResultDto, ApiError> {
    run_blocking(app, move |core| {
        let target = std::path::PathBuf::from(&path);
        core.relocate_library(target)
            .map(|result| RelocateResultDto {
                restart_required: result.restart_required,
                new_root: result.new_root,
            })
            .map_err(|err| ApiError::from(&err))
    })
    .await
}

/// 目录选择器：由 Rust 侧调用 dialog 插件，前端只拿到路径字符串。
///
/// 这样不必给前端开放文件系统权限或新增 capability——与 `open_library_folder`
/// 的「路径只在 Rust 侧流转」约定一致。用户取消返回 `None`。
#[tauri::command]
#[specta::specta]
pub async fn pick_directory(app: tauri::AppHandle) -> Result<Option<String>, ApiError> {
    use tauri_plugin_dialog::DialogExt;

    let (tx, rx) = std::sync::mpsc::channel();
    app.dialog().file().pick_folder(move |folder| {
        let _ = tx.send(folder);
    });

    // `pick_folder` 不返回「是否成功打开」，因此给等待加一个上限：对话框若因
    // 平台原因没有起来，回调永远不会到达，不能让 blocking 线程永久挂住。
    let picked = tauri::async_runtime::spawn_blocking(move || {
        rx.recv_timeout(std::time::Duration::from_secs(600))
            .ok()
            .flatten()
    })
    .await
    .map_err(|err| ApiError::new("INTERNAL", format!("blocking task failed: {err}")))?;

    Ok(picked
        .and_then(|path| path.into_path().ok())
        .map(|path| path.display().to_string()))
}

/// 立即重启进程；只应在 `restartRequired = true` 时由横幅按钮调用。
#[tauri::command]
#[specta::specta]
pub async fn restart_app(app: tauri::AppHandle) -> Result<(), ApiError> {
    app.restart();
}

/// 所有阶段 01 命令的注册点；命令同时带 `#[tauri::command]` 与 `#[specta::specta]`。
/// 检索（三路召回 + RRF）；解析是纯规则快路径，不阻塞。
#[tauri::command]
#[specta::specta]
pub async fn search_items(
    app: tauri::AppHandle,
    raw: String,
    limit: Option<u32>,
    filters: Option<ItemFiltersDto>,
) -> Result<SearchResponseDto, ApiError> {
    let limit = limit.unwrap_or(50);
    let filters = filters.unwrap_or_default().to_domain();
    run_blocking(app, move |core| {
        let library = core.library().map_err(ApiError::from)?;
        let outcome = core
            .search(&raw, limit, &filters)
            .map_err(|err| ApiError::from(&err))?;
        let hits = outcome
            .hits
            .into_iter()
            .map(|hit| SearchHitDto {
                item: dto_with_preview(library, hit.item),
                score: hit.score,
                explanation: ExplanationDto {
                    filters: hit.explanation.filters,
                    terms: hit.explanation.terms,
                    semantic: hit.explanation.semantic,
                    highlight_boxes: hit
                        .explanation
                        .highlight_boxes
                        .into_iter()
                        .map(OcrBoxDto::from)
                        .collect(),
                },
            })
            .collect();
        Ok(SearchResponseDto {
            query_id: outcome.query_id,
            parsed: ParsedQueryDto::from(outcome.parsed),
            semantic_used: outcome.semantic_used,
            took_ms: outcome.took_ms,
            total: outcome.total,
            hits,
        })
    })
    .await
}

/// 「不是这个」反馈；`queryId` 不存在返回 `NOT_FOUND`。
#[tauri::command]
#[specta::specta]
pub async fn search_feedback(
    app: tauri::AppHandle,
    query_id: String,
    item_id: String,
    signal: String,
) -> Result<(), ApiError> {
    run_blocking(app, move |core| {
        // `item_id` 目前只记日志：V0.1 的反馈粒度是「整个查询」，字段留给 V0.5 的逐条反馈。
        tracing::info!(%query_id, %item_id, %signal, "search feedback");
        core.search_feedback(&query_id, &signal)
            .map_err(|err| ApiError::from(&err))
    })
    .await
}

/// 一个自定义模型的读取视图：**永远不含密钥**，只告诉界面「是否已设置」。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct ModelEntryDto {
    pub id: String,
    pub name: String,
    pub model: String,
    pub base_url: String,
    pub protocol: String,
    pub protocol_id: String,
    pub has_api_key: bool,
}

/// 模型配置读取视图：列表 + 当前启用项。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct ModelConfigDto {
    pub entries: Vec<ModelEntryDto>,
    pub active_id: Option<String>,
}

/// 模型输入：`api_key` 缺省 = 不改；空串 = 清除；非空 = 覆盖写入凭据管理器。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct ModelEntryInputDto {
    pub id: String,
    pub name: String,
    pub model: String,
    pub base_url: String,
    pub protocol: String,
    pub protocol_id: String,
    pub api_key: Option<String>,
}

/// 模型配置整表提交：一次写入完整列表与当前启用项。
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct ModelConfigPatchDto {
    pub entries: Vec<ModelEntryInputDto>,
    pub active_id: Option<String>,
}

fn model_config_dto(config: &visrecall_app_core::ModelConfig) -> ModelConfigDto {
    let entries = config
        .entries
        .iter()
        .map(|entry| ModelEntryDto {
            id: entry.id.clone(),
            name: entry.name.clone(),
            model: entry.model.clone(),
            base_url: entry.base_url.clone(),
            protocol: entry.protocol.clone(),
            protocol_id: entry.protocol_id.clone(),
            // 只看「有没有」，绝不回传内容
            has_api_key: visrecall_ai::load_key(&visrecall_ai::KeySlot::for_model(&entry.id))
                .ok()
                .flatten()
                .is_some(),
        })
        .collect();
    ModelConfigDto {
        entries,
        active_id: config.active_entry().map(|entry| entry.id.clone()),
    }
}

/// 读取模型配置（自定义模型列表 + 是否已设置密钥）。
#[tauri::command]
#[specta::specta]
pub async fn get_model_config(app: tauri::AppHandle) -> Result<ModelConfigDto, ApiError> {
    run_blocking(app, move |core| Ok(model_config_dto(&core.model_config()))).await
}

/// 保存模型列表：配置写 `config.json`，密钥写凭据管理器，保存即热生效。
#[tauri::command]
#[specta::specta]
pub async fn set_model_config(
    app: tauri::AppHandle,
    patch: ModelConfigPatchDto,
) -> Result<ModelConfigDto, ApiError> {
    run_blocking(app, move |core| {
        let patch = visrecall_app_core::ModelConfigPatch {
            entries: patch
                .entries
                .into_iter()
                .map(|entry| visrecall_app_core::ModelEntryInput {
                    id: entry.id,
                    name: entry.name,
                    model: entry.model,
                    base_url: entry.base_url,
                    protocol: entry.protocol,
                    protocol_id: entry.protocol_id,
                    api_key: entry.api_key,
                })
                .collect(),
            active_id: patch.active_id,
        };
        let saved = core
            .set_model_config(patch)
            .map_err(|err| ApiError::from(&err))?;
        Ok(model_config_dto(&saved))
    })
    .await
}

/// 探测指定模型 id：连通性 + 视觉能力 + 延迟（按该模型已保存的协议）。
#[tauri::command]
#[specta::specta]
pub async fn model_probe(app: tauri::AppHandle, id: String) -> Result<ProbeOutcomeDto, ApiError> {
    run_blocking(app, move |core| {
        core.probe_model(&id)
            .map(ProbeOutcomeDto::from)
            .map_err(|err| ApiError::from(&err))
    })
    .await
}

/// 触发向量蓝绿重建（当前模型 id/维度；重建期间检索零中断）。
#[tauri::command]
#[specta::specta]
pub async fn embedding_rebuild(app: tauri::AppHandle) -> Result<EmbeddingRebuildDto, ApiError> {
    run_blocking(app, move |core| {
        let provider = core.embedding_provider();
        let report = core
            .rebuild_embeddings(provider.id(), provider.dim() as u32)
            .map_err(|err| ApiError::from(&err))?;
        Ok(EmbeddingRebuildDto {
            target_table: report.target_table,
            items: report.items,
            verified: report.verified,
            switched: report.switched,
        })
    })
    .await
}

/// 蓝绿重建结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddingRebuildDto {
    pub target_table: String,
    pub items: u64,
    pub verified: bool,
    pub switched: bool,
}

pub fn specta_builder() -> tauri_specta::Builder<tauri::Wry> {
    tauri_specta::Builder::<tauri::Wry>::new()
        .commands(tauri_specta::collect_commands![
            get_app_status,
            open_library_folder,
            list_items,
            get_item,
            get_settings,
            update_settings,
            get_item_ext,
            note_set,
            tags_list,
            tag_add,
            tag_detach,
            tag_rename,
            tag_delete,
            copy_item_original,
            item_delete,
            item_delete_many,
            item_rename,
            item_reparse,
            list_items_filtered,
            list_item_ids_filtered,
            library_purge_preview,
            library_purge,
            storage_usage_get,
            storage_capacity_set,
            cloud_text_ai_set,
            retention_set,
            retention_preview,
            retention_apply,
            storage_orphans_get,
            storage_orphans_cleanup,
            storage_relocate,
            restart_app,
            pick_directory,
            search_items,
            search_feedback,
            model_probe,
            get_model_config,
            set_model_config,
            embedding_rebuild,
            crate::import_ipc::pick_import_files,
            crate::import_ipc::pick_import_directory,
            crate::import_ipc::discard_import_selection,
            crate::import_ipc::start_file_import,
            crate::import_ipc::create_text_item,
            crate::import_ipc::scan_import_directory,
            crate::import_ipc::start_directory_import,
            crate::import_ipc::get_import_task,
            crate::import_ipc::list_import_tasks,
            crate::import_ipc::list_import_task_entries,
            crate::import_ipc::cancel_import_task,
            crate::import_ipc::list_import_sources,
            crate::import_ipc::refresh_import_source,
            crate::import_ipc::relocate_import_source,
            crate::import_ipc::remove_import_source,
            crate::import_ipc::open_reference_file,
            crate::import_ipc::set_import_drop_target
        ])
        // 前端用 try/catch 处理 `ApiError`，而不是每处解包 `{ status }`。
        .error_handling(tauri_specta::ErrorHandlingMode::Throw)
        // `captured_at` 是 Unix 毫秒，远小于 2^53，用 number 比 BigInt 更适合前端。
        .dangerously_cast_bigints_to_number()
}

/// 绑定文件的受版本控制位置；按 `CARGO_MANIFEST_DIR` 解析，不依赖进程工作目录。
#[cfg(debug_assertions)]
pub fn bindings_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../src/lib/bindings.ts")
}

/// 把 TypeScript 绑定导出到指定路径；测试导到临时文件，避免改写受版本控制的文件。
#[cfg(debug_assertions)]
pub fn export_bindings_to(
    builder: &tauri_specta::Builder<tauri::Wry>,
    path: &std::path::Path,
) -> Result<(), String> {
    builder
        .export(specta_typescript::Typescript::default(), path)
        .map_err(|err| format!("export {}: {err}", path.display()))
}

/// debug 启动时刷新真实绑定文件（`src/lib/bindings.ts`）。
#[cfg(debug_assertions)]
pub fn export_bindings(builder: &tauri_specta::Builder<tauri::Wry>) -> Result<(), String> {
    export_bindings_to(builder, &bindings_path())
}

/// 在阻塞线程池上访问 `AppCore`。
///
/// SQLite 查询与配置落盘（`save_atomic` + `sync_all`）都是同步 IO，既不能跑在
/// Tauri 主线程（架构 §3），也会拖住 async worker，所以统一丢给 blocking 线程池。
pub(crate) async fn run_blocking<T, F>(app: tauri::AppHandle, work: F) -> Result<T, ApiError>
where
    T: Send + 'static,
    F: FnOnce(&AppCore) -> Result<T, ApiError> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(move || {
        let core = app.state::<AppCore>();
        work(core.inner())
    })
    .await
    .map_err(|err| ApiError::new("INTERNAL", format!("blocking task failed: {err}")))?
}

/// 只读内存状态：命令在主线程执行，但不碰磁盘。
#[tauri::command]
#[specta::specta]
pub fn get_app_status(core: tauri::State<'_, AppCore>) -> AppStatus {
    let config = core.config();
    AppStatus {
        version: env!("CARGO_PKG_VERSION").to_string(),
        library_path: config.library_path.display().to_string(),
        library_ok: core.library_ok(),
        restart_required: core.restart_required(),
    }
}

/// 打开库目录；路径始终来自 `AppCore`，前端不能传路径。
#[tauri::command]
#[specta::specta]
pub async fn open_library_folder(app: tauri::AppHandle) -> Result<(), ApiError> {
    // 只读内存里的路径；真正拉起文件管理器放到阻塞线程。
    let path = app.state::<AppCore>().library_root().display().to_string();
    let handle = app.clone();

    tauri::async_runtime::spawn_blocking(move || {
        use tauri_plugin_opener::OpenerExt;

        handle
            .opener()
            .open_path(path.clone(), None::<&str>)
            .map_err(|err| ApiError::new("INTERNAL", format!("open {path}: {err}")))
    })
    .await
    .map_err(|err| ApiError::new("INTERNAL", format!("blocking task failed: {err}")))?
}

#[tauri::command]
#[specta::specta]
pub async fn list_items(
    app: tauri::AppHandle,
    cursor: Option<i64>,
    limit: u32,
) -> Result<Vec<ItemDto>, ApiError> {
    let limit = limit.clamp(1, 100);

    run_blocking(app, move |core| {
        let library = core.library().map_err(ApiError::from)?;
        let items = library
            .list_items(cursor, limit)
            .map_err(|err| ApiError::from(&err))?;
        Ok(items
            .into_iter()
            .map(|item| dto_with_preview(library, item))
            .collect())
    })
    .await
}

/// 带结构化筛选的列表分页；无关键词时直接用它替代检索链路。
#[tauri::command]
#[specta::specta]
pub async fn list_items_filtered(
    app: tauri::AppHandle,
    filters: ItemFiltersDto,
    cursor: Option<i64>,
    limit: u32,
) -> Result<Vec<ItemDto>, ApiError> {
    let limit = limit.clamp(1, 100);
    let filters = filters.to_domain();

    run_blocking(app, move |core| {
        let library = core.library().map_err(ApiError::from)?;
        let items = library
            .list_items_filtered(&filters, cursor, limit)
            .map_err(|err| ApiError::from(&err))?;
        Ok(items
            .into_iter()
            .map(|item| dto_with_preview(library, item))
            .collect())
    })
    .await
}

/// 当前筛选下的全部 id（上限 2000），供「全选当前筛选」。
#[tauri::command]
#[specta::specta]
pub async fn list_item_ids_filtered(
    app: tauri::AppHandle,
    filters: ItemFiltersDto,
    limit: Option<u32>,
) -> Result<Vec<String>, ApiError> {
    let limit = limit.unwrap_or(visrecall_library::items::MAX_FILTERED_IDS);
    let filters = filters.to_domain();

    run_blocking(app, move |core| {
        let library = core.library().map_err(ApiError::from)?;
        library
            .list_item_ids_filtered(&filters, limit)
            .map_err(|err| ApiError::from(&err))
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn get_item(app: tauri::AppHandle, id: String) -> Result<ItemDto, ApiError> {
    run_blocking(app, move |core| {
        let library = core.library().map_err(ApiError::from)?;
        let item = library
            .get_item(&id)
            .map_err(|err| ApiError::from(&err))?
            .ok_or_else(|| ApiError::new("NOT_FOUND", format!("item {id} not found")))?;
        Ok(dto_with_preview(library, item))
    })
    .await
}

/// 读取设置：内存配置 + 库内策略（后者要读 SQLite，因此放到阻塞线程）。
#[tauri::command]
#[specta::specta]
pub async fn get_settings(app: tauri::AppHandle) -> Result<SettingsDto, ApiError> {
    run_blocking(app, move |core| {
        Ok(SettingsDto::from_config(
            &core.config(),
            core.library().ok(),
        ))
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn update_settings(
    app: tauri::AppHandle,
    patch: SettingsPatch,
) -> Result<SettingsDto, ApiError> {
    run_blocking(app, move |core| {
        let config = core
            .update_settings(visrecall_app_core::SettingsPatchInput {
                ball_enabled: patch.ball_enabled,
                ball_size: patch.ball_size,
                capture_hotkey: patch.capture_hotkey,
                ball_edge_hide: patch.ball_edge_hide,
                hide_in_fullscreen: patch.hide_in_fullscreen,
            })
            .map_err(|err| ApiError::from(&err))?;
        Ok(SettingsDto::from_config(&config, core.library().ok()))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_error_maps_stable_codes() {
        let cases = [
            (AppError::ConfigMissing, "CONFIG_MISSING"),
            (AppError::InvalidInput("bad".into()), "INVALID_INPUT"),
            (AppError::NotFound, "NOT_FOUND"),
            (AppError::StorageFull, "STORAGE_FULL"),
            (
                AppError::CaptureFailed("no display".into()),
                "CAPTURE_FAILED",
            ),
            (AppError::CaptureCancelled, "CAPTURE_CANCELLED"),
            (AppError::DrmProtected, "DRM_PROTECTED"),
            (
                AppError::FileMissing("originals/a.png".into()),
                "FILE_MISSING",
            ),
            (AppError::ClipboardBusy, "CLIPBOARD_BUSY"),
            (
                AppError::ClipboardFailed("GlobalAlloc".into()),
                "CLIPBOARD_FAILED",
            ),
            (AppError::Busy("migrating".into()), "BUSY"),
            (AppError::Io("disk".into()), "IO"),
            (AppError::Database("locked".into()), "DATABASE"),
            (AppError::Internal("oops".into()), "INTERNAL"),
            (AppError::ModelNotReady("bge".into()), "MODEL_NOT_READY"),
            (AppError::Provider("timeout".into()), "PROVIDER"),
            (AppError::ProviderConfig("401".into()), "PROVIDER_CONFIG"),
            (
                AppError::SchemaViolation("missing summary".into()),
                "SCHEMA_VIOLATION",
            ),
        ];
        for (err, code) in cases {
            assert_eq!(ApiError::from(&err).code, code);
        }

        let restart = ApiError::from(&AppError::InvalidInput("restart required".into()));
        assert_eq!(restart.code, "INVALID_INPUT");
        assert_eq!(restart.message, "restart required");
    }

    /// 「文本上传云端解析」来自库内策略：默认关闭，写入后随 SettingsDto 一起回前端。
    #[test]
    fn settings_dto_reads_cloud_text_ai_from_library() {
        let dir = tempfile::tempdir().unwrap();
        let library = Library::open(dir.path().to_path_buf()).unwrap();
        let config = visrecall_app_core::AppConfig::default();

        let dto = SettingsDto::from_config(&config, Some(&library));
        assert!(!dto.cloud_text_ai, "默认必须是关闭");
        // 前端契约：JSON 键名是 camelCase。
        let json = serde_json::to_value(&dto).unwrap();
        assert_eq!(json["cloudTextAi"], serde_json::Value::Bool(false));

        library.set_cloud_text_ai(true).unwrap();
        let dto = SettingsDto::from_config(&config, Some(&library));
        assert!(dto.cloud_text_ai);
    }

    /// 库打不开时也应回默认（关闭），而不是 panic 或报错。
    #[test]
    fn settings_dto_cloud_text_ai_defaults_when_library_unavailable() {
        let config = visrecall_app_core::AppConfig::default();
        let dto = SettingsDto::from_config(&config, None);
        assert!(!dto.cloud_text_ai);
    }

    #[test]
    fn capture_failed_tooltip_carries_message_and_code() {
        let tooltip = capture_failed_tooltip("STORAGE_FULL", "storage full");
        assert_eq!(tooltip, "目记 VisRecall — storage full (STORAGE_FULL)");
    }

    #[test]
    fn capture_failed_tooltip_truncates_long_messages() {
        let message = "x".repeat(500);
        let tooltip = capture_failed_tooltip("IO", &message);
        // 截断到上限 + 一个省略号，且仍然是合法 UTF-8（中文前缀未被切坏）。
        assert_eq!(tooltip.chars().count(), TOOLTIP_MAX_CHARS + 1);
        assert!(tooltip.starts_with("目记 VisRecall — xxx"));
        assert!(tooltip.ends_with('…'));
    }

    #[test]
    fn item_dto_builds_thumb_url_only_with_thumb() {
        let base = visrecall_domain::Item {
            id: "id-1".into(),
            kind: visrecall_domain::ItemKind::Image,
            status: visrecall_domain::ItemStatus::Ready,
            captured_at: 42,
            source_app: None,
            source_title: None,
            title: None,
            source_url: None,
            width: Some(10),
            height: Some(20),
            char_count: None,
            original_path: "originals/images/2026/01/a.png".into(),
            thumb_path: None,
            note: String::new(),
            content_hash: "hash".into(),
            revision: 0,
            primary_color: None,
        };

        let dto = ItemDto::from(base.clone());
        assert_eq!(dto.kind, "image");
        assert_eq!(dto.status, "ready");
        assert_eq!(dto.thumb_url, None);

        let with_thumb = visrecall_domain::Item {
            thumb_path: Some("thumbs/id-1.png".into()),
            ..base
        };
        assert_eq!(
            ItemDto::from(with_thumb).thumb_url.as_deref(),
            Some("visrecall://thumb/id-1?rev=0")
        );
    }

    /// 标签 chip 的关联来源必须是 JSON 键 `auto`（不是 `auto_`）。
    #[test]
    fn tag_chip_serializes_auto_key() {
        let chip = TagChipDto::from(ItemTag::new(
            visrecall_domain::Tag {
                id: "t1".into(),
                name: "work".into(),
                source: TagSource::Manual,
                created_at: 0,
            },
            TagSource::Auto,
            2,
        ));

        let json = serde_json::to_value(&chip).unwrap();
        assert_eq!(json["auto"], true);
        assert_eq!(json["name"], "work");
        assert_eq!(json["itemCount"], 2);
        assert!(json.get("auto_").is_none());
        assert!(json["hue"].is_string());
    }

    #[test]
    fn item_detail_dto_nests_tags_under_extras() {
        let detail = visrecall_app_core::ItemDetail {
            item: visrecall_domain::Item {
                id: "id-1".into(),
                kind: visrecall_domain::ItemKind::Text,
                status: visrecall_domain::ItemStatus::Ready,
                captured_at: 1,
                source_app: None,
                source_title: None,
                title: None,
                source_url: None,
                width: None,
                height: None,
                char_count: Some(3),
                original_path: "texts/id-1.txt".into(),
                thumb_path: None,
                note: String::new(),
                content_hash: "hash".into(),
                revision: 0,
                primary_color: None,
            },
            extras: visrecall_app_core::DetailExtras {
                summary: None,
                summary_source: "none".into(),
                ocr_boxes: vec![],
                body_text: Some("abc".into()),
                original_url: None,
                tags: vec![],
            },
        };

        let json = serde_json::to_value(ItemDetailDto::from(detail)).unwrap();
        assert_eq!(json["extras"]["bodyText"], "abc");
        assert_eq!(json["extras"]["summarySource"], "none");
        assert!(json["extras"]["ocrBoxes"].is_array());
        assert!(json["extras"]["originalUrl"].is_null());
        assert!(json["extras"]["tags"].is_array());
    }
}
