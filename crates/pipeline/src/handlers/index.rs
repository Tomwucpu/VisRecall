//! `index` handler（stage 一收口 + 重建/修复共用）：
//! search document → 嵌入 → **同事务**写 insights + FTS + 向量 → `item_indexed`。

use visrecall_domain::vector_math::quantize_i8;
use visrecall_domain::{now_millis, AppError, Insight, Item, ItemKind, ItemStatus};
use visrecall_library::{fts_upsert_tx, insight_upsert_tx, vector_upsert_tx, FtsColumns, Library};
use visrecall_search::document::{build_search_document, DocumentFields};
use visrecall_search::tokenizer::tokenize_for_fts;

use crate::handlers::{
    first_chars, joined_entities, joined_tag_names, placeholder_summary, read_original_text,
    ORIGINAL_TEXT_LIMIT,
};
use crate::worker::{JobCtx, JobHandler};

/// 索引阶段（嵌入 + FTS + 向量，同事务）。
#[derive(Debug, Default, Clone, Copy)]
pub struct IndexHandler;

impl JobHandler for IndexHandler {
    fn stage(&self) -> &'static str {
        crate::STAGE_INDEX
    }

    fn run(&self, ctx: &JobCtx<'_>) -> Result<(), AppError> {
        run_for(ctx)
    }
}

/// 执行一次完整索引（`rebuild` handler 复用同一份逻辑）。
pub(crate) fn run_for(ctx: &JobCtx<'_>) -> Result<(), AppError> {
    let item = ctx
        .library
        .get_item(ctx.item_id())?
        .ok_or(AppError::NotFound)?;
    let rowid = ctx
        .library
        .item_rowid(&item.id)?
        .ok_or(AppError::NotFound)?;
    let now = now_millis();
    let mut insight = ctx
        .library
        .insight_get(&item.id)?
        .unwrap_or_else(|| Insight::empty(&item.id, now));

    let original_text = if item.kind == ItemKind::Image {
        String::new()
    } else {
        read_original_text(ctx.library, &item).unwrap_or_default()
    };

    // 占位摘要（阶段一）：OCR 为空/缺失时不写，summary 保持 NULL、source 保持 none
    if let Some((summary, source)) = placeholder_summary(&item, &insight, &original_text) {
        insight.summary = Some(summary);
        insight.summary_source = source;
    }

    let tags = joined_tag_names(ctx.library, &item.id)?;
    let entities = joined_entities(&insight);
    let document = build_search_document(&DocumentFields {
        title: item.display_title().unwrap_or(""),
        summary: insight.summary.as_deref().unwrap_or(""),
        tags: &tags,
        entities: &entities,
        note: &item.note,
        ocr_or_original: if item.kind == ItemKind::Image {
            insight.ocr_text.as_deref().unwrap_or("")
        } else {
            &original_text
        },
    });

    // 嵌入空间固定于库（首次写入；模型不一致要报错而不是混写）
    let model_id = ctx.deps.embed.id().to_string();
    let dim = u32::try_from(ctx.deps.embed.dim())
        .map_err(|_| AppError::Internal("embedding dim overflow".into()))?;
    ctx.library.init_embedding_space(&model_id, dim)?;

    let mut vectors = ctx
        .deps
        .runtime
        .block_on(ctx.deps.embed.embed(std::slice::from_ref(&document)))?;
    let vector = vectors
        .pop()
        .ok_or_else(|| AppError::Internal("embedder returned no vector".into()))?;
    let vec8 = quantize_i8(&vector);

    let table = ctx.library.active_vector_table();
    let cols = FtsColumns {
        title: &tokenize_for_fts(item.display_title().unwrap_or("")),
        summary: &tokenize_for_fts(insight.summary.as_deref().unwrap_or("")),
        ocr_text: &tokenize_for_fts(insight.ocr_text.as_deref().unwrap_or("")),
        note: &tokenize_for_fts(&item.note),
        tags: &tokenize_for_fts(&tags),
        entities: &tokenize_for_fts(&entities),
        original_text: &tokenize_for_fts(&first_chars(&original_text, ORIGINAL_TEXT_LIMIT)),
    };
    insight.updated_at = now;

    // 三写同事务：任一步失败整体回滚，任务重试（不会出现「FTS 搜得到但向量搜不到」）
    ctx.library.write_index_tx(|tx| {
        insight_upsert_tx(tx, &insight)?;
        fts_upsert_tx(tx, rowid, &cols)?;
        vector_upsert_tx(tx, &table, rowid, &vec8, dim, &model_id)?;
        Ok(())
    })?;

    // 此刻起三路可检索；`status` 仍是 parsing（阶段二才置 ready）
    ctx.events.item_indexed(&item.id);
    // 未授权云端 AI 的目录引用在本地索引完成后即可使用，不能一直停留在 parsing。
    if !ctx.library.cloud_ai_allowed(&item.id)? {
        ctx.library
            .update_status(&item.id, ItemStatus::Ready, None)?;
    }
    Ok(())
}

/// 供重建/修复路径复用的「构造 document」入口（不写库）。
pub(crate) fn document_for(library: &Library, item: &Item) -> Result<String, AppError> {
    let insight = library
        .insight_get(&item.id)?
        .unwrap_or_else(|| Insight::empty(&item.id, now_millis()));
    let original_text = if item.kind == ItemKind::Image {
        String::new()
    } else {
        read_original_text(library, item).unwrap_or_default()
    };
    let tags = joined_tag_names(library, &item.id)?;
    let entities = joined_entities(&insight);
    Ok(build_search_document(&DocumentFields {
        title: item.display_title().unwrap_or(""),
        summary: insight.summary.as_deref().unwrap_or(""),
        tags: &tags,
        entities: &entities,
        note: &item.note,
        ocr_or_original: if item.kind == ItemKind::Image {
            insight.ocr_text.as_deref().unwrap_or("")
        } else {
            &original_text
        },
    }))
}

impl IndexHandler {
    /// 便捷入口：给测试与 `app-core` 手动触发一次索引（同步，不经过队列）。
    pub fn run_now(ctx: &JobCtx<'_>) -> Result<(), AppError> {
        run_for(ctx)
    }
}
