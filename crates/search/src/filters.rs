//! `ParsedQuery` → 候选集 SQL（design §6.3 / Task 11 Step 2）。
//!
//! 产出 `SELECT i.rowid FROM items i LEFT JOIN insights ins …  WHERE …` 与参数表；
//! **无任何过滤器时 clause 为空**（调用方据此跳过查询，直接视为全集）。
//!
//! 参数全部用编号占位符（`?1`…），`SqlFilter::clause` 的占位符个数必须等于
//! `params.len()`——单测强制断言，避免拼接漂移。

use rusqlite::types::Value;
use visrecall_domain::item::ItemStatus;
use visrecall_domain::search::{ItemFilters, ParsedQuery};
use visrecall_domain::AppError;

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
pub fn to_sql(query: &ParsedQuery) -> Result<SqlFilter, AppError> {
    to_sql_with_filters(query, &ItemFilters::default())
}

/// 自然语言条件 + 界面筛选条条件的合并候选集。
///
/// 两套条件都下推到 SQL：组内 OR、组间 AND。合并成**一个**候选集后，FTS 与向量
/// 通道才共用同一批 rowid——否则先召回再过滤会出现「解释说命中、结果却被筛掉」。
pub fn to_sql_with_filters(
    query: &ParsedQuery,
    filters: &ItemFilters,
) -> Result<SqlFilter, AppError> {
    if !query.has_filters() && filters.is_empty() {
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

    if let Some(kind) = query.content_kind {
        params.push(Value::Text(kind.as_str().to_string()));
        clauses.push(format!("i.kind = ?{}", params.len()));
    }

    if let Some(colors) = &query.color {
        if !colors.is_empty() {
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
    }

    if let Some(chart) = &query.chart {
        params.push(Value::Text(chart.clone()));
        clauses.push(format!("ins.chart_type = ?{} COLLATE NOCASE", params.len()));
    }

    if let Some(has_formula) = query.has_formula {
        params.push(Value::Integer(i64::from(has_formula)));
        clauses.push(format!("ins.has_formula = ?{}", params.len()));
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

/// 内存侧过滤（SQL 候选集之外的安全网 + 解释装配用）。
///
/// SQL 已经把过滤下推了，这里再判一次的理由：候选集可能因为 `CANDIDATE_INLINE_LIMIT`
/// 或调用方传 `None` 而放宽；宁可在装配阶段丢掉不符的命中，也不要给出「解释里写着橙色、
/// 实际素材是蓝色」的结果。
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
    if let Some(kind) = query.content_kind {
        if item.kind != kind {
            return false;
        }
    }
    if let Some(colors) = query.color.as_ref().filter(|c| !c.is_empty()) {
        let actual = insight.and_then(|value| value.primary_color.as_deref());
        match actual {
            Some(actual) if colors.iter().any(|color| color == actual) => {}
            _ => return false,
        }
    }
    if let Some(chart) = &query.chart {
        let actual = insight.and_then(|value| value.chart_type.as_deref());
        if actual.is_none_or(|value| value != chart) {
            return false;
        }
    }
    if let Some(has_formula) = query.has_formula {
        let actual = insight.is_some_and(|value| value.has_formula);
        if actual != has_formula {
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

/// 解析条件 + 界面筛选条件的命中文案（解释面板的 filters 行）。
pub fn matched_labels_with_filters(
    item: &visrecall_domain::Item,
    insight: Option<&visrecall_domain::Insight>,
    query: &ParsedQuery,
    filters: &ItemFilters,
) -> Vec<String> {
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
    if let Some(kind) = query.content_kind {
        if item.kind == kind {
            labels.push(kind.label_zh().to_string());
        }
    }
    if let Some(colors) = query.color.as_ref().filter(|c| !c.is_empty()) {
        if let Some(actual) = insight.and_then(|value| value.primary_color.as_deref()) {
            if colors.iter().any(|color| color == actual) {
                let label = visrecall_domain::insight::PrimaryColor::parse(actual)
                    .map(|color| color.label_zh().to_string())
                    .unwrap_or_else(|| actual.to_string());
                labels.push(label);
            }
        }
    }
    if let Some(chart) = &query.chart {
        if insight
            .and_then(|value| value.chart_type.as_deref())
            .is_some_and(|value| value == chart)
        {
            let label = visrecall_domain::insight::ChartType::parse(chart)
                .map(|value| value.label_zh().to_string())
                .unwrap_or_else(|| chart.clone());
            labels.push(label);
        }
    }
    if query.has_formula == Some(true) && insight.is_some_and(|value| value.has_formula) {
        labels.push("公式".to_string());
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
    fn color_filter_uses_in_list() {
        let query = ParsedQuery {
            color: Some(vec!["black".into(), "gray".into()]),
            ..base()
        };
        let filter = to_sql(&query).unwrap();
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
        let filter = to_sql(&query).unwrap();
        // 时间 2 + 应用 1 + 类型 1 + 颜色 1 + 图表 1 + 公式 1 + 标签 2 = 9
        assert_eq!(filter.params.len(), 9);
        assert_eq!(filter.placeholder_count(), filter.params.len());
        assert!(filter.clause.contains("i.captured_at >= ?1"));
        assert!(filter.clause.contains("i.captured_at < ?2"));
        assert!(filter.clause.contains("i.kind = ?4"));
        assert!(filter.clause.contains("ins.has_formula = ?7"));
        assert!(filter.clause.contains("t.name = ?8"));
        assert!(filter.clause.contains("t.name = ?9"));
        assert!(filter.clause.contains("ORDER BY i.captured_at DESC"));
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
