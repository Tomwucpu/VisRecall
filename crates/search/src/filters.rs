//! `ParsedQuery` → 候选集 SQL（design §6.3 / Task 11 Step 2）。
//!
//! 产出 `SELECT i.rowid FROM items i LEFT JOIN insights ins …  WHERE …` 与参数表；
//! **无任何过滤器时 clause 为空**（调用方据此跳过查询，直接视为全集）。
//!
//! 只有**硬条件**（时间 / `app:X` / `#tag` / 界面筛选）进 WHERE；颜色、图表、素材类型、
//! 公式是**软命中**——它们默认不进候选集，改由 [`facet_rowids`] 走单独的召回通道
//! （主色与图表类型不在 FTS 索引里，只能靠 SQL 排序）。唯一的例外是孤词
//! （[`ParsedQuery::facets_only`]）：整条查询只剩软词时退回 [`FacetMode::Hard`]。
//!
//! 参数全部用编号占位符（`?1`…），`SqlFilter::clause` 的占位符个数必须等于
//! `params.len()`——单测强制断言，避免拼接漂移。

use rusqlite::types::Value;
use visrecall_domain::item::ItemStatus;
use visrecall_domain::search::{ItemFilters, ParsedQuery};
use visrecall_domain::AppError;
use visrecall_library::Library;

/// 软条件的处理方式。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FacetMode {
    /// 软命中（默认）：只加分不排除，候选集只看硬条件。
    #[default]
    Soft,
    /// 孤词降级：软条件拼进 WHERE（`红色` = 列出红色素材）。
    Hard,
}

impl FacetMode {
    /// 按查询语义选模式：只剩软词时退回硬过滤。
    pub fn for_query(query: &ParsedQuery) -> Self {
        if query.facets_only() {
            Self::Hard
        } else {
            Self::Soft
        }
    }
}

/// 一段可执行的候选集查询。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SqlFilter {
    /// 完整 SQL；为空表示「无过滤器」（全集）。
    pub clause: String,
    pub params: Vec<Value>,
}

impl SqlFilter {
    pub fn is_empty(&self) -> bool {
        self.clause.is_empty()
    }

    /// SQL 里 `?<n>` 占位符的最大编号（单测断言用）。
    pub fn placeholder_count(&self) -> usize {
        let mut max = 0usize;
        let bytes = self.clause.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'?' {
                let mut j = i + 1;
                let mut number = 0usize;
                let mut has_digit = false;
                while j < bytes.len() && bytes[j].is_ascii_digit() {
                    number = number * 10 + usize::from(bytes[j] - b'0');
                    has_digit = true;
                    j += 1;
                }
                if has_digit {
                    max = max.max(number);
                }
            }
            i += 1;
        }
        max
    }
}

/// 把解析结果翻译成候选集 SQL。无过滤器返回空 `SqlFilter`。
///
/// 软条件的处理方式由查询自身决定（孤词 → [`FacetMode::Hard`]）。
pub fn to_sql(query: &ParsedQuery) -> Result<SqlFilter, AppError> {
    to_sql_with_filters(query, &ItemFilters::default(), FacetMode::for_query(query))
}

/// 自然语言条件 + 界面筛选条条件的合并候选集。
///
/// 两套条件都下推到 SQL：组内 OR、组间 AND。合并成**一个**候选集后，FTS 与向量
/// 通道才共用同一批 rowid——否则先召回再过滤会出现「解释说命中、结果却被筛掉」。
///
/// [`FacetMode::Soft`] 时软条件不进 WHERE（只由 [`facet_rowids`] 加分）；
/// [`FacetMode::Hard`] 时按旧逻辑拼进 WHERE，供孤词路径复用同一份 SQL。
pub fn to_sql_with_filters(
    query: &ParsedQuery,
    filters: &ItemFilters,
    facet_mode: FacetMode,
) -> Result<SqlFilter, AppError> {
    let has_facets = query.has_facets();
    if !query.has_hard_filters() && !has_facets && filters.is_empty() {
        return Ok(SqlFilter::default());
    }
    if facet_mode == FacetMode::Soft && !query.has_hard_filters() && filters.is_empty() {
        // 只有软条件：不进 WHERE（候选集 = 全集），交给 facet 通道加分
        return Ok(SqlFilter::default());
    }

    let mut clauses: Vec<String> = Vec::new();
    let mut params: Vec<Value> = Vec::new();

    if let Some(time) = &query.time {
        params.push(Value::Integer(time.from));
        params.push(Value::Integer(time.to));
        clauses.push(format!(
            "i.captured_at >= ?{} AND i.captured_at < ?{}",
            params.len() - 1,
            params.len()
        ));
    }

    if let Some(app) = &query.app {
        params.push(Value::Text(format!("%{}%", app.to_ascii_lowercase())));
        clauses.push(format!(
            "i.source_app LIKE ?{} COLLATE NOCASE",
            params.len()
        ));
    }

    // 软条件：只有孤词降级（Hard）才进 WHERE；Soft 模式由 facet 通道负责加分
    if facet_mode == FacetMode::Hard {
        push_hard_facets(&mut clauses, &mut params, query);
    }

    for tag in &query.tags {
        params.push(Value::Text(tag.clone()));
        clauses.push(format!(
            "EXISTS (SELECT 1 FROM item_tags it JOIN tags t ON t.id = it.tag_id \
             WHERE it.item_id = i.id AND t.name = ?{} COLLATE NOCASE)",
            params.len()
        ));
    }

    // ----- 界面筛选条：组内 OR、组间 AND -----
    if !filters.kinds.is_empty() {
        let placeholders: Vec<String> = filters
            .kinds
            .iter()
            .map(|kind| {
                params.push(Value::Text(kind.as_str().to_string()));
                format!("?{}", params.len())
            })
            .collect();
        clauses.push(format!("i.kind IN ({})", placeholders.join(", ")));
    }

    if !filters.statuses.is_empty() {
        let placeholders: Vec<String> = filters
            .statuses
            .iter()
            .map(|status| {
                params.push(Value::Text(status_str(*status).to_string()));
                format!("?{}", params.len())
            })
            .collect();
        clauses.push(format!("i.status IN ({})", placeholders.join(", ")));
    }

    if !filters.tags.is_empty() {
        let placeholders: Vec<String> = filters
            .tags
            .iter()
            .map(|tag| {
                params.push(Value::Text(tag.clone()));
                format!("?{}", params.len())
            })
            .collect();
        // 与自然语言 `#tag`（逐条 AND）不同：界面多选标签是「任一命中」。
        clauses.push(format!(
            "EXISTS (SELECT 1 FROM item_tags it JOIN tags t ON t.id = it.tag_id \
             WHERE it.item_id = i.id AND t.name COLLATE NOCASE IN ({}))",
            placeholders.join(", ")
        ));
    }

    if let Some(from) = filters.time_from {
        params.push(Value::Integer(from));
        clauses.push(format!("i.captured_at >= ?{}", params.len()));
    }
    if let Some(to) = filters.time_to {
        params.push(Value::Integer(to));
        clauses.push(format!("i.captured_at < ?{}", params.len()));
    }

    let where_clause = clauses.join(" AND ");
    Ok(SqlFilter {
        clause: format!(
            "SELECT i.rowid FROM items i LEFT JOIN insights ins ON ins.item_id = i.id \
             WHERE {where_clause} ORDER BY i.captured_at DESC, i.rowid DESC"
        ),
        params,
    })
}

/// 软条件的 WHERE 片段（孤词降级用）：组内 OR、组间 AND。
fn push_hard_facets(clauses: &mut Vec<String>, params: &mut Vec<Value>, query: &ParsedQuery) {
    if let Some(colors) = query.color.as_ref().filter(|colors| !colors.is_empty()) {
        let placeholders: Vec<String> = colors
            .iter()
            .map(|color| {
                params.push(Value::Text(color.clone()));
                format!("?{}", params.len())
            })
            .collect();
        // 同一素材只能有一个主色：多色 = 任一命中
        clauses.push(format!(
            "ins.primary_color IN ({})",
            placeholders.join(", ")
        ));
    }
    if let Some(chart) = &query.chart {
        params.push(Value::Text(chart.clone()));
        clauses.push(format!("ins.chart_type = ?{} COLLATE NOCASE", params.len()));
    }
    if let Some(kind) = query.content_kind {
        params.push(Value::Text(kind.as_str().to_string()));
        clauses.push(format!("i.kind = ?{}", params.len()));
    }
    if let Some(has_formula) = query.has_formula {
        params.push(Value::Integer(i64::from(has_formula)));
        clauses.push(format!("ins.has_formula = ?{}", params.len()));
    }
}

/// 软命中召回通道：命中的软条件越多排得越前，返回**有序** rowid。
///
/// 多软条件语义是 OR（任一命中即召回），`hits DESC` 让「全部命中」的素材优先；
/// 同分再按 `captured_at DESC`、`rowid DESC`，与其它通道的平局键一致（可回放）。
/// RRF 只用名次（[`crate::rank::rrf`]），所以这里不需要分数。
pub fn facet_rowids(
    library: &Library,
    query: &ParsedQuery,
    limit: u32,
) -> Result<Vec<i64>, AppError> {
    let mut params: Vec<Value> = Vec::new();
    let mut cases: Vec<String> = Vec::new();

    if let Some(colors) = query.color.as_ref().filter(|colors| !colors.is_empty()) {
        let placeholders: Vec<String> = colors
            .iter()
            .map(|color| {
                params.push(Value::Text(color.clone()));
                format!("?{}", params.len())
            })
            .collect();
        cases.push(format!(
            "CASE WHEN ins.primary_color IN ({}) THEN 1 ELSE 0 END",
            placeholders.join(", ")
        ));
    }
    if let Some(chart) = &query.chart {
        params.push(Value::Text(chart.clone()));
        cases.push(format!(
            "CASE WHEN ins.chart_type = ?{} COLLATE NOCASE THEN 1 ELSE 0 END",
            params.len()
        ));
    }
    if let Some(kind) = query.content_kind {
        params.push(Value::Text(kind.as_str().to_string()));
        cases.push(format!(
            "CASE WHEN i.kind = ?{} THEN 1 ELSE 0 END",
            params.len()
        ));
    }
    if let Some(has_formula) = query.has_formula {
        params.push(Value::Integer(i64::from(has_formula)));
        cases.push(format!(
            "CASE WHEN ins.has_formula = ?{} THEN 1 ELSE 0 END",
            params.len()
        ));
    }
    if cases.is_empty() {
        return Ok(Vec::new());
    }

    params.push(Value::Integer(i64::from(limit.max(1))));
    let sql = format!(
        "SELECT rowid FROM ( \
           SELECT i.rowid AS rowid, i.captured_at AS captured_at, ({}) AS hits \
           FROM items i LEFT JOIN insights ins ON ins.item_id = i.id \
         ) WHERE hits > 0 ORDER BY hits DESC, captured_at DESC, rowid DESC LIMIT ?{}",
        cases.join(" + "),
        params.len()
    );
    library.candidate_rowids(&sql, &params)
}

/// 内存侧过滤（SQL 候选集之外的安全网 + 解释装配用）。
///
/// 只判**硬条件**：软命中不排除任何素材，判了就会把「颜色不符但文字命中」的素材
/// 重新筛掉，等于把这次改造白做。
///
/// SQL 已经把过滤下推了，这里再判一次的理由：候选集可能因为 `CANDIDATE_INLINE_LIMIT`
/// 或调用方传 `None` 而放宽；宁可在装配阶段丢掉不符的命中，也不要给出「解释里写着
/// `#工作`、实际素材没这个标签」的结果。
pub fn passes(
    item: &visrecall_domain::Item,
    insight: Option<&visrecall_domain::Insight>,
    query: &ParsedQuery,
) -> bool {
    passes_with_filters(item, insight, query, &ItemFilters::default())
}

/// 解析条件 + 界面筛选条件的内存侧过滤（语义同 [`to_sql_with_filters`]）。
pub fn passes_with_filters(
    item: &visrecall_domain::Item,
    insight: Option<&visrecall_domain::Insight>,
    query: &ParsedQuery,
    filters: &ItemFilters,
) -> bool {
    let _ = insight; // 软命中不需要判断，参数保留以免调用方跟着改
    if let Some(time) = &query.time {
        if !time.contains(item.captured_at) {
            return false;
        }
    }
    if let Some(app) = &query.app {
        let matches = item
            .source_app
            .as_deref()
            .map(|value| {
                value
                    .to_ascii_lowercase()
                    .contains(&app.to_ascii_lowercase())
            })
            .unwrap_or(false);
        if !matches {
            return false;
        }
    }
    // 界面筛选：类型/状态/时间可以就地判断；标签在 SQL 里已用 EXISTS 过滤，
    // 内存侧不再查库（避免 N+1），交给调用方保证。
    if !filters.kinds.is_empty() && !filters.kinds.contains(&item.kind) {
        return false;
    }
    if !filters.statuses.is_empty() && !filters.statuses.contains(&item.status) {
        return false;
    }
    if let Some(from) = filters.time_from {
        if item.captured_at < from {
            return false;
        }
    }
    if let Some(to) = filters.time_to {
        if item.captured_at >= to {
            return false;
        }
    }

    true
}

/// 该素材真正命中的过滤条件（中文展示，design §6.3 的 Explanation.filters）。
pub fn matched_labels(
    item: &visrecall_domain::Item,
    insight: Option<&visrecall_domain::Insight>,
    query: &ParsedQuery,
) -> Vec<String> {
    matched_labels_with_filters(item, insight, query, &ItemFilters::default())
}

/// 解析条件 + 界面筛选条件的命中文案（解释面板的 `filters` 行）。
///
/// 只列**硬**条件；软命中走 [`matched_facets`]（两者混在一起会让用户以为软词筛掉了结果）。
pub fn matched_labels_with_filters(
    item: &visrecall_domain::Item,
    insight: Option<&visrecall_domain::Insight>,
    query: &ParsedQuery,
    filters: &ItemFilters,
) -> Vec<String> {
    let _ = insight; // 软命中不写进 filters 行
    let mut labels = Vec::new();
    if query.time.is_some() && query.time.is_some_and(|t| t.contains(item.captured_at)) {
        labels.push("时间".to_string());
    }
    if let Some(app) = &query.app {
        let hit = item.source_app.as_deref().is_some_and(|value| {
            value
                .to_ascii_lowercase()
                .contains(&app.to_ascii_lowercase())
        });
        if hit {
            labels.push(format!("来自 {app}"));
        }
    }
    for tag in &query.tags {
        labels.push(format!("#{tag}"));
    }

    // 界面筛选命中的条件（同样只在真正满足时展示）。
    if filters.kinds.contains(&item.kind) {
        labels.push(item.kind.label_zh().to_string());
    }
    if filters.statuses.contains(&item.status) {
        labels.push(item.status.label_zh().to_string());
    }
    if let Some(from) = filters.time_from {
        if item.captured_at >= from {
            labels.push("时间".to_string());
        }
    }
    for tag in &filters.tags {
        labels.push(format!("#{tag}"));
    }
    labels
}

/// 该素材真正命中的**软**条件（解释面板的 `facets` 行）。
///
/// 只在真正命中时列出（`主色 红` / `图表 漏斗图` / `类型 图片` / `公式`）：软命中是
/// 「加分项」而不是「筛选条件」，把没命中的软词也列出来会让人以为它参与了过滤。
pub fn matched_facets(
    item: &visrecall_domain::Item,
    insight: Option<&visrecall_domain::Insight>,
    query: &ParsedQuery,
) -> Vec<String> {
    let mut labels = Vec::new();
    if let Some(colors) = query.color.as_ref().filter(|colors| !colors.is_empty()) {
        if let Some(actual) = insight.and_then(|value| value.primary_color.as_deref()) {
            if colors.iter().any(|color| color == actual) {
                let label = visrecall_domain::insight::PrimaryColor::parse(actual)
                    .map(|color| color.label_zh().to_string())
                    .unwrap_or_else(|| actual.to_string());
                labels.push(format!("主色 {label}"));
            }
        }
    }
    if let Some(chart) = &query.chart {
        if insight
            .and_then(|value| value.chart_type.as_deref())
            .is_some_and(|value| value.eq_ignore_ascii_case(chart))
        {
            let label = visrecall_domain::insight::ChartType::parse(chart)
                .map(|value| value.label_zh().to_string())
                .unwrap_or_else(|| chart.clone());
            labels.push(format!("图表 {label}"));
        }
    }
    if let Some(kind) = query.content_kind {
        if item.kind == kind {
            labels.push(format!("类型 {}", kind.label_zh()));
        }
    }
    if query.has_formula == Some(true) && insight.is_some_and(|value| value.has_formula) {
        labels.push("公式".to_string());
    }
    labels
}

fn status_str(status: ItemStatus) -> &'static str {
    match status {
        ItemStatus::Parsing => "parsing",
        ItemStatus::Ready => "ready",
        ItemStatus::Pending => "pending",
        ItemStatus::Failed => "failed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use visrecall_domain::item::ItemKind;

    fn base() -> ParsedQuery {
        ParsedQuery::default()
    }

    #[test]
    fn no_filters_yields_empty_clause() {
        let filter = to_sql(&base()).unwrap();
        assert!(filter.is_empty());
        assert!(filter.params.is_empty());
        assert_eq!(filter, SqlFilter::default());
    }

    #[test]
    fn soft_facets_do_not_reach_the_where_clause() {
        let query = ParsedQuery {
            color: Some(vec!["black".into(), "gray".into()]),
            facet_words: vec!["深色".into()],
            semantic_text: "深色主题".into(),
            lexical_terms: vec!["深色".into(), "主题".into()],
            ..base()
        };
        // 软命中 + 其它语义词 → 不进 WHERE（G3）
        assert!(to_sql(&query).unwrap().is_empty(), "{:?}", to_sql(&query));
        // 孤词 → 退回硬过滤（G4）
        let solo = ParsedQuery {
            lexical_terms: vec!["深色".into()],
            ..query.clone()
        };
        let filter = to_sql(&solo).unwrap();
        assert!(filter.clause.contains("ins.primary_color IN (?1, ?2)"));
        assert_eq!(filter.params.len(), 2);
        assert_eq!(filter.placeholder_count(), filter.params.len());
    }

    #[test]
    fn all_filters_compose_with_matching_placeholder_count() {
        let query = ParsedQuery {
            time: Some(visrecall_domain::search::TimeRange::new(100, 200)),
            app: Some("chrome".into()),
            color: Some(vec!["orange".into()]),
            chart: Some("funnel".into()),
            content_kind: Some(ItemKind::Image),
            has_formula: Some(true),
            tags: vec!["工作".into(), "财务".into()],
            ..base()
        };
        // Hard：时间 2 + 应用 1 + 颜色 1 + 图表 1 + 类型 1 + 公式 1 + 标签 2 = 9
        let filter = to_sql_with_filters(&query, &ItemFilters::default(), FacetMode::Hard).unwrap();
        assert_eq!(filter.params.len(), 9);
        assert_eq!(filter.placeholder_count(), filter.params.len());
        assert!(filter.clause.contains("i.captured_at >= ?1"));
        assert!(filter.clause.contains("i.captured_at < ?2"));
        assert!(filter.clause.contains("ins.primary_color IN (?4)"));
        assert!(filter.clause.contains("ins.chart_type = ?5 COLLATE NOCASE"));
        assert!(filter.clause.contains("i.kind = ?6"));
        assert!(filter.clause.contains("ins.has_formula = ?7"));
        assert!(filter.clause.contains("t.name = ?8"));
        assert!(filter.clause.contains("t.name = ?9"));
        assert!(filter.clause.contains("ORDER BY i.captured_at DESC"));

        // Soft：只剩硬条件（时间 2 + 应用 1 + 标签 2 = 5）
        let soft = to_sql_with_filters(&query, &ItemFilters::default(), FacetMode::Soft).unwrap();
        assert_eq!(soft.params.len(), 5);
        assert_eq!(soft.placeholder_count(), soft.params.len());
        assert!(!soft.clause.contains("primary_color"));
        assert!(!soft.clause.contains("chart_type"));
        assert!(!soft.clause.contains("has_formula"));
    }

    #[test]
    fn soft_color_words_never_reach_the_where_clause() {
        // G3：正文写「红色预警」的蓝色素材不能被主色排除
        assert!(to_sql(&crate::parser::parse_query("红色预警"))
            .unwrap()
            .is_empty());
        // G4：孤词仍硬过滤
        assert!(!to_sql(&crate::parser::parse_query("红色"))
            .unwrap()
            .is_empty());
        // 词边界：`红楼` 连软命中都没有
        let honglou = crate::parser::parse_query("红楼");
        assert!(!honglou.has_facets());
        assert!(to_sql(&honglou).unwrap().is_empty());
    }

    #[test]
    fn app_phrases_produce_no_where_clause() {
        // G10：`来自X` / `在X里` / `存在文件里` 都不产生任何 WHERE
        for raw in ["来自Chrome的报错", "在微信里的聊天记录", "存在文件里"] {
            let query = crate::parser::parse_query(raw);
            assert!(query.app.is_none(), "{raw}");
            assert!(to_sql(&query).unwrap().is_empty(), "{raw}");
        }
    }

    #[test]
    fn facet_mode_follows_solo_facet_queries() {
        let solo = ParsedQuery {
            color: Some(vec!["red".into()]),
            facet_words: vec!["红色".into()],
            lexical_terms: vec!["红色".into()],
            ..base()
        };
        assert_eq!(FacetMode::for_query(&solo), FacetMode::Hard);

        let mixed = ParsedQuery {
            lexical_terms: vec!["红色".into(), "预警".into()],
            ..solo
        };
        assert_eq!(FacetMode::for_query(&mixed), FacetMode::Soft);
    }

    #[test]
    fn app_like_pattern_is_lowercased_and_wrapped() {
        let query = ParsedQuery {
            app: Some("Chrome".into()),
            ..base()
        };
        let filter = to_sql(&query).unwrap();
        assert_eq!(filter.params, vec![Value::Text("%chrome%".into())]);
        assert!(filter.clause.contains("COLLATE NOCASE"));
    }

    #[test]
    fn empty_color_list_is_ignored() {
        let query = ParsedQuery {
            color: Some(Vec::new()),
            ..base()
        };
        // 空颜色数组不算过滤器
        assert!(to_sql(&query).unwrap().is_empty());
    }

    #[test]
    fn tag_filter_uses_exists_subquery() {
        let query = ParsedQuery {
            tags: vec!["工作".into()],
            ..base()
        };
        let filter = to_sql(&query).unwrap();
        assert!(filter.clause.contains("EXISTS (SELECT 1 FROM item_tags it"));
        assert!(filter.clause.contains("COLLATE NOCASE"));
        assert_eq!(filter.placeholder_count(), 1);
    }

    #[test]
    fn placeholder_count_ignores_plain_question_marks_in_text() {
        let filter = SqlFilter {
            clause: "SELECT 1 WHERE name = 'what?' AND id = ?3".into(),
            params: Vec::new(),
        };
        assert_eq!(filter.placeholder_count(), 3);
    }
}
