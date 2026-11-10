//! `vlm` handler（stage 二）：VLM/文本 Provider 增强 → 写 insights → 自动标签 → 重建索引。
//!
//! 关键语义：
//! - **同一 content_hash 复用**：同画面二次捕获不再调模型（省调用，不改变「不去重」）；
//! - **自动标签过滤 `tag_suppressions`**：用户删过的自动标签不得回贴；
//! - **失败降级**：可重试错误交给 worker 退避；配置错误（401/403）或最后一次尝试失败时把
//!   素材置 `pending`（OCR 摘录已占位，UI 显示「待重试」）；
//! - **revision**：`bump_revision` 与入队 `index` 同事务（design §7.6）。

use visrecall_domain::insight::{Insight, OcrBox, SummarySource};
use visrecall_domain::providers::VisionInsight;
use visrecall_domain::{now_millis, AppError, ItemKind, ItemStatus};

use crate::handlers::{read_original_bytes, read_original_text};
use crate::worker::{JobCtx, JobHandler};
use crate::PRIORITY_NEW;

/// stage 二：云端增强。
#[derive(Debug, Default, Clone, Copy)]
pub struct VlmHandler;

impl JobHandler for VlmHandler {
    fn stage(&self) -> &'static str {
        crate::STAGE_VLM
    }

    fn run(&self, ctx: &JobCtx<'_>) -> Result<(), AppError> {
        let item = ctx
            .library
            .get_item(ctx.item_id())?
            .ok_or(AppError::NotFound)?;
        // 防御性检查：旧队列或手动重新解析也不能绕过来源级云端授权。
        if !ctx.library.cloud_ai_allowed(&item.id)? {
            ctx.library
                .update_status(&item.id, visrecall_domain::ItemStatus::Ready, None)?;
            return Ok(());
        }
        let now = now_millis();
        let mut insight = ctx
            .library
            .insight_get(&item.id)?
            .unwrap_or_else(|| Insight::empty(&item.id, now));

        // 1) content_hash 复用：同画面的另一条素材已有完整解析 → 拷贝字段、跳过模型调用
        let mut tags: Vec<String> = Vec::new();
        let mut reused = false;
        if let Some(other) = ctx
            .library
            .insight_by_content_hash(&item.content_hash, &item.id)?
        {
            if other.summary_source == SummarySource::Vlm {
                copy_model_fields(&mut insight, &other);
                tags = ctx
                    .library
                    .tags_for_item(&other.item_id)?
                    .into_iter()
                    .map(|tag| tag.tag.name)
                    .collect();
                reused = true;
                tracing::info!(
                    item = %item.id,
                    reused_from = %other.item_id,
                    "reusing insight from identical content hash"
                );
            }
        }

        if !reused {
            let analyzed = match item.kind {
                ItemKind::Image => {
                    let png = read_original_bytes(ctx.library, &item)?;
                    ctx.deps
                        .runtime
                        .block_on(ctx.deps.vision.analyze(&png))
                        .map(|result| (result, ctx.deps.vision.id().to_string()))
                }
                ItemKind::Text | ItemKind::Link => {
                    let text = read_original_text(ctx.library, &item)?;
                    ctx.deps
                        .runtime
                        .block_on(ctx.deps.text.summarize(&text))
                        .map(|result| {
                            (
                                VisionInsight {
                                    summary: result.summary,
                                    tags: result.tags,
                                    confidence: result.confidence,
                                    ..VisionInsight::default()
                                },
                                ctx.deps.text.id().to_string(),
                            )
                        })
                }
            };

            match analyzed {
                Ok((result, provider)) => {
                    apply_model_result(&mut insight, &result, &provider, now);
                    tags = result.tags;
                }
                Err(err) => {
                    // 配置错误或最后一次尝试：素材进 pending（OCR 摘录已占位，可重试）
                    let terminal = err.is_config_error() || ctx.is_final_attempt();
                    if terminal {
                        if let Err(update_err) =
                            ctx.library
                                .update_status(&item.id, ItemStatus::Pending, None)
                        {
                            tracing::warn!(%update_err, "mark item pending failed");
                        }
                        ctx.events.item_pending(&item.id);
                        tracing::warn!(
                            item = %item.id,
                            attempts = ctx.attempts(),
                            %err,
                            "vlm failed; item marked pending"
                        );
                    }
                    return Err(err);
                }
            }
        }

        insight.updated_at = now;
        ctx.library.insight_upsert(&insight)?;

        // 自动标签（origin='auto'，过滤 suppressions，同名复用）
        for tag in &tags {
            match ctx.library.tag_add_auto(&item.id, tag) {
                Ok(Some(name)) => tracing::debug!(item = %item.id, tag = %name, "auto tag added"),
                Ok(None) => tracing::debug!(item = %item.id, tag = %tag, "auto tag suppressed"),
                Err(err) => tracing::warn!(item = %item.id, tag = %tag, %err, "auto tag failed"),
            }
        }

        // revision + 入队 index 必须同事务（否则崩溃会留下「revision 变了但没任务」）
        ctx.library
            .bump_revision_and_enqueue(&item.id, crate::STAGE_INDEX, PRIORITY_NEW)?;
        ctx.library
            .update_status(&item.id, ItemStatus::Ready, None)?;
        ctx.events.item_enriched(&item.id);
        Ok(())
    }
}

/// 把模型结果写进 insight（保留已有 OCR 字段）。
fn apply_model_result(insight: &mut Insight, result: &VisionInsight, provider: &str, now: i64) {
    insight.summary = Some(result.summary.trim().to_string());
    insight.summary_source = SummarySource::Vlm;
    insight.content_type = result.content_type.clone();
    insight.primary_color = result.primary_color.clone();
    insight.chart_type = result.chart_type.clone();
    insight.ui_type = result.ui_type.clone();
    insight.entities = result.entities.clone();
    insight.latex = result.latex.clone();
    insight.has_formula = result.has_formula;
    insight.model_provider = Some(provider.to_string());
    insight.confidence = result.confidence;
    insight.updated_at = now;
}

/// content_hash 复用时拷贝模型字段（不动本素材自己的 OCR 文本）。
fn copy_model_fields(insight: &mut Insight, other: &Insight) {
    insight.summary = other.summary.clone();
    insight.summary_source = other.summary_source;
    insight.content_type = other.content_type.clone();
    insight.primary_color = other.primary_color.clone();
    insight.chart_type = other.chart_type.clone();
    insight.ui_type = other.ui_type.clone();
    insight.entities = other.entities.clone();
    insight.latex = other.latex.clone();
    insight.has_formula = other.has_formula;
    insight.model_provider = other.model_provider.clone();
    insight.model_name = other.model_name.clone();
    insight.confidence = other.confidence;
}

/// 预览用：把 OCR 框按命中词过滤（供 `app-core` 的解释装配复用）。
pub fn boxes_matching(insight: &Insight, terms: &[String]) -> Vec<OcrBox> {
    insight
        .ocr_boxes
        .iter()
        .filter(|obox| terms.iter().any(|term| obox.contains(term)))
        .cloned()
        .collect()
}
