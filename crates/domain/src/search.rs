//! 检索侧的共享类型与常量。
//!
//! 分词器版本必须同时被「索引写入侧」(`library`) 与「查询侧」(`search`) 读到，
//! 因此常量放在 domain：`crates/search` 只是 re-export，保证只有一处真相。

use serde::{Deserialize, Serialize};

use crate::insight::OcrBox;
use crate::item::{Item, ItemKind, ItemStatus};

/// 分词器版本（写入 `settings['fts.tokenizer_version']`，与依赖版本同步维护）。
pub const TOKENIZER_VERSION: &str = "jieba-rs-0.11";

/// `settings` 键：分词器版本。
pub const TOKENIZER_VERSION_KEY: &str = "fts.tokenizer_version";

/// `settings` 键：RRF 参数（`{"k":60,"weights":{"fts":1.0,"semantic":1.0}}`）。
pub const RRF_SETTINGS_KEY: &str = "search.rrf";

/// 时间范围（毫秒时间戳，左闭右开）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeRange {
    pub from: i64,
    pub to: i64,
}

impl TimeRange {
    pub fn new(from: i64, to: i64) -> Self {
        Self { from, to }
    }

    pub fn contains(&self, timestamp: i64) -> bool {
        timestamp >= self.from && timestamp < self.to
    }
}

/// 界面筛选条的结构化条件（与自然语言解析出的 [`ParsedQuery`] 合并使用）。
///
/// 语义：**组内 OR、组间 AND**。例如 `kinds = [image, text]` 且 `tags = [工作]`
/// 表示「图片或文本，且带工作标签」；空数组表示该维度不限制。
/// 时间沿用左闭右开（`time_from` 包含、`time_to` 不包含），与 [`TimeRange`] 一致。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemFilters {
    #[serde(default)]
    pub kinds: Vec<ItemKind>,
    #[serde(default)]
    pub statuses: Vec<ItemStatus>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub time_from: Option<i64>,
    pub time_to: Option<i64>,
}

impl ItemFilters {
    /// 是否没有任何条件（空筛选应完全跳过 SQL 拼装）。
    pub fn is_empty(&self) -> bool {
        self.kinds.is_empty()
            && self.statuses.is_empty()
            && self.tags.is_empty()
            && self.time_from.is_none()
            && self.time_to.is_none()
    }
}

/// 解析后的查询：结构化过滤 + 语义文本 + 字面词。
///
/// `semantic_text` 是**剥离**已消费片段后的剩余原文——保证「上个月」这类词不会同时
/// 作为过滤条件与语义查询词（design §6.6 的「一个词只消费一次」）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParsedQuery {
    pub time: Option<TimeRange>,
    /// 应用名（小写归一；SQL 用 `LIKE %X%` COLLATE NOCASE）。
    pub app: Option<String>,
    /// 主色枚举集（同义词可映射多个，如 深色 → [black, gray]）。
    pub color: Option<Vec<String>>,
    /// 图表类型枚举值。
    pub chart: Option<String>,
    /// 素材类型。
    pub content_kind: Option<ItemKind>,
    /// 是否要求含公式。
    pub has_formula: Option<bool>,
    /// 标签名（精确匹配，NOCASE）。
    pub tags: Vec<String>,
    /// 剩余文本（进入嵌入与 FTS）。
    pub semantic_text: String,
    /// `semantic_text` 的分词结果（去停用词）。
    pub lexical_terms: Vec<String>,
    /// 引号内文本的分词结果（AND 语义）。
    pub quoted_terms: Vec<String>,
}

impl ParsedQuery {
    /// 是否没有任何结构化条件（只有语义文本）。
    ///
    /// 注意 `color: Some(vec![])` 视为「没有颜色条件」：解析器不会产出空集，但上层补丁
    /// （筛选点击）可能出现空数组，不能让空 WHERE 出现在 SQL 里。
    pub fn has_filters(&self) -> bool {
        self.time.is_some()
            || self.app.is_some()
            || self.color.as_ref().is_some_and(|colors| !colors.is_empty())
            || self.chart.is_some()
            || self.content_kind.is_some()
            || self.has_formula.is_some()
            || !self.tags.is_empty()
    }

    /// 是否完全空查询（无过滤、无语义词、无引号）。
    pub fn is_empty(&self) -> bool {
        !self.has_filters() && self.semantic_text.trim().is_empty() && self.quoted_terms.is_empty()
    }

    /// 前端 chips（中文展示，顺序与规则表一致）。
    pub fn chips(&self) -> Vec<String> {
        let mut chips = Vec::new();
        if let Some(time) = &self.time {
            chips.push(format!("时间:{}~{}", time.from, time.to));
        }
        if let Some(app) = &self.app {
            chips.push(format!("来自 {app}"));
        }
        if let Some(colors) = &self.color {
            chips.push(colors.join("/"));
        }
        if let Some(chart) = &self.chart {
            chips.push(chart.clone());
        }
        if let Some(kind) = &self.content_kind {
            chips.push(kind.label_zh().to_string());
        }
        if self.has_formula == Some(true) {
            chips.push("公式".to_string());
        }
        for tag in &self.tags {
            chips.push(format!("#{tag}"));
        }
        for quoted in &self.quoted_terms {
            chips.push(format!("\"{quoted}\""));
        }
        chips
    }
}

/// 一条命中结果的解释（design §4.5 / §6.3）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Explanation {
    /// 实际命中的结构化条件（中文展示，如「橙色」）。
    pub filters: Vec<String>,
    /// FTS 实际命中的词（与查询词的交集）。
    pub terms: Vec<String>,
    /// 仅语义命中（无 FTS/过滤命中）。
    pub semantic: bool,
    /// OCR 命中词对应的归一化框（可能为空：无 OCR 或未命中）。
    pub highlight_boxes: Vec<OcrBox>,
}

/// 一条带分数与解释的搜索结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScoredItem {
    pub item: Item,
    pub score: f32,
    pub explanation: Explanation,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_range_is_half_open() {
        let range = TimeRange::new(100, 200);
        assert!(!range.contains(99));
        assert!(range.contains(100));
        assert!(range.contains(199));
        assert!(!range.contains(200));
    }

    #[test]
    fn empty_query_detection() {
        let mut query = ParsedQuery::default();
        assert!(query.is_empty());
        query.semantic_text = "发票".into();
        assert!(!query.is_empty());
        let mut filtered = ParsedQuery::default();
        filtered.tags.push("工作".into());
        assert!(filtered.has_filters());
        assert!(!filtered.is_empty());
    }

    #[test]
    fn chips_render_filters() {
        let query = ParsedQuery {
            app: Some("chrome".into()),
            color: Some(vec!["orange".into()]),
            chart: Some("funnel".into()),
            content_kind: Some(ItemKind::Image),
            has_formula: Some(true),
            tags: vec!["工作".into()],
            quoted_terms: vec!["季度 营收".into()],
            ..Default::default()
        };
        let chips = query.chips();
        assert!(chips.iter().any(|chip| chip.contains("chrome")));
        assert!(chips.iter().any(|chip| chip == "orange"));
        assert!(chips.iter().any(|chip| chip == "#工作"));
        assert!(chips.iter().any(|chip| chip.contains("季度")));
    }
}
