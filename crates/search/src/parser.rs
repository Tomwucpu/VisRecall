//! 规则表驱动的查询解析（design §6.6）。
//!
//! 核心纪律：**一个词只消费一次**。每条规则从 `remaining` 里「剥离」已识别的片段，
//! 剩下的原文才是 `semantic_text`——否则「上个月」会既当时间过滤又当语义查询词，
//! 「橙色」会既过滤又参与 BM25。
//!
//! 纯函数、无 IO、无锁：随机输入不 panic（proptest 覆盖），`now` 可注入（测试可控）。

use chrono::{DateTime, Datelike, Duration, Local, Months, NaiveDate, TimeZone};
use visrecall_domain::insight::{ChartType, PrimaryColor};
use visrecall_domain::item::ItemKind;
use visrecall_domain::now_millis;
use visrecall_domain::search::{ParsedQuery, TimeRange};

use crate::tokenizer::terms_for_query;

/// 「最近 N 天」的 N 上限（超出不当作时间表达式）。
pub const MAX_RECENT_DAYS: u32 = 365;

/// 时间词（最长匹配优先；顺序即优先级）。
const TIME_WORDS: &[&str] = &[
    "上个月",
    "上个季度",
    "这个月",
    "本月",
    "本季度",
    "上周",
    "本周",
    "这周",
    "今年",
    "去年",
    "今天",
    "昨天",
    "前天",
    "上一年",
    "最近一周",
    "最近一个月",
];

/// 颜色同义词表：`(词, 枚举集)`。多字词在前，避免「红色」被「红」抢先消费。
const COLOR_WORDS: &[(&str, &[PrimaryColor])] = &[
    ("深色", &[PrimaryColor::Black, PrimaryColor::Gray]),
    ("暗色", &[PrimaryColor::Black]),
    ("浅色", &[PrimaryColor::White, PrimaryColor::Gray]),
    ("彩色", &[PrimaryColor::Multicolor]),
    ("多彩", &[PrimaryColor::Multicolor]),
    ("多色", &[PrimaryColor::Multicolor]),
    ("红色", &[PrimaryColor::Red]),
    ("橙色", &[PrimaryColor::Orange]),
    ("橘色", &[PrimaryColor::Orange]),
    ("黄色", &[PrimaryColor::Yellow]),
    ("绿色", &[PrimaryColor::Green]),
    ("蓝色", &[PrimaryColor::Blue]),
    ("紫色", &[PrimaryColor::Purple]),
    ("粉色", &[PrimaryColor::Pink]),
    ("粉红", &[PrimaryColor::Pink]),
    ("棕色", &[PrimaryColor::Brown]),
    ("褐色", &[PrimaryColor::Brown]),
    ("黑色", &[PrimaryColor::Black]),
    ("白色", &[PrimaryColor::White]),
    ("灰色", &[PrimaryColor::Gray]),
    ("银灰", &[PrimaryColor::Gray]),
    ("红", &[PrimaryColor::Red]),
    ("橙", &[PrimaryColor::Orange]),
    ("橘", &[PrimaryColor::Orange]),
    ("黄", &[PrimaryColor::Yellow]),
    ("绿", &[PrimaryColor::Green]),
    ("蓝", &[PrimaryColor::Blue]),
    ("紫", &[PrimaryColor::Purple]),
    ("粉", &[PrimaryColor::Pink]),
    ("棕", &[PrimaryColor::Brown]),
    ("褐", &[PrimaryColor::Brown]),
    ("黑", &[PrimaryColor::Black]),
    ("白", &[PrimaryColor::White]),
    ("灰", &[PrimaryColor::Gray]),
];

/// 图表类型词表（长词在前）。
const CHART_WORDS: &[(&str, ChartType)] = &[
    ("柱状图", ChartType::Bar),
    ("条形图", ChartType::Bar),
    ("折线图", ChartType::Line),
    ("曲线图", ChartType::Line),
    ("趋势图", ChartType::Line),
    ("饼状图", ChartType::Pie),
    ("圆饼图", ChartType::Pie),
    ("饼图", ChartType::Pie),
    ("漏斗图", ChartType::Funnel),
    ("表格", ChartType::Table),
    ("数据表", ChartType::Table),
];

/// 素材类型词表（长词在前）。
const KIND_WORDS: &[(&str, ItemKind)] = &[
    ("截图", ItemKind::Image),
    ("图片", ItemKind::Image),
    ("照片", ItemKind::Image),
    ("图像", ItemKind::Image),
    ("文本", ItemKind::Text),
    ("文字", ItemKind::Text),
    ("笔记", ItemKind::Text),
    ("链接", ItemKind::Link),
    ("网址", ItemKind::Link),
];

/// 公式词表。
const FORMULA_WORDS: &[&str] = &["公式", "latex", "LaTeX", "LATEX"];

/// 引号对（开, 闭）。
const QUOTE_PAIRS: &[(char, char)] = &[
    ('"', '"'),
    ('\'', '\''),
    ('\u{300c}', '\u{300d}'),
    ('\u{300e}', '\u{300f}'),
    ('\u{201c}', '\u{201d}'),
    ('\u{2018}', '\u{2019}'),
];

/// 用「当前时间」解析查询。
pub fn parse_query(raw: &str) -> ParsedQuery {
    parse_query_at(raw, now_millis())
}

/// 用指定时间解析查询（测试注入）。
pub fn parse_query_at(raw: &str, now_ms: i64) -> ParsedQuery {
    let mut remaining = raw.to_string();

    // 顺序即优先级：引号 → 标签 → 应用 → 时间 → 颜色 → 图表 → 类型 → 公式
    let mut quoted_raw = Vec::new();
    while let Some(fragment) = take_quoted(&mut remaining) {
        if !fragment.trim().is_empty() {
            quoted_raw.push(fragment);
        }
    }
    let tags = take_tags(&mut remaining);
    let app = take_app(&mut remaining);
    let time = take_time(&mut remaining, now_ms);
    let color = take_color(&mut remaining);
    let chart = take_chart(&mut remaining);
    let content_kind = take_kind(&mut remaining);
    let has_formula = take_formula(&mut remaining);

    // 引号内容再分词（design §4.3「引号内分词，AND 语义」）：
    // 一次 jieba 切分后每个词都是必命中项，交给 MATCH 构造器用 AND 连接。
    let mut quoted_terms: Vec<String> = Vec::new();
    for fragment in &quoted_raw {
        for term in terms_for_query(fragment) {
            if !quoted_terms.iter().any(|existing| existing == &term) {
                quoted_terms.push(term);
            }
        }
    }

    let semantic_text = normalize_whitespace(&remaining);
    let lexical_terms = terms_for_query(&semantic_text);

    ParsedQuery {
        time,
        app,
        color,
        chart,
        content_kind,
        has_formula,
        tags,
        semantic_text,
        lexical_terms,
        quoted_terms,
    }
}

/// 收起连续空白，去掉首尾空白。
fn normalize_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 从 `remaining` 中剥离第一次出现的 `needle`（返回是否命中）。
fn strip_once(remaining: &mut String, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    match remaining.find(needle) {
        Some(pos) => {
            remaining.replace_range(pos..pos + needle.len(), " ");
            true
        }
        None => false,
    }
}

/// 按「长词优先」剥离第一个命中的表达式，返回其右值。
fn strip_first_of<T: Clone>(remaining: &mut String, table: &[(&str, T)]) -> Option<T> {
    let mut sorted: Vec<&(&str, T)> = table.iter().collect();
    sorted.sort_by_key(|(word, _)| std::cmp::Reverse(word.chars().count()));
    for (word, value) in sorted {
        if strip_once(remaining, word) {
            return Some(value.clone());
        }
    }
    None
}

/// 提取成对引号内的文本（`"…"` / `「…」` / `"…"` …），并剥离。
fn take_quoted(remaining: &mut String) -> Option<String> {
    for (open, close) in QUOTE_PAIRS {
        let chars: Vec<char> = remaining.chars().collect();
        let Some(start) = chars.iter().position(|c| c == open) else {
            continue;
        };
        let Some(rel_end) = chars[start + 1..].iter().position(|c| c == close) else {
            continue;
        };
        let end = start + 1 + rel_end;
        let inner: String = chars[start + 1..end].iter().collect();
        // 按字符下标换算回字节下标
        let byte_start: usize = chars[..start].iter().map(|c| c.len_utf8()).sum();
        let consumed_len: usize = chars[start..=end].iter().map(|c| c.len_utf8()).sum();
        remaining.replace_range(byte_start..byte_start + consumed_len, " ");
        return Some(inner);
    }
    None
}

/// `tag:X` 与 `#X`（精确名）。
fn take_tags(remaining: &mut String) -> Vec<String> {
    let mut tags = Vec::new();
    // #X：井号后连续非空白
    loop {
        let Some(pos) = remaining.find('#') else {
            break;
        };
        let rest: String = remaining[pos + 1..].to_string();
        let value: String = rest
            .chars()
            .take_while(|c| !c.is_whitespace() && !is_punct(*c))
            .collect();
        if value.is_empty() {
            // 孤立的 # 不再尝试，避免死循环
            break;
        }
        let consumed = 1 + value.len();
        remaining.replace_range(pos..pos + consumed, " ");
        if !tags.iter().any(|tag| tag == &value) {
            tags.push(value);
        }
    }
    // tag:X
    loop {
        let lower = remaining.to_ascii_lowercase();
        let Some(pos) = lower.find("tag:") else {
            break;
        };
        let rest: String = remaining[pos + 4..].to_string();
        let value: String = rest
            .chars()
            .take_while(|c| !c.is_whitespace() && !is_punct(*c))
            .collect();
        if value.is_empty() {
            break;
        }
        let consumed = 4 + value.len();
        remaining.replace_range(pos..pos + consumed, " ");
        if !tags.iter().any(|tag| tag == &value) {
            tags.push(value);
        }
    }
    tags
}

/// `app:X`、`来自X`、`在X里/中`。
fn take_app(remaining: &mut String) -> Option<String> {
    // app:X
    let lower = remaining.to_ascii_lowercase();
    if let Some(pos) = lower.find("app:") {
        let rest: String = remaining[pos + 4..].to_string();
        let value: String = rest
            .chars()
            .take_while(|c| !c.is_whitespace() && !is_punct(*c))
            .collect();
        if !value.is_empty() {
            let consumed = 4 + value.len();
            remaining.replace_range(pos..pos + consumed, " ");
            return Some(value.to_ascii_lowercase());
        }
    }

    // 来自X / 在X里 / 在X中
    for marker in ["来自", "在"] {
        let Some(pos) = remaining.find(marker) else {
            continue;
        };
        let after_marker = &remaining[pos + marker.len()..];
        // 允许「来自 chrome」这种带空格的写法；跳过的空白也要算进消费长度
        let trimmed = after_marker.trim_start();
        let skipped = after_marker.len() - trimmed.len();
        let rest: String = trimmed.to_string();

        let mut value = String::new();
        for ch in rest.chars() {
            if is_value_terminator(ch) {
                break;
            }
            value.push(ch);
        }
        if value.is_empty() {
            continue;
        }
        let consumed_value = marker.len() + skipped + value.len();

        if marker == "在" {
            // 「在X里/中」必须真的带方位词，否则「在」只是普通虚词（「我在整理文档」）
            let after: String = rest.chars().skip(value.chars().count()).collect();
            let Some(suffix) = after.chars().next().filter(|ch| is_locative_suffix(*ch)) else {
                continue;
            };
            let consumed = consumed_value + suffix.len_utf8();
            remaining.replace_range(pos..pos + consumed, " ");
            return Some(value.to_ascii_lowercase());
        }

        remaining.replace_range(pos..pos + consumed_value, " ");
        return Some(value.to_ascii_lowercase());
    }
    None
}

fn is_locative_suffix(ch: char) -> bool {
    matches!(ch, '里' | '中')
}

/// 应用名取值的终止字符：空白、标点、方位词与常见助词。
///
/// 中文没有词间空格，只靠「连续非空白」会得到 `chrome的报错` 这种结果；这里补一层
/// 助词/方位词终止（design §6.6 的「X 为连续非空白词」在中文语境下的必要细化）。
fn is_value_terminator(ch: char) -> bool {
    ch.is_whitespace()
        || is_punct(ch)
        || matches!(
            ch,
            '里' | '中'
                | '的'
                | '上'
                | '下'
                | '内'
                | '外'
                | '和'
                | '与'
                | '是'
                | '有'
                | '我'
                | '了'
                | '吗'
                | '呢'
                | '吧'
        )
}

/// 时间词 → `TimeRange`（本地时区，周一起始）。
fn take_time(remaining: &mut String, now_ms: i64) -> Option<TimeRange> {
    // 「最近 N 天」先处理（含数字，不能被固定词表吃掉）
    if let Some(range) = take_recent_days(remaining, now_ms) {
        return Some(range);
    }
    let dt = local_from_millis(now_ms)?;
    for word in TIME_WORDS {
        if !strip_once(remaining, word) {
            continue;
        }
        return match *word {
            "今天" => Some(day_range(dt, 0)),
            "昨天" => Some(day_range(dt, -1)),
            "前天" => Some(day_range(dt, -2)),
            "本周" | "这周" => Some(week_range(dt, 0)),
            "上周" => Some(week_range(dt, -1)),
            "这个月" | "本月" => Some(month_range(dt, 0)),
            "上个月" => Some(month_range(dt, -1)),
            "本季度" => Some(quarter_range(dt, 0)),
            "上个季度" => Some(quarter_range(dt, -1)),
            "今年" => Some(year_range(dt, 0)),
            "去年" | "上一年" => Some(year_range(dt, -1)),
            "最近一周" => Some(TimeRange::new(
                now_ms - 7 * 86_400_000,
                now_ms.saturating_add(1),
            )),
            "最近一个月" => Some(TimeRange::new(
                now_ms - 30 * 86_400_000,
                now_ms.saturating_add(1),
            )),
            _ => None,
        };
    }
    None
}

/// `最近 N 天` / `最近N日`。
fn take_recent_days(remaining: &mut String, now_ms: i64) -> Option<TimeRange> {
    let pos = remaining.find("最近")?;
    let rest: String = remaining[pos + "最近".len()..].to_string();
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return None;
    }
    let after_digits: String = rest.chars().skip(digits.chars().count()).collect();
    let suffix = after_digits
        .chars()
        .next()
        .filter(|ch| *ch == '天' || *ch == '日')?;
    let days: u32 = digits.parse().ok()?;
    if days == 0 || days > MAX_RECENT_DAYS {
        return None;
    }
    // 后缀是 3 字节汉字，必须按 UTF-8 长度推进（否则会切到字符中间 panic）
    let consumed = "最近".len() + digits.len() + suffix.len_utf8();
    remaining.replace_range(pos..pos + consumed, " ");
    Some(TimeRange::new(
        now_ms - i64::from(days) * 86_400_000,
        now_ms.saturating_add(1),
    ))
}

fn take_color(remaining: &mut String) -> Option<Vec<String>> {
    let mut colors: Vec<String> = Vec::new();
    // 同一次查询里可以同时点多个颜色词（「红色和蓝色」）
    while let Some(hit) = strip_first_of(remaining, COLOR_WORDS) {
        for color in hit {
            let value = color.as_str().to_string();
            if !colors.iter().any(|existing| existing == &value) {
                colors.push(value);
            }
        }
    }
    if colors.is_empty() {
        None
    } else {
        Some(colors)
    }
}

fn take_chart(remaining: &mut String) -> Option<String> {
    strip_first_of(remaining, CHART_WORDS).map(|chart| chart.as_str().to_string())
}

fn take_kind(remaining: &mut String) -> Option<ItemKind> {
    strip_first_of(remaining, KIND_WORDS)
}

fn take_formula(remaining: &mut String) -> Option<bool> {
    let mut found = false;
    for word in FORMULA_WORDS {
        if strip_once(remaining, word) {
            found = true;
        }
    }
    found.then_some(true)
}

fn is_punct(ch: char) -> bool {
    matches!(
        ch,
        '，' | '。'
            | '、'
            | '；'
            | '：'
            | '？'
            | '！'
            | '（'
            | '）'
            | '《'
            | '》'
            | '【'
            | '】'
            | ','
            | '.'
            | ';'
            | ':'
            | '?'
            | '!'
            | '('
            | ')'
            | '['
            | ']'
            | '/'
            | '\\'
    )
}

fn local_from_millis(ms: i64) -> Option<DateTime<Local>> {
    Local.timestamp_millis_opt(ms).single()
}

fn start_of_day(dt: DateTime<Local>) -> DateTime<Local> {
    let Some(midnight) = dt.date_naive().and_hms_opt(0, 0, 0) else {
        return dt;
    };
    // DST 切换日 00:00 可能不存在，退化到当天最早的合法时刻
    Local
        .from_local_datetime(&midnight)
        .earliest()
        .unwrap_or(dt)
}

fn day_range(dt: DateTime<Local>, offset_days: i64) -> TimeRange {
    let base = start_of_day(dt) + Duration::days(offset_days);
    TimeRange::new(
        base.timestamp_millis(),
        (base + Duration::days(1)).timestamp_millis(),
    )
}

/// 周一起始的本周/上周。
fn week_range(dt: DateTime<Local>, week_offset: i64) -> TimeRange {
    let monday_offset = i64::from(dt.weekday().num_days_from_monday());
    let base = start_of_day(dt) - Duration::days(monday_offset) + Duration::weeks(week_offset);
    TimeRange::new(
        base.timestamp_millis(),
        (base + Duration::weeks(1)).timestamp_millis(),
    )
}

/// 按月份平移（负数向前）。
fn shift_months(date: NaiveDate, months: i64) -> NaiveDate {
    if months >= 0 {
        date + Months::new(u32::try_from(months).unwrap_or(0))
    } else {
        date - Months::new(u32::try_from(-months).unwrap_or(0))
    }
}

fn month_range(dt: DateTime<Local>, month_offset: i64) -> TimeRange {
    let date = dt.date_naive();
    let first = NaiveDate::from_ymd_opt(date.year(), date.month(), 1).unwrap_or(date);
    let start = shift_months(first, month_offset);
    let end = shift_months(start, 1);
    TimeRange::new(local_midnight_millis(start), local_midnight_millis(end))
}

fn quarter_range(dt: DateTime<Local>, quarter_offset: i64) -> TimeRange {
    let date = dt.date_naive();
    let current_quarter_month = ((date.month() - 1) / 3) * 3 + 1;
    let first = NaiveDate::from_ymd_opt(date.year(), current_quarter_month, 1).unwrap_or(date);
    let start = shift_months(first, quarter_offset * 3);
    let end = shift_months(start, 3);
    TimeRange::new(local_midnight_millis(start), local_midnight_millis(end))
}

fn year_range(dt: DateTime<Local>, year_offset: i64) -> TimeRange {
    let year = dt.year() + i32::try_from(year_offset).unwrap_or(0);
    let start = NaiveDate::from_ymd_opt(year, 1, 1).unwrap_or(dt.date_naive());
    let end = NaiveDate::from_ymd_opt(year + 1, 1, 1).unwrap_or(start);
    TimeRange::new(local_midnight_millis(start), local_midnight_millis(end))
}

fn local_midnight_millis(date: NaiveDate) -> i64 {
    let Some(naive) = date.and_hms_opt(0, 0, 0) else {
        return 0;
    };
    match Local.from_local_datetime(&naive).earliest() {
        Some(dt) => dt.timestamp_millis(),
        // DST 跳跃日的 00:00 不存在：用当天 01:00 兜底（区间误差 1h 可接受）
        None => Local
            .from_local_datetime(&date.and_hms_opt(1, 0, 0).unwrap_or(naive))
            .earliest()
            .map(|dt| dt.timestamp_millis())
            .unwrap_or(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-11-09 12:00:00 本地时间（周一）。
    fn monday_noon() -> i64 {
        let naive = NaiveDate::from_ymd_opt(2026, 11, 9)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap();
        Local
            .from_local_datetime(&naive)
            .earliest()
            .unwrap()
            .timestamp_millis()
    }

    fn parse(raw: &str) -> ParsedQuery {
        parse_query_at(raw, monday_noon())
    }

    #[test]
    fn plain_text_goes_to_semantic_text() {
        let query = parse("季度营收分析");
        assert_eq!(query.semantic_text, "季度营收分析");
        assert!(!query.has_filters(), "无结构化词时不应有过滤器");
        assert!(!query.lexical_terms.is_empty());
        assert!(query.time.is_none());

        // 「漏斗图」按规则表被消费成图表过滤，语义文本只剩前面的词
        let query = parse("季度营收漏斗图");
        assert_eq!(query.semantic_text, "季度营收");
        assert_eq!(query.chart.as_deref(), Some("funnel"));
    }

    #[test]
    fn time_words_are_consumed_once() {
        let now = monday_noon();
        let query = parse_query_at("上个月 营收", now);
        let time = query.time.expect("应识别时间");
        // 上个月 = 2026-10-01 ~ 2026-11-01
        let expected_from = Local
            .from_local_datetime(
                &NaiveDate::from_ymd_opt(2026, 10, 1)
                    .unwrap()
                    .and_hms_opt(0, 0, 0)
                    .unwrap(),
            )
            .earliest()
            .unwrap()
            .timestamp_millis();
        assert_eq!(time.from, expected_from);
        assert_eq!(query.semantic_text, "营收", "时间词必须从语义文本里剥离");
        assert!(!query
            .lexical_terms
            .iter()
            .any(|term| term.contains("上个月")));
    }

    #[test]
    fn today_and_yesterday_are_half_open_days() {
        let now = monday_noon();
        let today = parse_query_at("今天", now).time.unwrap();
        let yesterday = parse_query_at("昨天", now).time.unwrap();
        assert_eq!(today.to - today.from, 86_400_000);
        assert_eq!(yesterday.to, today.from);
    }

    #[test]
    fn week_starts_on_monday() {
        let now = monday_noon();
        let this_week = parse_query_at("本周", now).time.unwrap();
        let last_week = parse_query_at("上周", now).time.unwrap();
        assert_eq!(this_week.to - this_week.from, 7 * 86_400_000);
        assert_eq!(last_week.to, this_week.from);
        // 周一的「本周」应正好从今天开始
        assert_eq!(
            this_week.from,
            parse_query_at("今天", now).time.unwrap().from
        );
    }

    #[test]
    fn recent_n_days_parses_and_rejects_out_of_range() {
        let now = monday_noon();
        let range = parse_query_at("最近7天 报表", now).time.unwrap();
        assert_eq!(range.to, now + 1);
        assert_eq!(range.from, now - 7 * 86_400_000);
        assert_eq!(parse_query_at("最近7天 报表", now).semantic_text, "报表");

        assert!(parse_query_at("最近0天", now).time.is_none());
        assert!(parse_query_at("最近400天", now).time.is_none());
        assert!(parse_query_at("最近天", now).time.is_none());
    }

    #[test]
    fn color_synonyms_map_to_enums() {
        assert_eq!(parse("橙色漏斗图").color, Some(vec!["orange".to_string()]));
        assert_eq!(
            parse("深色主题").color,
            Some(vec!["black".to_string(), "gray".to_string()])
        );
        assert_eq!(
            parse("红色和蓝色").color,
            Some(vec!["red".into(), "blue".into()])
        );
        // 反例：不含颜色词
        assert!(parse("漏斗图").color.is_none());
    }

    #[test]
    fn chart_and_kind_and_formula_are_recognized() {
        assert_eq!(parse("柱状图").chart.as_deref(), Some("bar"));
        assert_eq!(parse("漏斗图").chart.as_deref(), Some("funnel"));
        assert_eq!(parse("截图").content_kind, Some(ItemKind::Image));
        assert_eq!(parse("文本").content_kind, Some(ItemKind::Text));
        assert_eq!(parse("含公式的推导").has_formula, Some(true));
        assert_eq!(parse("latex 表达式").has_formula, Some(true));
        assert!(parse("公式推导").has_formula.is_some());
        assert!(parse("普通文本").has_formula.is_none());
    }

    #[test]
    fn app_patterns_are_recognized() {
        assert_eq!(parse("app:chrome 报错").app.as_deref(), Some("chrome"));
        assert_eq!(parse("来自Chrome的报错").app.as_deref(), Some("chrome"));
        // 「在X里」必须带方位词
        let query = parse("在微信里的聊天记录");
        assert_eq!(query.app.as_deref(), Some("微信"));
        // 「的」会留在语义文本里（分词阶段当停用词丢掉），但应用名不能残留
        assert!(
            !query.semantic_text.contains("微信"),
            "{:?}",
            query.semantic_text
        );
        // 反例：孤立的「在」不消费
        let query = parse("我在整理文档");
        assert!(query.app.is_none());
    }

    #[test]
    fn tags_and_quotes_are_extracted() {
        let query = parse("#工作 和 tag:财务 的报告");
        assert_eq!(query.tags, vec!["工作".to_string(), "财务".to_string()]);

        let query = parse(r#"查找 "季度营收" 的图"#);
        assert!(
            query.quoted_terms.contains(&"季度".to_string()),
            "{:?}",
            query.quoted_terms
        );
        assert!(query.quoted_terms.contains(&"营收".to_string()));
        assert!(!query.semantic_text.contains("季度营收"));
        assert!(query.semantic_text.contains("查找"));

        let query = parse("「发票号码」");
        assert!(
            query.quoted_terms.contains(&"发票".to_string()),
            "{:?}",
            query.quoted_terms
        );
    }

    #[test]
    fn mixed_query_is_fully_parsed() {
        let now = monday_noon();
        let query = parse_query_at("上个月来自 chrome 的橙色漏斗图 \"季度营收\"", now);
        assert!(query.time.is_some());
        assert_eq!(query.app.as_deref(), Some("chrome"));
        assert_eq!(query.color, Some(vec!["orange".to_string()]));
        assert_eq!(query.chart.as_deref(), Some("funnel"));
        assert!(
            query.quoted_terms.contains(&"季度".to_string()),
            "{:?}",
            query.quoted_terms
        );
        assert!(query.quoted_terms.contains(&"营收".to_string()));
        // 语义文本只剩没有结构含义的部分
        assert!(!query.semantic_text.contains("上个月"));
        assert!(!query.semantic_text.contains("chrome"));
        assert!(!query.semantic_text.contains("橙色"));
        assert!(!query.semantic_text.contains("漏斗图"));
    }

    #[test]
    fn empty_and_garbage_inputs_are_safe() {
        for raw in [
            "", "   ", "#", "app:", "来自", "在", "\"", "「」", "最近", "tag:",
        ] {
            let query = parse(raw);
            assert!(query.tags.is_empty() || !query.tags.iter().any(|t| t.is_empty()));
        }
    }

    #[test]
    fn a_word_is_consumed_only_once() {
        // 颜色词不会被重复消费成两种颜色
        let query = parse("橙色");
        assert_eq!(query.color, Some(vec!["orange".to_string()]));
        assert!(query.semantic_text.trim().is_empty());
    }
}
