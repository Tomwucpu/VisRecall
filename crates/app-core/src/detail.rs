//! 详情聚合：一条命令返回素材本体 + 面板需要的额外内容（design §6.1）。
//!
//! 聚合放在编排层而不是库层：它要读文件（原文）并拼协议 URL，属于「跨存储与壳层的
//! 组合」，库层不该知道 `visrecall://` 协议。
//!
//! 两条降级约定：
//! - 原文文件读不到（缺失/非 UTF-8/无权限）→ `body_text = None`，其余字段照常返回，
//!   详情面板显示「原文不可读」而不是整条命令失败；
//! - 原图文件缺失 → `original_url` 仍然返回，由协议层 404，前端显示占位。

use std::borrow::Cow;
use std::fs;
use std::io::Read;

use visrecall_domain::tag::ItemTag;
use visrecall_domain::{AppError, Item, ItemKind};
use visrecall_library::Library;

/// 原文返回的字符上限；超出部分截断并追加标记。
pub const BODY_TEXT_LIMIT: usize = 20_000;

/// 卡片预览的字符上限（列表用，只够铺满缩略区，不代表全文）。
pub const PREVIEW_CHARS: usize = 160;

/// 生成预览时最多读取的字节数：160 个汉字约 480 字节，4KB 足够覆盖多字节字符。
pub const PREVIEW_MAX_BYTES: usize = 4 * 1024;

/// 标题去重后至少还要剩这么多字符，否则视为「标题就是全文」而不是重复前缀。
const PREVIEW_MIN_REST_CHARS: usize = 40;

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

/// 列表卡片的缩略区预览。
///
/// - 文本素材：只读原文**头部**（[`PREVIEW_MAX_BYTES`]）再截到 [`PREVIEW_CHARS`]，
///   不做整文件读取——列表里可能有几十条素材，一页全量读大文件不划算；
/// - 链接素材：没有本地文件，用 `source_url` 去掉协议与 `www.` 后展示；
/// - 图片素材：有真正的缩略图，返回 `None`。
///
/// 读不到（文件缺失 / 非 UTF-8）返回 `None`，卡片回退到类型占位。
pub fn read_preview(library: &Library, item: &Item) -> Option<String> {
    match item.kind {
        ItemKind::Image => None,
        ItemKind::Text => {
            let head = read_head_text(library, item)?;
            let body = strip_title_prefix(&head, item.source_title.as_deref());
            let preview = normalize_preview(&body);
            (!preview.is_empty()).then_some(preview)
        }
        ItemKind::Link => link_preview(item.source_url.as_deref()),
    }
}

/// 读文本文件头部；非 UTF-8 按「不可预览」处理，与详情 `body_text` 的口径一致
/// （详情也不会猜 GBK 编码，避免卡片上出现一段乱码）。
fn read_head_text(library: &Library, item: &Item) -> Option<String> {
    let path = library.root().join(&item.original_path);
    let mut file = fs::File::open(path).ok()?;
    let mut buf = Vec::with_capacity(PREVIEW_MAX_BYTES);
    let mut chunk = [0u8; 1024];
    let mut remaining = PREVIEW_MAX_BYTES;
    while remaining > 0 {
        let want = remaining.min(chunk.len());
        let read = file.read(&mut chunk[..want]).ok()?;
        if read == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..read]);
        remaining -= read;
    }

    match std::str::from_utf8(&buf) {
        Ok(text) => Some(text.to_string()),
        // 只有「读满了头部上限」才可能是尾部截断：取合法前缀；
        // 文件本身不是 UTF-8 时 `valid_up_to` 之前也可能有内容，但这里保守返回 `None`。
        Err(err) if remaining == 0 && err.valid_up_to() > 0 => {
            Some(String::from_utf8_lossy(&buf[..err.valid_up_to()]).into_owned())
        }
        Err(_) => None,
    }
}

/// 标题常是正文第一行（剪贴板片段尤其如此）。若预览以标题开头且剩余内容够长，
/// 就切掉这段，避免卡片标题与预览上下重复。
fn strip_title_prefix<'a>(text: &'a str, title: Option<&str>) -> Cow<'a, str> {
    let trimmed = text.trim_start();
    let Some(title) = title.map(str::trim).filter(|value| !value.is_empty()) else {
        return Cow::Borrowed(trimmed);
    };
    let Some(rest) = trimmed.strip_prefix(title) else {
        return Cow::Borrowed(trimmed);
    };
    let rest = rest.trim_start();
    if rest.chars().count() >= PREVIEW_MIN_REST_CHARS {
        Cow::Owned(rest.to_string())
    } else {
        Cow::Borrowed(trimmed)
    }
}

/// 压平换行噪声：统一行尾、去掉每行尾部空白、丢弃开头空行并把连续空行折叠成一行。
fn normalize_preview(text: &str) -> String {
    let unified = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut lines: Vec<&str> = Vec::new();
    for line in unified.lines() {
        let line = line.trim_end();
        if line.trim().is_empty() {
            // 开头空行没有信息量；连续空行只保留一个换行。
            if lines.is_empty() || lines.last().is_some_and(|prev| prev.is_empty()) {
                continue;
            }
            lines.push("");
        } else {
            lines.push(line);
        }
    }
    first_chars(lines.join("\n").trim(), PREVIEW_CHARS)
}

/// 链接预览：去掉 `https://` / `www.` 与尾部斜杠，保留路径便于区分同站页面。
fn link_preview(url: Option<&str>) -> Option<String> {
    let url = url.map(str::trim).filter(|value| !value.is_empty())?;
    let display = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let display = display.strip_prefix("www.").unwrap_or(display);
    let display = display.trim_end_matches('/');
    (!display.is_empty()).then(|| first_chars(display, PREVIEW_CHARS))
}

/// 按字符截断（不切断多字节序列），不加省略号——卡片用渐隐遮罩暗示「还有更多」。
fn first_chars(text: &str, limit: usize) -> String {
    match text.char_indices().nth(limit) {
        Some((byte_idx, _)) => text[..byte_idx].to_string(),
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

    #[test]
    fn preview_truncates_on_char_boundary_without_marker() {
        let long = "字".repeat(PREVIEW_CHARS + 50);
        let preview = normalize_preview(&long);
        assert_eq!(preview.chars().count(), PREVIEW_CHARS);
        assert!(
            !preview.ends_with('…'),
            "mask conveys truncation, not a marker"
        );

        let mut emoji = "字".repeat(PREVIEW_CHARS - 1);
        emoji.push('😀');
        emoji.push_str("尾巴");
        let preview = normalize_preview(&emoji);
        assert!(preview.ends_with('😀'), "must not split the emoji");
        assert!(!preview.contains('\u{fffd}'));
    }

    #[test]
    fn preview_collapses_blank_noise() {
        let text = "\r\n\r\n  第一行  \r\n\r\n\r\n第二行\r\n";
        assert_eq!(normalize_preview(text), "第一行\n\n第二行");
        // 全是空白的原文没有可展示内容。
        assert_eq!(normalize_preview(" \n\r\n\t"), "");
    }

    #[test]
    fn preview_drops_duplicated_title_only_when_rest_is_long_enough() {
        let text = format!("会议纪要\n{}", "正文".repeat(30));
        let stripped = strip_title_prefix(&text, Some("会议纪要"));
        assert!(stripped.starts_with("正文"));
        assert!(!stripped.contains("会议纪要"));

        // 标题几乎就是全文：切掉后没有信息量，保留原样。
        let short = "会议纪要\n下周一";
        assert_eq!(strip_title_prefix(short, Some("会议纪要")), short);
        // 标题与正文无关时原样返回。
        let other = "购物清单\n牛奶";
        assert_eq!(strip_title_prefix(other, Some("会议纪要")), other);
    }

    #[test]
    fn link_preview_strips_scheme_and_www() {
        assert_eq!(
            link_preview(Some("https://www.example.com/a/b/")).as_deref(),
            Some("example.com/a/b")
        );
        assert_eq!(
            link_preview(Some("http://example.com")).as_deref(),
            Some("example.com")
        );
        assert_eq!(link_preview(Some("   ")), None);
        assert_eq!(link_preview(None), None);
    }
}
