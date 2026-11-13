//! 规则表驱动的查询解析（design §6.6）。
//!
//! 核心纪律：**一个词只消费一次**，但硬条件与软条件分层处理：
//! - **硬条件**（时间 / `app:X` / `#tag` / 引号）从 `remaining` 里「剥离」，剩下的原文才是
//!   `semantic_text`——否则「上个月」会既当时间过滤又当语义查询词；
//! - **软条件**（颜色 / 图表 / 素材类型 / 公式）**不剥离**：用 jieba 的 token 边界识别，
//!   原词继续参与 FTS 与查询嵌入，只在召回阶段多加一路「软命中」通道。这是对上面那条
//!   纪律的**显式豁免**，只适用于软命中层——`红楼` 若被读成「红」+「楼」，主色非红的
//!   素材就会被候选集整条排除（假阴性），比「少一个过滤」代价大得多。
//!
//! 应用名的自然语言形式（`来自X` / `在X里`）**不解析**：`来自` / `在` 是高频汉字组合
//! （存在 / 现在 / 正在），误判会把素材整条排除且无法靠词表规避；这类短语交给字面与语义通道。
//!
//! 纯函数、无 IO、无锁：随机输入不 panic（proptest 覆盖），`now` 可注入（测试可控）。

use chrono::{DateTime, Datelike, Duration, Local, Months, NaiveDate, TimeZone};
use visrecall_domain::insight::{ChartType, PrimaryColor};
use visrecall_domain::item::ItemKind;
use visrecall_domain::now_millis;
use visrecall_domain::search::{ParsedQuery, TimeRange};

use crate::tokenizer::{segments, terms_for_query};

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

    // 顺序即优先级：引号 → 标签 → 应用 → 时间（以上均为硬条件，逐段剥离）
    let mut quoted_raw = Vec::new();
    while let Some(fragment) = take_quoted(&mut remaining) {
        if !fragment.trim().is_empty() {
            quoted_raw.push(fragment);
        }
    }
    let tags = take_tags(&mut remaining);
    let app = take_app(&mut remaining);
    let time = take_time(&mut remaining, now_ms);

    // 软命中：只识别不剥离，原词继续参与 FTS 与查询嵌入。
    // 四个 take_* 共用同一份切分结果（软词不改变 remaining，区间与原文一致）。
    let segments = segments(&remaining);
    let mut facet_words: Vec<String> = Vec::new();
    let color = take_color(&segments, &remaining, &mut facet_words);
    let chart = take_chart(&segments, &remaining, &mut facet_words);
    let content_kind = take_kind(&segments, &remaining, &mut facet_words);
    let has_formula = take_formula(&segments, &remaining, &mut facet_words);

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
        facet_words,
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

/// 一个分词片段：`(词, 起始字符下标, 结束字符下标)`。
type Segment = (String, usize, usize);

/// 软命中的一次命中：`(枚举值, 命中的原词)`。
type FacetHit<T> = (T, String);

/// 单个 token 是否命中词表（长词优先）。
///
/// 单字颜色词的启用条件（D3-Q02，偏保守）：孤词、后邻是标点/空白/句末，或以「色」
/// 结尾的复合词（`酒红色`）。理由是中文单字颜色词歧义极高（红楼 / 红旗 / 红烧肉 /
/// 红利 / 红线），误判代价是**彻底丢素材**，漏判代价只是少一个过滤——两者不对称。
fn token_match<T: Clone>(
    token: &str,
    text: &str,
    end: usize,
    table: &[(&str, T)],
    isolated: bool,
) -> Option<T> {
    let mut sorted: Vec<&(&str, T)> = table.iter().collect();
    sorted.sort_by_key(|(word, _)| std::cmp::Reverse(word.chars().count()));

    // ① 整 token 命中：多字词只要 token 相等即可；单字词必须有边界或处于孤词场景
    for (word, value) in &sorted {
        if !token_matches(token, word) {
            continue;
        }
        let single = word.chars().count() == 1;
        if !single || isolated || is_boundary_after(text, end) {
            return Some(value.clone());
        }
    }

    // ② 单字词作为复合词的词头（`酒红色` → 红）：以该字开头、以「色」结尾且长度 > 1
    for (word, value) in &sorted {
        if word.chars().count() != 1 {
            continue;
        }
        let Some(head) = word.chars().next() else {
            continue;
        };
        let count = token.chars().count();
        if count > 1 && token.starts_with(head) && token.ends_with('色') {
            return Some(value.clone());
        }
    }

    None
}

/// `token == entry`（拉丁字母按大小写不敏感比较，中文等价于相等）。
fn token_matches(token: &str, entry: &str) -> bool {
    token.len() == entry.len() && token.eq_ignore_ascii_case(entry)
}

/// `end` 之后是标点、空白还是句末（用于单字词的后邻判定）。
fn is_boundary_after(text: &str, end: usize) -> bool {
    match text.chars().nth(end) {
        None => true,
        Some(ch) => ch.is_whitespace() || is_punct(ch),
    }
}

/// 把命中的原词追加进 `facet_words`（去重，保持首次出现顺序）。
fn push_facet_word(facet_words: &mut Vec<String>, word: &str) {
    if !facet_words.iter().any(|existing| existing == word) {
        facet_words.push(word.to_string());
    }
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

/// `app:X` 显式语法（**唯一**产生 `app` 的规则，仍走硬过滤）。
///
/// `来自X` / `在X里` 不再解析：`来自` / `在` 是高频汉字组合（存在 / 现在 / 正在 /
/// 来自哪里），子串匹配必然误判，且这类短语本来就该由字面与语义通道处理。
fn take_app(remaining: &mut String) -> Option<String> {
    let lower = remaining.to_ascii_lowercase();
    let pos = lower.find("app:")?;
    let rest: String = remaining[pos + 4..].to_string();
    let value: String = rest
        .chars()
        .take_while(|c| !c.is_whitespace() && !is_punct(*c))
        .collect();
    if value.is_empty() {
        return None;
    }
    let consumed = 4 + value.len();
    remaining.replace_range(pos..pos + consumed, " ");
    Some(value.to_ascii_lowercase())
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

/// 主色词（软命中，不剥离）。同一次查询里可以命中多个颜色词（`红色和蓝色`）。
fn take_color(
    segments: &[Segment],
    text: &str,
    facet_words: &mut Vec<String>,
) -> Option<Vec<String>> {
    let mut colors: Vec<String> = Vec::new();
    for (words, word) in match_facet(segments, text, COLOR_WORDS) {
        for color in words {
            let value = color.as_str().to_string();
            if !colors.iter().any(|existing| existing == &value) {
                colors.push(value);
            }
        }
        push_facet_word(facet_words, &word);
    }
    if colors.is_empty() {
        None
    } else {
        Some(colors)
    }
}

/// 图表类型词（软命中，不剥离；多条命中取第一处）。
fn take_chart(segments: &[Segment], text: &str, facet_words: &mut Vec<String>) -> Option<String> {
    match_facet(segments, text, CHART_WORDS)
        .into_iter()
        .next()
        .map(|(chart, word)| {
            push_facet_word(facet_words, &word);
            chart.as_str().to_string()
        })
}

/// 素材类型词（软命中，不剥离；多条命中取第一处）。
fn take_kind(segments: &[Segment], text: &str, facet_words: &mut Vec<String>) -> Option<ItemKind> {
    match_facet(segments, text, KIND_WORDS)
        .into_iter()
        .next()
        .map(|(kind, word)| {
            push_facet_word(facet_words, &word);
            kind
        })
}

/// 公式词（软命中，不剥离）。
fn take_formula(segments: &[Segment], text: &str, facet_words: &mut Vec<String>) -> Option<bool> {
    let table: Vec<(&str, bool)> = FORMULA_WORDS.iter().map(|word| (*word, true)).collect();
    let hits = match_facet(segments, text, &table);
    if hits.is_empty() {
        return None;
    }
    for (_, word) in hits {
        push_facet_word(facet_words, &word);
    }
    Some(true)
}

/// [`match_facet`] 的「复用切分结果」版本：解析器一次切分、四个词表共用。
fn match_facet<T: Clone>(
    segments: &[Segment],
    text: &str,
    table: &[(&str, T)],
) -> Vec<FacetHit<T>> {
    let isolated = segments
        .iter()
        .filter(|(word, _, _)| word.chars().any(char::is_alphanumeric))
        .count()
        <= 1;

    let mut out: Vec<FacetHit<T>> = Vec::new();
    for (token, _start, end) in segments {
        if !token.chars().any(char::is_alphanumeric) {
            continue;
        }
        let Some(value) = token_match(token, text, *end, table, isolated) else {
            continue;
        };
        if !out.iter().any(|(_, word)| word == token) {
            out.push((value, token.clone()));
        }
    }
    out
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

        // 「漏斗图」是软命中：识别成图表条件，但**不从语义文本里剥离**
        let query = parse("季度营收漏斗图");
        assert_eq!(query.semantic_text, "季度营收漏斗图");
        assert_eq!(query.chart.as_deref(), Some("funnel"));
        assert_eq!(query.facet_words, vec!["漏斗图".to_string()]);
        assert!(!query.has_hard_filters(), "软条件不进候选集 WHERE");
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

        // 词边界：`红楼` / `红旗` 是单 token，切不出「红」
        for raw in ["红楼", "红旗", "红烧肉的配方"] {
            let query = parse(raw);
            assert!(query.color.is_none(), "{raw} 不该被当成颜色条件");
            assert!(
                query.facet_words.is_empty(),
                "{raw} 不该产生软命中词：{:?}",
                query.facet_words
            );
            assert_eq!(query.semantic_text, raw, "整句必须留在语义文本里");
        }

        // 软词不剥离：正文写「红色预警」但主色不是红的素材仍要能被字面/语义召回
        let query = parse("红色预警");
        assert_eq!(query.color, Some(vec!["red".into()]));
        assert_eq!(query.semantic_text, "红色预警");
        assert!(query.lexical_terms.contains(&"红色".to_string()));

        // 单字颜色词只在孤词 / 后邻边界 / 「X色」复合词时生效
        assert_eq!(parse("红").color, Some(vec!["red".into()]));
        assert_eq!(parse("红 楼").color, Some(vec!["red".into()]));
        assert_eq!(parse("酒红色").color, Some(vec!["red".into()]));
        // 后邻不是边界：「他买了红玫瑰」里的「红」不消费
        assert!(parse("买了红玫瑰").color.is_none());
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
    fn app_prefix_is_hard() {
        let query = parse("app:chrome 报错");
        assert_eq!(query.app.as_deref(), Some("chrome"));
        assert_eq!(query.semantic_text, "报错");
        assert!(query.has_hard_filters());
        assert!(query
            .hard_chips()
            .iter()
            .any(|chip| chip.contains("chrome")));
    }

    #[test]
    fn app_phrases_are_not_parsed() {
        // `来自X` / `在X里` 不再产生任何结构化字段：整句按普通文本进字面与语义通道
        for raw in [
            "来自Chrome的报错",
            "在微信里的聊天记录",
            "存在文件里",
            "来自Chrome",
        ] {
            let query = parse(raw);
            assert!(query.app.is_none(), "{raw} 不该解析出应用名");
            assert!(query.facet_words.is_empty(), "{raw} 不该产生软命中词");
            assert_eq!(query.semantic_text, raw, "{raw} 必须整句保留");
            assert!(!query.has_filters(), "{raw} 不该产生 WHERE 条件");
        }
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
        let query = parse_query_at("上个月 app:chrome 的橙色漏斗图 \"季度营收\"", now);
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
        // 硬条件（时间 / app:X / 引号）被剥离
        assert!(!query.semantic_text.contains("上个月"));
        assert!(!query.semantic_text.contains("chrome"));
        assert!(!query.semantic_text.contains("季度营收"));
        // 软条件（颜色 / 图表）**不剥离**：仍参与 FTS 与查询嵌入
        assert!(query.semantic_text.contains("橙色"));
        assert!(query.semantic_text.contains("漏斗图"));
        assert_eq!(
            query.facet_words,
            vec!["橙色".to_string(), "漏斗图".to_string()]
        );
    }

    #[test]
    fn empty_and_garbage_inputs_are_safe() {
        for raw in [
            "", "   ", "#", "app:", "来自", "在", "\"", "「」", "最近", "tag:",
        ] {
            let query = parse(raw);
            assert!(query.tags.is_empty() || !query.tags.iter().any(|t| t.is_empty()));
            // 不变量：软命中词一定来自原文，不会凭空出现
            for word in &query.facet_words {
                assert!(raw.contains(word.as_str()), "{raw} → {word}");
            }
        }
    }

    #[test]
    fn facet_words_do_not_erase_semantics() {
        // 颜色条件只记一次（同义重复不会产生两种颜色），且原词留在语义文本里
        let query = parse("橙色");
        assert_eq!(query.color, Some(vec!["orange".to_string()]));
        assert_eq!(query.semantic_text, "橙色");
        assert_eq!(query.facet_words, vec!["橙色".to_string()]);
        // 孤词判定：整条查询只剩软命中词 → 软条件退回硬过滤
        assert!(query.facets_only());

        // 带其它语义词时不是孤词（软条件只加分）
        let query = parse("橙色 发票");
        assert!(!query.facets_only());
        assert_eq!(
            query.lexical_terms,
            vec!["橙色".to_string(), "发票".to_string()]
        );
    }
}
