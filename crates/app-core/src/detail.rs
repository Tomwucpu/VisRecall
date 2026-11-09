//! 详情聚合：一条命令返回素材本体 + 面板需要的额外内容（design §6.1）。
//!
//! 聚合放在编排层而不是库层：它要读文件（原文）并拼协议 URL，属于「跨存储与壳层的
//! 组合」，库层不该知道 `visrecall://` 协议。
//!
//! 两条降级约定：
//! - 原文文件读不到（缺失/非 UTF-8/无权限）→ `body_text = None`，其余字段照常返回，
//!   详情面板显示「原文不可读」而不是整条命令失败；
//! - 原图文件缺失 → `original_url` 仍然返回，由协议层 404，前端显示占位。

use std::fs;

use visrecall_domain::tag::ItemTag;
use visrecall_domain::{AppError, Item, ItemKind};
use visrecall_library::Library;

/// 原文返回的字符上限；超出部分截断并追加标记。
pub const BODY_TEXT_LIMIT: usize = 20_000;

/// 原图协议前缀。
pub const ORIGINAL_URL_PREFIX: &str = "visrecall://original/";

/// 截断标记。
pub const TRUNCATED_MARKER: &str = "…（已截断）";

/// 详情面板需要的额外内容。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DetailExtras {
    /// AI 摘要；未解析时为 `None`（面板显示占位）。
    pub summary: Option<String>,
    /// 摘要来源：`vlm` / `ocr` / `extractive` / `user` / `none`（面板据此显示来源徽标）。
    pub summary_source: String,
    /// OCR 归一化包围盒（图片素材；无文字为空）——详情面板据此画高亮框。
    pub ocr_boxes: Vec<visrecall_domain::OcrBox>,
    /// 文本素材的原文（前 [`BODY_TEXT_LIMIT`] 字符）。
    pub body_text: Option<String>,
    /// 图片素材的原图协议 URL；文本/链接为 `None`。
    pub original_url: Option<String>,
    /// 该素材的标签（含关联来源）。
    pub tags: Vec<ItemTag>,
}

/// 素材本体 + 额外内容。
#[derive(Clone, Debug, PartialEq)]
pub struct ItemDetail {
    pub item: Item,
    pub extras: DetailExtras,
}

/// 读取一条素材的完整详情。
pub fn load_detail(library: &Library, id: &str) -> Result<ItemDetail, AppError> {
    let item = library.get_item(id)?.ok_or(AppError::NotFound)?;
    let tags = library.tags_for_item(&item.id)?;
    // 阶段 03：AI 解析结果（可能还没有行 → 全部按未解析展示）
    let insight = library.insight_get(&item.id)?;

    let body_text = read_body_text(library, &item);
    let original_url = match item.kind {
        ItemKind::Image => Some(format!("{ORIGINAL_URL_PREFIX}{}", item.id)),
        ItemKind::Text | ItemKind::Link => None,
    };

    Ok(ItemDetail {
        item,
        extras: DetailExtras {
            summary: insight.as_ref().and_then(|value| value.summary.clone()),
            summary_source: insight
                .as_ref()
                .map(|value| value.summary_source.as_str().to_string())
                .unwrap_or_else(|| "none".to_string()),
            ocr_boxes: insight
                .as_ref()
                .map(|value| value.ocr_boxes.clone())
                .unwrap_or_default(),
            body_text,
            original_url,
            tags,
        },
    })
}

/// 读文本素材的原文，按**字符**截断到 [`BODY_TEXT_LIMIT`]。
///
/// 非文本素材、文件不存在、内容不是合法 UTF-8 都返回 `None`。
pub fn read_body_text(library: &Library, item: &Item) -> Option<String> {
    if item.kind != ItemKind::Text {
        return None;
    }
    let path = library.root().join(&item.original_path);
    let text = fs::read_to_string(path).ok()?;
    Some(truncate_body(&text))
}

/// 按字符边界截断，超出时追加 [`TRUNCATED_MARKER`]。
///
/// 用 `char_indices().nth(limit)` 找第 `limit + 1` 个字符的字节偏移，
/// 禁止 `&text[..limit]`——那会切断多字节序列（UTF-8 下直接 panic 或产生乱码）。
pub fn truncate_body(text: &str) -> String {
    match text.char_indices().nth(BODY_TEXT_LIMIT) {
        Some((byte_idx, _)) => format!("{}{TRUNCATED_MARKER}", &text[..byte_idx]),
        None => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_keeps_short_text_untouched() {
        assert_eq!(truncate_body("abc"), "abc");
        let exact = "字".repeat(BODY_TEXT_LIMIT);
        assert_eq!(truncate_body(&exact), exact);
    }

    #[test]
    fn truncate_adds_marker_and_keeps_exact_char_count() {
        let long = "字".repeat(BODY_TEXT_LIMIT + 1);
        let out = truncate_body(&long);
        assert!(out.ends_with(TRUNCATED_MARKER));
        assert_eq!(
            out.trim_end_matches(TRUNCATED_MARKER).chars().count(),
            BODY_TEXT_LIMIT
        );
    }

    #[test]
    fn truncate_never_splits_multibyte_sequences() {
        // 第 20000 个字符是 emoji（4 字节），后面还有内容。
        let mut text = "字".repeat(BODY_TEXT_LIMIT - 1);
        text.push('😀');
        text.push_str("尾巴");
        let out = truncate_body(&text);
        assert!(!out.contains('\u{fffd}'), "no replacement chars");
        assert_eq!(
            out.trim_end_matches(TRUNCATED_MARKER).chars().count(),
            BODY_TEXT_LIMIT
        );
        assert!(
            out.trim_end_matches(TRUNCATED_MARKER).ends_with('😀'),
            "emoji must stay intact"
        );
    }
}
