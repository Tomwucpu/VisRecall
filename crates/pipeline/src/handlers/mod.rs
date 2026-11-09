//! 四个阶段的 handler：`ocr` → `index` → `vlm`，另有 `rebuild`（修复/重建）。
//!
//! 共同的纪律：
//! - **推理不持库锁**：读文件、跑 OCR、跑嵌入、跑 HTTP 都在锁外；写结果才开短事务。
//! - **三写同事务**：`index` handler 的 insights + FTS + 向量必须在 `write_index_tx` 里。
//! - **事件小载荷**：只传 id，不传 OCR 全文/向量。

pub mod index;
pub mod ocr;
pub mod rebuild;
pub mod vlm;

pub use index::IndexHandler;
pub use ocr::OcrHandler;
pub use rebuild::RebuildHandler;
pub use vlm::VlmHandler;

use visrecall_domain::insight::{Insight, SummarySource};
use visrecall_domain::{AppError, Item, ItemKind};
use visrecall_library::Library;

/// 占位摘要长度（OCR 摘录 / 原文摘录）。
pub const PLACEHOLDER_CHARS: usize = 280;
/// 文本素材进 FTS 的字符上限（requirements P3-R05）。
pub const ORIGINAL_TEXT_LIMIT: usize = 10_000;

/// 读原图/原文文件的字节。
pub(crate) fn read_original_bytes(library: &Library, item: &Item) -> Result<Vec<u8>, AppError> {
    let path = library.resolve_existing_or_join(&item.original_path)?;
    std::fs::read(&path).map_err(|_| AppError::FileMissing(item.original_path.clone()))
}

/// 读文本素材原文（截到 [`ORIGINAL_TEXT_LIMIT`] 字符）。
pub(crate) fn read_original_text(library: &Library, item: &Item) -> Result<String, AppError> {
    let bytes = read_original_bytes(library, item)?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    Ok(first_chars(&text, ORIGINAL_TEXT_LIMIT))
}

/// 取前 `limit` 个字符（按字符而不是字节，避免切坏 UTF-8）。
pub(crate) fn first_chars(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    text.chars().take(limit).collect()
}

/// 素材标签名（空格连接），供 FTS 与 search document 使用。
pub(crate) fn joined_tag_names(library: &Library, item_id: &str) -> Result<String, AppError> {
    let tags = library.tags_for_item(item_id)?;
    Ok(tags
        .into_iter()
        .map(|tag| tag.tag.name)
        .collect::<Vec<_>>()
        .join(" "))
}

/// 素材实体名（空格连接）。
pub(crate) fn joined_entities(insight: &Insight) -> String {
    insight.entities.join(" ")
}

/// 阶段一的占位摘要规则（design §6.1）。
///
/// - 只在「还没有摘要」（`summary_source == none` 且未被用户编辑）时写；
/// - 图片取 OCR 文本前 280 字符（`source='ocr'`），文本/链接取原文前 280 字符
///   （`source='extractive'`）；
/// - **OCR 文本为空/缺失时不写**：空串占位会让「无摘要」与「OCR 摘录为空」不可区分。
pub(crate) fn placeholder_summary(
    item: &Item,
    insight: &Insight,
    original_text: &str,
) -> Option<(String, SummarySource)> {
    if insight.summary_user_edited || insight.summary_source != SummarySource::None {
        return None;
    }
    match item.kind {
        ItemKind::Image => insight
            .ocr_text
            .as_deref()
            .filter(|text| !text.trim().is_empty())
            .map(|text| (first_chars(text, PLACEHOLDER_CHARS), SummarySource::Ocr)),
        ItemKind::Text | ItemKind::Link => (!original_text.trim().is_empty()).then(|| {
            (
                first_chars(original_text, PLACEHOLDER_CHARS),
                SummarySource::Extractive,
            )
        }),
    }
}
