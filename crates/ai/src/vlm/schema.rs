//! VLM JSON 契约的严格校验（design §4.5）。
//!
//! 契约：模型必须返回**纯 JSON**（不许 Markdown 代码块）。校验流程：
//! 1. 剥离可能的 ```` ```json ```` 包裹；
//! 2. serde 反序列化成强类型（未知字段忽略、缺省字段可空）；
//! 3. 枚举合法性校验（`content_type` / `primary_color` / `chart_type` / `ui_type`）；
//! 4. 长度约束（`summary` ≤200 字符给修复留余量、`tags` ≤8 个且逐个 ≤32 字符——超限截断不报错）。
//!
//! 任一步失败返回 `SchemaViolation`，由调用方把错误与原文回传模型**修复一次**。

use serde::Deserialize;
use visrecall_domain::insight::{ChartType, ContentType, PrimaryColor, UiType};
use visrecall_domain::providers::{TextInsight, VisionInsight};
use visrecall_domain::AppError;

/// 摘要长度上限（契约写 ≤120 字，这里留修复余量到 200，超限截断）。
pub const MAX_SUMMARY_CHARS: usize = 200;
/// 标签数量上限。
pub const MAX_TAGS: usize = 8;
/// 单个标签长度上限。
pub const MAX_TAG_CHARS: usize = 32;

/// VLM 原始返回结构（字段全部宽松，校验在 [`to_vision_insight`] 里做）。
#[derive(Debug, Deserialize)]
struct RawVision {
    summary: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    content_type: Option<String>,
    #[serde(default)]
    primary_color: Option<String>,
    #[serde(default)]
    chart_type: Option<String>,
    #[serde(default)]
    ui_type: Option<String>,
    #[serde(default)]
    entities: Vec<String>,
    #[serde(default)]
    latex: Option<String>,
    #[serde(default)]
    has_formula: Option<bool>,
    #[serde(default)]
    confidence: Option<f32>,
}

#[derive(Debug, Deserialize)]
struct RawText {
    summary: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    confidence: Option<f32>,
}

/// 剥离 Markdown 代码块包裹（```json ... ```）。
pub fn strip_code_fence(raw: &str) -> &str {
    let trimmed = raw.trim();
    let Some(rest) = trimmed.strip_prefix("```") else {
        return trimmed;
    };
    // 去掉语言标记行（```json / ```JSON / ```）
    let rest = rest.split_once('\n').map(|(_, body)| body).unwrap_or("");
    rest.trim_end().trim_end_matches("```").trim()
}

/// 校验图片理解结果。
pub fn validate_vision(raw: &str) -> Result<VisionInsight, AppError> {
    let payload = strip_code_fence(raw);
    if payload.is_empty() {
        return Err(AppError::SchemaViolation("empty response".into()));
    }
    let parsed: RawVision = serde_json::from_str(payload)
        .map_err(|e| AppError::SchemaViolation(format!("{e}; raw={}", truncate(payload, 400))))?;
    to_vision_insight(parsed)
}

/// 校验文本摘要结果。
pub fn validate_text(raw: &str) -> Result<TextInsight, AppError> {
    let payload = strip_code_fence(raw);
    if payload.is_empty() {
        return Err(AppError::SchemaViolation("empty response".into()));
    }
    let parsed: RawText = serde_json::from_str(payload)
        .map_err(|e| AppError::SchemaViolation(format!("{e}; raw={}", truncate(payload, 400))))?;
    let summary = clean_summary(&parsed.summary)?;
    Ok(TextInsight {
        summary,
        tags: clean_tags(parsed.tags),
        confidence: parsed.confidence.map(|c| c.clamp(0.0, 1.0)),
    })
}

/// 修复提示词：把「哪里错」与「上次输出」一起回传，要求只重发 JSON。
pub fn repair_instruction(error: &str) -> String {
    format!(
        "你上一次的输出不是合法 JSON 或不符合 schema：{error}\n\
         请只输出一个 JSON 对象（不要 Markdown 代码块、不要解释文字），字段与类型必须与要求一致。"
    )
}

fn to_vision_insight(raw: RawVision) -> Result<VisionInsight, AppError> {
    let summary = clean_summary(&raw.summary)?;
    let content_type = parse_enum::<ContentType>("content_type", raw.content_type.as_deref())?
        .map(|v| v.as_str().to_string());
    let primary_color = parse_enum::<PrimaryColor>("primary_color", raw.primary_color.as_deref())?
        .map(|v| v.as_str().to_string());
    let chart_type = parse_enum::<ChartType>("chart_type", raw.chart_type.as_deref())?
        .map(|v| v.as_str().to_string());
    let ui_type =
        parse_enum::<UiType>("ui_type", raw.ui_type.as_deref())?.map(|v| v.as_str().to_string());

    let latex = raw
        .latex
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty());

    Ok(VisionInsight {
        summary,
        tags: clean_tags(raw.tags),
        content_type,
        primary_color,
        chart_type,
        ui_type,
        entities: clean_entities(raw.entities),
        has_formula: raw.has_formula.unwrap_or(false) || latex.is_some(),
        latex,
        confidence: raw.confidence.map(|c| c.clamp(0.0, 1.0)),
    })
}

fn clean_summary(summary: &str) -> Result<String, AppError> {
    let trimmed = summary.trim();
    if trimmed.is_empty() {
        return Err(AppError::SchemaViolation("summary is empty".into()));
    }
    Ok(truncate(trimmed, MAX_SUMMARY_CHARS))
}

fn clean_tags(tags: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for tag in tags {
        let trimmed = tag.trim();
        if trimmed.is_empty() {
            continue;
        }
        // 超长标签截断而不是拒绝：删掉一整个标签会让用户看到「莫名其妙的缺项」
        let value = truncate(trimmed, MAX_TAG_CHARS);
        if !out.iter().any(|t| t == &value) {
            out.push(value);
        }
        if out.len() >= MAX_TAGS {
            break;
        }
    }
    out
}

fn clean_entities(entities: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for entity in entities {
        let trimmed = entity.trim();
        if trimmed.is_empty() || trimmed.chars().count() > 40 {
            continue;
        }
        if !out.iter().any(|e| e == trimmed) {
            out.push(trimmed.to_string());
        }
        if out.len() >= 12 {
            break;
        }
    }
    out
}

fn parse_enum<T: EnumLiteral>(field: &str, raw: Option<&str>) -> Result<Option<T>, AppError> {
    let Some(raw) = raw.map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(None);
    };
    T::parse_literal(raw)
        .map(Some)
        .ok_or_else(|| AppError::SchemaViolation(format!("invalid {field}: {raw}")))
}

/// 枚举字面量解析（`domain::insight` 的四个枚举都实现它）。
trait EnumLiteral: Sized {
    fn parse_literal(raw: &str) -> Option<Self>;
}

impl EnumLiteral for ContentType {
    fn parse_literal(raw: &str) -> Option<Self> {
        Self::parse(raw)
    }
}

impl EnumLiteral for PrimaryColor {
    fn parse_literal(raw: &str) -> Option<Self> {
        Self::parse(raw)
    }
}

impl EnumLiteral for ChartType {
    fn parse_literal(raw: &str) -> Option<Self> {
        Self::parse(raw)
    }
}

impl EnumLiteral for UiType {
    fn parse_literal(raw: &str) -> Option<Self> {
        Self::parse(raw)
    }
}

fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    text.chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"{
        "summary": "季度营收漏斗图，显示转化率提升",
        "tags": ["营收", "漏斗图", "周报"],
        "content_type": "screenshot",
        "primary_color": "orange",
        "chart_type": "funnel",
        "ui_type": "list",
        "entities": ["VisRecall"],
        "latex": "",
        "has_formula": false,
        "confidence": 0.85
    }"#;

    #[test]
    fn valid_payload_maps_all_fields() {
        let insight = validate_vision(GOOD).expect("合法 JSON 应通过");
        assert_eq!(insight.tags.len(), 3);
        assert_eq!(insight.content_type.as_deref(), Some("screenshot"));
        assert_eq!(insight.primary_color.as_deref(), Some("orange"));
        assert_eq!(insight.chart_type.as_deref(), Some("funnel"));
        assert_eq!(insight.ui_type.as_deref(), Some("list"));
        assert_eq!(insight.entities, vec!["VisRecall"]);
        assert!(insight.latex.is_none());
        assert!(!insight.has_formula);
        assert!((insight.confidence.unwrap() - 0.85).abs() < 1e-6);
    }

    #[test]
    fn markdown_fence_is_stripped() {
        let fenced = format!("```json\n{GOOD}\n```");
        assert!(validate_vision(&fenced).is_ok());
        let generic = format!("```\n{GOOD}\n```");
        assert!(validate_vision(&generic).is_ok());
    }

    #[test]
    fn malformed_json_is_a_schema_violation() {
        let err = validate_vision(r#"{"summary": "截断"#).expect_err("截断 JSON 必须报错");
        assert!(matches!(err, AppError::SchemaViolation(_)), "{err:?}");
    }

    #[test]
    fn invalid_enum_is_a_schema_violation() {
        let payload = GOOD.replace("\"orange\"", "\"橙色\"");
        let err = validate_vision(&payload).expect_err("非法枚举必须报错");
        assert!(matches!(err, AppError::SchemaViolation(_)), "{err:?}");
        assert!(err.to_string().contains("primary_color"));
    }

    #[test]
    fn empty_summary_is_rejected() {
        let payload = GOOD.replace("季度营收漏斗图，显示转化率提升", "   ");
        assert!(validate_vision(&payload).is_err());
    }

    #[test]
    fn over_long_values_are_truncated_not_rejected() {
        let long_summary = "长".repeat(MAX_SUMMARY_CHARS + 50);
        let many_tags: Vec<String> = (0..20).map(|i| format!("标签{i}")).collect();
        let payload = serde_json::json!({
            "summary": long_summary,
            "tags": many_tags,
            "content_type": "screenshot",
            "primary_color": null,
            "chart_type": null,
            "ui_type": null,
            "entities": [],
            "latex": null,
            "has_formula": false,
            "confidence": 1.7
        })
        .to_string();
        let insight = validate_vision(&payload).expect("超限应截断而不是报错");
        assert_eq!(insight.summary.chars().count(), MAX_SUMMARY_CHARS);
        assert_eq!(insight.tags.len(), MAX_TAGS);
        assert!((insight.confidence.unwrap() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn missing_optional_fields_are_fine() {
        let payload = r#"{"summary": "只有摘要"}"#;
        let insight = validate_vision(payload).expect("可选字段可缺省");
        assert!(insight.tags.is_empty());
        assert!(insight.primary_color.is_none());
        assert!(!insight.has_formula);
    }

    #[test]
    fn latex_presence_implies_formula() {
        let payload = r#"{"summary": "公式推导", "latex": "E=mc^2", "has_formula": false}"#;
        let insight = validate_vision(payload).unwrap();
        assert_eq!(insight.latex.as_deref(), Some("E=mc^2"));
        assert!(insight.has_formula, "有 latex 必须置 has_formula");
    }

    #[test]
    fn text_payload_validates() {
        let insight = validate_text(r#"{"summary": "一份会议纪要", "tags": ["会议"]}"#).unwrap();
        assert_eq!(insight.summary, "一份会议纪要");
        assert_eq!(insight.tags, vec!["会议"]);
        assert!(validate_text("not json").is_err());
    }

    #[test]
    fn repair_instruction_mentions_error() {
        let text = repair_instruction("invalid primary_color: 橙色");
        assert!(text.contains("invalid primary_color: 橙色"));
        assert!(text.contains("JSON"));
    }
}
