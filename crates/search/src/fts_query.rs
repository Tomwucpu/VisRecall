//! `MATCH` 表达式构造与转义（design §6.3）。
//!
//! 策略：**每个词都包成 FTS5 字符串字面量**（`"词"`），内部的双引号按 FTS5 规则写成
//! `""`。这样 `* ( ) : ^ - "` 等语法字符全部退化为普通字符（被 unicode61 当作分隔符），
//! 不需要维护一份「危险字符黑名单」，也不会因为用户输入 `a OR b` 而意外改变布尔语义。
//!
//! 组合规则：
//! - `terms`（未加引号的查询词）→ `OR` 连接：任一词命中即算字面命中；
//! - `quoted`（双引号内的词）→ `AND` 连接：引号内所有词都必须命中（design §1.2 的
//!   「必命中词组」口径，不做 trigram 语序保证）；
//! - 两组同时存在 → `(A OR B) AND "c" AND "d"`。
//!
//! 返回空串表示 FTS 通道应跳过。

/// 把单个词包成 FTS5 字符串字面量；词为空或不含字母数字时返回 `None`。
pub fn escape_phrase(word: &str) -> Option<String> {
    let trimmed = word.trim();
    if trimmed.is_empty() || !trimmed.chars().any(|c| c.is_alphanumeric()) {
        return None;
    }
    // FTS5 字符串内部的双引号用两个双引号表示；控制字符会让查询解析器困惑，直接剔除。
    let escaped: String = trimmed
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .replace('"', "\"\"");
    Some(format!("\"{escaped}\""))
}

/// 构造 `MATCH` 表达式；两部分都为空时返回空串。
pub fn build_match_expr(terms: &[String], quoted: &[String]) -> String {
    let or_part: Vec<String> = terms.iter().filter_map(|t| escape_phrase(t)).collect();
    let and_part: Vec<String> = quoted.iter().filter_map(|t| escape_phrase(t)).collect();

    let mut expr = String::new();
    if !or_part.is_empty() {
        if or_part.len() == 1 {
            expr.push_str(&or_part[0]);
        } else {
            expr.push('(');
            expr.push_str(&or_part.join(" OR "));
            expr.push(')');
        }
    }
    if !and_part.is_empty() {
        if !expr.is_empty() {
            expr.push_str(" AND ");
        }
        expr.push_str(&and_part.join(" AND "));
    }
    expr
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn empty_input_produces_empty_expr() {
        assert!(build_match_expr(&[], &[]).is_empty());
        assert!(build_match_expr(&v(&["", "  "]), &[]).is_empty());
        assert!(build_match_expr(&v(&["，。"]), &[]).is_empty());
    }

    #[test]
    fn single_term_is_not_wrapped_in_parens() {
        assert_eq!(build_match_expr(&v(&["发票"]), &[]), "\"发票\"");
    }

    #[test]
    fn multiple_terms_are_ored() {
        assert_eq!(
            build_match_expr(&v(&["发票", "报销"]), &[]),
            "(\"发票\" OR \"报销\")"
        );
    }

    #[test]
    fn quoted_terms_are_anded() {
        assert_eq!(
            build_match_expr(&[], &v(&["季度", "营收"])),
            "\"季度\" AND \"营收\""
        );
    }

    #[test]
    fn mixed_terms_combine_with_and() {
        assert_eq!(
            build_match_expr(&v(&["发票", "报销"]), &v(&["季度"])),
            "(\"发票\" OR \"报销\") AND \"季度\""
        );
    }

    #[test]
    fn syntax_characters_are_neutralized() {
        let expr = build_match_expr(&v(&["a*", "b(c)", "d:e", "f-g", "h\"i"]), &[]);
        // 全部被包进字面量，语法字符不再是运算符
        assert!(expr.starts_with('('));
        assert!(expr.contains("\"a*\""));
        assert!(expr.contains("\"b(c)\""));
        assert!(expr.contains("\"d:e\""));
        assert!(expr.contains("\"f-g\""));
        assert!(
            expr.contains("\"h\"\"i\""),
            "内部引号应转义成两个引号：{expr}"
        );
    }
}
