//! `ParsedQuery` → 候选集 SQL（design §6.3 / Task 11 Step 2）。
//!
//! 产出 `SELECT i.rowid FROM items i LEFT JOIN insights ins …  WHERE …` 与参数表；
//! **无任何过滤器时 clause 为空**（调用方据此跳过查询，直接视为全集）。
//!
//! 参数全部用编号占位符（`?1`…），`SqlFilter::clause` 的占位符个数必须等于
//! `params.len()`——单测强制断言，避免拼接漂移。

use rusqlite::types::Value;
use visrecall_domain::search::ParsedQuery;
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
    if !query.has_filters() {
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

    let where_clause = clauses.join(" AND ");
    Ok(SqlFilter {
        clause: format!(
            "SELECT i.rowid FROM items i LEFT JOIN insights ins ON ins.item_id = i.id \
             WHERE {where_clause} ORDER BY i.captured_at DESC, i.rowid DESC"
        ),
        params,
    })
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
