//! Search document 构造（索引侧，design §4.4）。
//!
//! **字段顺序即权重**：`标题 > 摘要 > 标签 > 实体 > 备注 > OCR/原文`。截断只发生在
//! 预算耗尽处，因此顺序靠前的字段必然完整保留——这正是「标题永远不该被 OCR 全文挤掉」
//! 的实现方式。
//!
//! 预算用字符数近似 512 token（T0 实测：中文约 1 字 ≈ 1 token，取 1600 字留余量；
//! 嵌入侧 `MAX_TOKENS` 仍会做硬截断，这里只是让拼接结果本身可控）。

/// 拼接预算（字符数）。
pub const DOC_CHAR_BUDGET: usize = 1600;

/// 字段分隔符（中文分号，不参与分词，只为人读）。
pub const FIELD_SEPARATOR: &str = "；";

/// 构造 search document 的输入。空字段会被跳过。
#[derive(Debug, Clone, Copy, Default)]
pub struct DocumentFields<'a> {
    pub title: &'a str,
    pub summary: &'a str,
    pub tags: &'a str,
    pub entities: &'a str,
    pub note: &'a str,
    /// 图片为 OCR 全文，文本/链接为原文。
    pub ocr_or_original: &'a str,
}

/// 按「标题 > 摘要 > 标签 > 实体 > 备注 > OCR/原文」拼接到预算上限。
pub fn build_search_document(fields: &DocumentFields<'_>) -> String {
    let ordered = [
        fields.title,
        fields.summary,
        fields.tags,
        fields.entities,
        fields.note,
        fields.ocr_or_original,
    ];

    let mut out = String::new();
    let mut used = 0usize;
    for field in ordered {
        let trimmed = field.trim();
        if trimmed.is_empty() {
            continue;
        }
        let separator_chars = if out.is_empty() {
            0
        } else {
            FIELD_SEPARATOR.chars().count()
        };
        if used + separator_chars >= DOC_CHAR_BUDGET {
            break;
        }
        if !out.is_empty() {
            out.push_str(FIELD_SEPARATOR);
            used += separator_chars;
        }
        let remaining = DOC_CHAR_BUDGET - used;
        let field_len = trimmed.chars().count();
        if field_len <= remaining {
            out.push_str(trimmed);
            used += field_len;
        } else {
            // 预算在字段中间耗尽：截断后直接结束（后续字段权重更低）
            out.extend(trimmed.chars().take(remaining));
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_fields_are_skipped_and_order_is_preserved() {
        let doc = build_search_document(&DocumentFields {
            title: "季度营收漏斗图",
            summary: "转化率提升",
            tags: "",
            entities: "",
            note: "周会",
            ocr_or_original: "",
        });
        assert_eq!(doc, "季度营收漏斗图；转化率提升；周会");
    }

    #[test]
    fn long_ocr_text_is_truncated_but_title_survives() {
        let long_ocr = "测".repeat(DOC_CHAR_BUDGET * 2);
        let doc = build_search_document(&DocumentFields {
            title: "标题必须完整保留",
            summary: "摘要",
            tags: "工作",
            entities: "VisRecall",
            note: "备注",
            ocr_or_original: &long_ocr,
        });
        assert_eq!(doc.chars().count(), DOC_CHAR_BUDGET);
        assert!(doc.starts_with("标题必须完整保留；摘要；工作；VisRecall；备注；"));
        assert!(doc.contains("测"));
    }

    #[test]
    fn whitespace_only_fields_are_treated_as_empty() {
        let doc = build_search_document(&DocumentFields {
            title: "  ",
            summary: "\n",
            tags: "",
            entities: "",
            note: "",
            ocr_or_original: "原文",
        });
        assert_eq!(doc, "原文");
    }

    #[test]
    fn all_empty_yields_empty_document() {
        assert!(build_search_document(&DocumentFields::default()).is_empty());
    }

    #[test]
    fn separator_does_not_blow_the_budget() {
        let doc = build_search_document(&DocumentFields {
            title: &"a".repeat(DOC_CHAR_BUDGET - 1),
            summary: "b",
            ..Default::default()
        });
        // 预算不足一个分隔符 + 一个字符时直接停，绝不超预算
        assert_eq!(doc.chars().count(), DOC_CHAR_BUDGET - 1);

        let doc = build_search_document(&DocumentFields {
            title: &"a".repeat(DOC_CHAR_BUDGET - 3),
            summary: "b",
            ..Default::default()
        });
        assert_eq!(doc.chars().count(), DOC_CHAR_BUDGET - 1);
        assert!(doc.ends_with("；b"));
    }
}
