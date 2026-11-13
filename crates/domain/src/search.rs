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

/// 解析后的查询：硬过滤 + 软命中 + 语义文本 + 字面词。
///
/// 两类条件的语义差别（本模块的「一个词只消费一次」纪律据此分层）：
/// - **硬条件**（`time` / `app` / `tags`）从 `semantic_text` 里**剥离**——它们被下推成
///   WHERE，同时再进语义通道只会让同一段文字既过滤又打分（「上个月」）；
/// - **软条件**（`color` / `chart` / `content_kind` / `has_formula`）**不剥离**，原词仍留在
///   `semantic_text` / `lexical_terms` 里，只额外提供一条「软命中」召回通道。词边界与
///   单字守卫在解析器里完成（`红楼` 不该被读成「红」）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParsedQuery {
    pub time: Option<TimeRange>,
    /// 应用名（**只由 `app:X` 显式语法产生**；小写归一，SQL 用 `LIKE %X%` COLLATE NOCASE）。
    ///
    /// `来自X` / `在X里` 这类自然语言形式不再解析（误判率高：存在/现在/正在），
    /// 整句按普通文本进字面与语义通道。
    pub app: Option<String>,
    /// 主色枚举集（同义词可映射多个，如 深色 → [black, gray]）。**软命中条件**。
    pub color: Option<Vec<String>>,
    /// 图表类型枚举值。**软命中条件**。
    pub chart: Option<String>,
    /// 素材类型。**软命中条件**。
    pub content_kind: Option<ItemKind>,
    /// 是否要求含公式。**软命中条件**。
    pub has_formula: Option<bool>,
    /// 标签名（精确匹配，NOCASE）。**硬条件**。
    pub tags: Vec<String>,
    /// 剩余文本（进入嵌入与 FTS）。软词不从里面剥离。
    pub semantic_text: String,
    /// `semantic_text` 的分词结果（去停用词）。
    pub lexical_terms: Vec<String>,
    /// 引号内文本的分词结果（AND 语义）。
    pub quoted_terms: Vec<String>,
    /// 软命中命中的**原词**（`红色预警` → `红色`），用于孤词判定与 chips 区分。
    ///
    /// 反序列化旧记录（`queries.parsed_json`）时缺该字段，按空数组读。
    #[serde(default)]
    pub facet_words: Vec<String>,
}

impl ParsedQuery {
    /// 是否有**硬过滤**条件（时间 / `app:X` / `#tag`）——只有它们进候选集 WHERE。
    pub fn has_hard_filters(&self) -> bool {
        self.time.is_some() || self.app.is_some() || !self.tags.is_empty()
    }

    /// 是否有软命中条件（颜色 / 图表 / 类型 / 公式）。
    ///
    /// 注意 `color: Some(vec![])` 视为「没有颜色条件」：解析器不会产出空集，但上层补丁
    /// （筛选点击）可能出现空数组。
    pub fn has_facets(&self) -> bool {
        self.color.as_ref().is_some_and(|colors| !colors.is_empty())
            || self.chart.is_some()
            || self.content_kind.is_some()
            || self.has_formula.is_some()
    }

    /// 查询里**只剩软命中词**（孤词）：例如 `红色` / `柱状图` / `橙色漏斗图`。
    ///
    /// 判据：软条件非空、无硬条件、无引号，且每个查询词都来自软命中原词。
    /// 此时软条件退回硬过滤（D3-Q05），保留「输入 `红色` = 列出红色素材」的直觉。
    pub fn facets_only(&self) -> bool {
        self.has_facets()
            && !self.has_hard_filters()
            && self.quoted_terms.is_empty()
            && !self.lexical_terms.is_empty()
            && self
                .lexical_terms
                .iter()
                .all(|term| self.facet_words.iter().any(|word| word == term))
    }

    /// 是否有任何结构化条件（硬 + 软）。
    ///
    /// 历史名称保留：语义收窄为 [`Self::has_hard_filters`]，调用方需要「软条件也要算进去」
    /// 时显式用 [`Self::has_facets`]。
    pub fn has_filters(&self) -> bool {
        self.has_hard_filters()
    }

    /// 是否完全空查询（无过滤、无语义词、无引号）。
    pub fn is_empty(&self) -> bool {
        !self.has_filters()
            && !self.has_facets()
            && self.semantic_text.trim().is_empty()
            && self.quoted_terms.is_empty()
    }

    /// 硬条件 chips（会进 WHERE 的条件）。
    pub fn hard_chips(&self) -> Vec<String> {
        let mut chips = Vec::new();
        if let Some(time) = &self.time {
            chips.push(format!("时间:{}~{}", time.from, time.to));
        }
        if let Some(app) = &self.app {
            chips.push(format!("来自 {app}"));
        }
        for tag in &self.tags {
            chips.push(format!("#{tag}"));
        }
        for quoted in &self.quoted_terms {
            chips.push(format!("\"{quoted}\""));
        }
        chips
    }

    /// 软命中 chips（只加分不排除，展示时加「(参考)」后缀以示区别）。
    pub fn soft_chips(&self) -> Vec<String> {
        if !self.facet_words.is_empty() {
            return self
                .facet_words
                .iter()
                .map(|word| format!("{word}(参考)"))
                .collect();
        }
        // 手工构造（或旧记录反序列化）时没有原词，退回枚举标签，避免 chip 与条件不一致
        let mut chips = Vec::new();
        if let Some(colors) = self.color.as_ref().filter(|colors| !colors.is_empty()) {
            chips.push(format!("{}(参考)", colors.join("/")));
        }
        if let Some(chart) = &self.chart {
            chips.push(format!("{chart}(参考)"));
        }
        if let Some(kind) = &self.content_kind {
            chips.push(format!("{}(参考)", kind.label_zh()));
        }
        if self.has_formula == Some(true) {
            chips.push("公式(参考)".to_string());
        }
        chips
    }

    /// 前端 chips（硬条件在前、软命中在后）。
    pub fn chips(&self) -> Vec<String> {
        let mut chips = self.hard_chips();
        chips.extend(self.soft_chips());
        chips
    }
}

/// 一条命中结果的解释（design §4.5 / §6.3）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Explanation {
    /// 实际命中的**硬**条件（中文展示，如「#工作」）。
    pub filters: Vec<String>,
    /// 实际命中的**软**条件（如「主色 红」「图表 漏斗图」「类型 图片」）。
    ///
    /// 与 `filters` 分开：软条件只加分不排除，混在一起会让用户以为它们筛掉了结果。
    #[serde(default)]
    pub facets: Vec<String>,
    /// FTS 实际命中的词（与查询词的交集）。
    pub terms: Vec<String>,
    /// 仅语义命中（无 FTS 命中、无硬条件、无软命中）。
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
        assert!(filtered.has_hard_filters());
        assert!(!filtered.is_empty());
    }

    #[test]
    fn soft_facets_are_not_hard_filters() {
        let query = ParsedQuery {
            color: Some(vec!["red".into()]),
            facet_words: vec!["红色".into()],
            semantic_text: "红色预警".into(),
            lexical_terms: vec!["红色".into(), "预警".into()],
            ..Default::default()
        };
        assert!(query.has_facets());
        assert!(!query.has_hard_filters());
        assert!(!query.has_filters(), "软条件不算硬过滤");
        assert!(!query.facets_only(), "还有非软词 → 不是孤词");

        // 孤词：每个查询词都来自软命中原词
        let solo = ParsedQuery {
            color: Some(vec!["red".into()]),
            facet_words: vec!["红色".into()],
            semantic_text: "红色".into(),
            lexical_terms: vec!["红色".into()],
            ..Default::default()
        };
        assert!(solo.facets_only());

        // 带硬条件（标签）就不算孤词
        let with_tag = ParsedQuery {
            tags: vec!["工作".into()],
            ..solo.clone()
        };
        assert!(!with_tag.facets_only());

        // 只剩软词但带引号：字面通道仍在，不降级
        let quoted = ParsedQuery {
            quoted_terms: vec!["红色".into()],
            ..solo
        };
        assert!(!quoted.facets_only());
    }

    #[test]
    fn chips_render_hard_and_soft_conditions() {
        let query = ParsedQuery {
            app: Some("chrome".into()),
            color: Some(vec!["orange".into()]),
            chart: Some("funnel".into()),
            content_kind: Some(ItemKind::Image),
            has_formula: Some(true),
            tags: vec!["工作".into()],
            quoted_terms: vec!["季度 营收".into()],
            facet_words: vec!["橙色".into(), "漏斗图".into()],
            ..Default::default()
        };
        let chips = query.chips();
        assert!(chips.iter().any(|chip| chip.contains("chrome")));
        assert!(chips.iter().any(|chip| chip == "#工作"));
        assert!(chips.iter().any(|chip| chip.contains("季度")));
        assert!(chips.iter().any(|chip| chip == "橙色(参考)"));
        assert!(chips.iter().any(|chip| chip == "漏斗图(参考)"));

        // 硬/软分开取：前端据此渲染两种样式
        assert!(query.hard_chips().iter().all(|chip| !chip.contains("参考")));
        assert_eq!(
            query.soft_chips(),
            vec!["橙色(参考)".to_string(), "漏斗图(参考)".to_string()]
        );
    }
}
