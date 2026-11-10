//! `ocr` handler（stage 一前半，仅图片）：PP-OCR → Windows 兜底 → 写 `insights.ocr_*`。
//!
//! 三条分支（design §6.1）：
//! 1. 有文字 → 写 `ocr_text`/`ocr_boxes`/`ocr_engine`/`ocr_confidence`；
//! 2. **无文字（正常结果）** → `ocr_text`/`boxes`/`ocr_confidence` 落 NULL/空，引擎记实际跑的那个；
//! 3. 两级引擎都失败 → 只建 insights 行（无 ocr 字段），索引照常（原文为空）。
//!
//! 三种分支都继续入队 `index`：无字图片依然要靠 VLM 摘要与结构化属性参与检索。

use visrecall_domain::{now_millis, AppError, Insight, ItemKind};

use crate::handlers::read_original_bytes;
use crate::worker::{JobCtx, JobHandler};
use crate::PRIORITY_NEW;

/// 图片 OCR 阶段。
#[derive(Debug, Default, Clone, Copy)]
pub struct OcrHandler;

impl JobHandler for OcrHandler {
    fn stage(&self) -> &'static str {
        crate::STAGE_OCR
    }

    fn run(&self, ctx: &JobCtx<'_>) -> Result<(), AppError> {
        let item = ctx
            .library
            .get_item(ctx.item_id())?
            .ok_or(AppError::NotFound)?;

        // 文本/链接不走 OCR（requirements P3-R01）：直接接力到阶段一收口 + 阶段二
        if item.kind != ItemKind::Image {
            ctx.library.enqueue_stage_at(
                &item.id,
                crate::STAGE_INDEX,
                ctx.revision(),
                PRIORITY_NEW,
            )?;
            if ctx.library.cloud_ai_allowed(&item.id)? {
                ctx.library.enqueue_stage_at(
                    &item.id,
                    crate::STAGE_VLM,
                    ctx.revision(),
                    PRIORITY_NEW,
                )?;
            }
            return Ok(());
        }

        let now = now_millis();
        let mut insight = ctx
            .library
            .insight_get(&item.id)?
            .unwrap_or_else(|| Insight::empty(&item.id, now));

        let bytes = read_original_bytes(ctx.library, &item)?;
        match ctx.deps.ocr.recognize_with_engine(&bytes) {
            Ok((output, engine)) => {
                // 无文字时全部落 None/空：**不是 0**，否则会误触「OCR 置信度低」（0.6 阈值）
                insight.ocr_text = output.text.filter(|text| !text.trim().is_empty());
                insight.ocr_boxes = if insight.ocr_text.is_some() {
                    output.boxes
                } else {
                    Vec::new()
                };
                insight.ocr_confidence = if insight.ocr_text.is_some() {
                    output.confidence
                } else {
                    None
                };
                insight.ocr_engine = Some(engine.to_string());
            }
            Err(err) => {
                // 两级引擎都失败：该素材无 OCR 文本，仅元数据/备注可搜（不阻塞索引）
                tracing::warn!(
                    item = %item.id,
                    %err,
                    "ocr engines failed; indexing without ocr text"
                );
                insight.ocr_text = None;
                insight.ocr_boxes = Vec::new();
                insight.ocr_confidence = None;
                insight.ocr_engine = None;
            }
        }

        insight.updated_at = now;
        ctx.library.insight_upsert(&insight)?;

        // 接力：先入队阶段一收口（`index`），再入队阶段二（`vlm`）。
        // 同一优先级下按 rowid 先进先出，因此 `index` 必然先被 claim——这正是「阶段一先完成、
        // 素材立即可检索，VLM 再异步增强」的顺序保证（P3-R04）。capture 路径只入队 `ocr`。
        ctx.library
            .enqueue_stage_at(&item.id, crate::STAGE_INDEX, ctx.revision(), PRIORITY_NEW)?;
        if ctx.library.cloud_ai_allowed(&item.id)? {
            ctx.library.enqueue_stage_at(
                &item.id,
                crate::STAGE_VLM,
                ctx.revision(),
                PRIORITY_NEW,
            )?;
        }
        Ok(())
    }
}
