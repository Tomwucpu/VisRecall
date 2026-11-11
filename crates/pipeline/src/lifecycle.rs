//! 索引生命周期（design §6.4）：启动自检修复、向量蓝绿重建。

use std::sync::Arc;

use visrecall_domain::insight::{Insight, SummarySource};
use visrecall_domain::providers::EmbeddingProvider;
use visrecall_domain::vector_math::quantize_i8;
use visrecall_domain::{now_millis, AppError};
use visrecall_library::{vector_upsert_tx, FtsColumns, IndexCounts, Library, DEFAULT_VECTOR_TABLE};
use visrecall_search::tokenizer::tokenize_for_fts;

use crate::handlers::index::document_for;
use crate::handlers::{joined_entities, joined_tag_names, read_original_text};
use crate::worker::{PRIORITY_NEW, PRIORITY_REBUILD};
use crate::STAGE_INDEX;

/// 启动自检结果（供日志与 `AppStatus` 使用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartupReport {
    pub counts: IndexCounts,
    /// 因缺失 FTS/向量行而重新入队的素材数。
    pub repaired: usize,
}

/// 启动自检：行数比对 → 缺失派生数据入队 `index`（`PRIORITY_REBUILD`）。
///
/// 只修复**派生数据缺失**（FTS/向量行被外部删掉）；不重跑 `pending` 素材的 `vlm`。
/// 解析失败的素材保持原状，等用户手动在详情里重新解析（`item_reparse`）。
pub fn run_startup_checks(library: &Library) -> Result<StartupReport, AppError> {
    let counts = library.index_counts()?;
    let mut repaired = 0usize;

    if !counts.is_consistent() {
        tracing::warn!(
            items = counts.items,
            fts = counts.fts,
            vectors = counts.vectors,
            "derived index rows mismatch; enqueueing repairs"
        );
        for (_rowid, item_id) in library.missing_index_rowids(200)? {
            match library.enqueue_stage(&item_id, STAGE_INDEX, PRIORITY_REBUILD) {
                Ok(_) => repaired += 1,
                Err(err) => tracing::warn!(item = %item_id, %err, "enqueue repair failed"),
            }
        }
    }

    Ok(StartupReport { counts, repaired })
}

/// 蓝绿重建的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebuildReport {
    /// 新写的目标表（成功后成为活跃表）。
    pub target_table: String,
    /// 重嵌入的素材数。
    pub items: u64,
    /// 行数校验是否通过（不通过则不切指针）。
    pub verified: bool,
    /// 是否已切换活跃表。
    pub switched: bool,
}

/// 选下一个蓝绿表名：活跃表是 `_v2` 就用 `_v3`，否则用 `_v2`。
pub fn next_rebuild_table(active: &str) -> String {
    if active == DEFAULT_VECTOR_TABLE {
        return format!("{DEFAULT_VECTOR_TABLE}_v2");
    }
    let suffix = active
        .strip_prefix(&format!("{DEFAULT_VECTOR_TABLE}_v"))
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(0);
    format!("{DEFAULT_VECTOR_TABLE}_v{}", suffix + 1)
}

/// 蓝绿重建：新建向量表 → 分批全量重嵌入 → 行数校验 → 原子切指针（旧表保留回滚）。
///
/// 检索全程读 `vector.active_table` 指针，切换是单事务，因此零中断。
/// `new_model_id` / `new_dim` 来自新模型；成功后 `embedding.model_id/dim` 一并更新。
///
/// 实现说明：本函数是**同步**的（20 条规模、每批 100 条一次跑完），由 `embedding_rebuild`
/// IPC 在 blocking 池上调用；`rebuild` **任务**只用于单条修复，见 `handlers::rebuild`。
pub fn rebuild_embeddings(
    library: &Arc<Library>,
    embed: &dyn EmbeddingProvider,
    runtime: &tokio::runtime::Handle,
    new_model_id: &str,
    new_dim: u32,
) -> Result<RebuildReport, AppError> {
    let active = library.active_vector_table();
    let target = next_rebuild_table(&active);
    library.create_vector_table(&target)?;
    tracing::info!(from = %active, to = %target, model = %new_model_id, "starting blue-green rebuild");

    let mut processed = 0u64;
    let mut cursor: Option<i64> = None;
    loop {
        let batch = library.list_items(cursor, 100)?;
        if batch.is_empty() {
            break;
        }
        for item in &batch {
            let rowid = library.item_rowid(&item.id)?.unwrap_or_default();
            let document = document_for(library, item)?;
            let mut vectors = runtime.block_on(embed.embed(std::slice::from_ref(&document)))?;
            let vector = vectors
                .pop()
                .ok_or_else(|| AppError::Internal("embedder returned no vector".into()))?;
            if vector.len() != new_dim as usize {
                return Err(AppError::InvalidInput(format!(
                    "rebuild dim mismatch: model {new_model_id} returns {}, expected {new_dim}",
                    vector.len()
                )));
            }
            let vec8 = quantize_i8(&vector);
            library.write_index_tx(|tx| {
                vector_upsert_tx(tx, &target, rowid, &vec8, new_dim, new_model_id)
            })?;
            processed += 1;
        }
        cursor = batch.last().map(|item| item.captured_at);
        if batch.len() < 100 {
            break;
        }
    }

    let items = library.count_items()?;
    let written = library.vector_count_for(&target)?;
    if items != written {
        tracing::error!(
            items,
            written,
            "rebuild row count mismatch; keeping old table"
        );
        return Ok(RebuildReport {
            target_table: target,
            items: processed,
            verified: false,
            switched: false,
        });
    }

    // 原子切换：指针 + 嵌入空间同一事务
    library.switch_vector_and_embedding_space(&target, new_model_id, new_dim)?;
    tracing::info!(table = %target, items = written, "blue-green rebuild switched active table");
    Ok(RebuildReport {
        target_table: target,
        items: processed,
        verified: true,
        switched: true,
    })
}

/// 用「当前库内容」给一条素材重建 FTS 行（排障/评测用的单条修复入口）。
///
/// V0.1 不做 `fts.tokenizer_version` 变更后的自动重建（design §1.2）。
pub fn rebuild_fts_row(library: &Library, item_id: &str) -> Result<(), AppError> {
    let item = library.get_item(item_id)?.ok_or(AppError::NotFound)?;
    let rowid = library.item_rowid(item_id)?.ok_or(AppError::NotFound)?;
    let insight = library
        .insight_get(item_id)?
        .unwrap_or_else(|| Insight::empty(item_id, now_millis()));
    let original_text = if item.kind == visrecall_domain::ItemKind::Image {
        String::new()
    } else {
        read_original_text(library, &item).unwrap_or_default()
    };
    let tags = joined_tag_names(library, item_id)?;
    let entities = joined_entities(&insight);
    let cols = FtsColumns {
        title: &tokenize_for_fts(item.display_title().unwrap_or("")),
        summary: &tokenize_for_fts(insight.summary.as_deref().unwrap_or("")),
        ocr_text: &tokenize_for_fts(insight.ocr_text.as_deref().unwrap_or("")),
        note: &tokenize_for_fts(&item.note),
        tags: &tokenize_for_fts(&tags),
        entities: &tokenize_for_fts(&entities),
        original_text: &tokenize_for_fts(&original_text),
    };
    library.fts_upsert(rowid, &cols)
}

/// 素材是否已有 VLM 级别的摘要。
pub fn is_enriched(library: &Library, item_id: &str) -> Result<bool, AppError> {
    Ok(library
        .insight_get(item_id)?
        .map(|insight| insight.summary_source == SummarySource::Vlm)
        .unwrap_or(false))
}

/// 新捕获链路的入队优先级（`app-core` 用）。
pub const ENQUEUE_PRIORITY_NEW: i64 = PRIORITY_NEW;
