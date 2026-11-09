//! 查询解析与过滤器（Task 11 验收）：
//! 规则表正/反例、混合查询全解析、proptest 不 panic 与「一个词只消费一次」、
//! `to_sql` 占位符一致性、以及**真实 SQLite** 上的候选集正确性。

use proptest::prelude::*;

use visrecall_domain::insight::{ChartType, PrimaryColor};
use visrecall_domain::item::ItemKind;
use visrecall_domain::search::ParsedQuery;
use visrecall_domain::{Insight, ItemKind as Kind, NewItem};
use visrecall_library::Library;
use visrecall_search::filters::to_sql;
use visrecall_search::parser::{parse_query, parse_query_at};

/// 固定时间点：2026-11-09 12:00（本地时区，周一）。
fn now() -> i64 {
    use chrono::{Local, NaiveDate, TimeZone};
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
    parse_query_at(raw, now())
}

// --- 规则表正/反例（requirements P3-R06 验收清单） ---

#[test]
fn every_chart_word_maps_to_its_enum() {
    let cases = [
        ("柱状图", ChartType::Bar),
        ("折线图", ChartType::Line),
        ("饼图", ChartType::Pie),
        ("漏斗图", ChartType::Funnel),
        ("表格", ChartType::Table),
    ];
    for (word, expected) in cases {
        assert_eq!(
            parse(word).chart.as_deref(),
            Some(expected.as_str()),
            "{word}"
        );
    }
    // 反例
    assert!(parse("随便什么").chart.is_none());
}

#[test]
fn color_words_map_to_enums_and_negative_cases_do_not() {
    for color in PrimaryColor::ALL {
        let word = color.label_zh();
        assert_eq!(
            parse(word).color,
            Some(vec![color.as_str().to_string()]),
            "{word}"
        );
    }
    assert!(parse("这个季度").color.is_none());
    assert!(parse("白板上的笔记").color.is_some()); // 「白」仍是颜色词（规则表口径）
}

#[test]
fn content_kind_and_formula_negatives() {
    assert!(parse("随便一张截图").content_kind == Some(Kind::Image));
    assert!(parse("一段文字").content_kind == Some(Kind::Text));
    assert!(parse("这个链接").content_kind == Some(Kind::Link));
    assert!(parse("会议纪要").content_kind.is_none());
    assert_eq!(parse("含公式").has_formula, Some(true));
    assert_eq!(parse("普通内容").has_formula, None);
}

#[test]
fn time_word_boundaries_are_stable() {
    let base = now();
    for word in [
        "今天",
        "昨天",
        "前天",
        "本周",
        "上周",
        "本月",
        "上个月",
        "今年",
        "去年",
    ] {
        let range = parse_query_at(word, base)
            .time
            .unwrap_or_else(|| panic!("{word} 应识别为时间"));
        assert!(range.from < range.to, "{word}");
        assert!(range.from <= base, "{word} 的起点不应晚于当前时间");
    }
    // 反例：句子里的「上个月」一样会被消费，但「月报」不会
    assert!(parse("月报").time.is_none());
}

// --- 混合查询 ---

#[test]
fn mixed_query_parses_every_rule_at_once() {
    let query = parse("上个月来自 chrome 的橙色漏斗图 \"季度营收\" #工作");
    assert!(query.time.is_some());
    assert_eq!(query.app.as_deref(), Some("chrome"));
    assert_eq!(query.color, Some(vec!["orange".to_string()]));
    assert_eq!(query.chart.as_deref(), Some("funnel"));
    assert_eq!(query.tags, vec!["工作".to_string()]);
    assert!(query.quoted_terms.contains(&"季度".to_string()));
    assert!(query.quoted_terms.contains(&"营收".to_string()));
    assert!(!query.semantic_text.contains("橙色"));
}

// --- proptest ---

fn rule_word_strategy() -> impl Strategy<Value = String> {
    let words = prop::sample::select(vec![
        "上个月",
        "本周",
        "今天",
        "最近7天",
        "橙色",
        "深色",
        "红",
        "漏斗图",
        "柱状图",
        "表格",
        "截图",
        "文本",
        "链接",
        "公式",
        "来自",
        "在",
        "里",
        "app:",
        "tag:",
        "#工作",
        "\"引号\"",
        "「括号」",
        "的",
        "营收",
        "发票",
        " ",
        "，",
        "🚀",
    ]);
    prop::collection::vec(words, 0..8).prop_map(|parts| parts.join(""))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn arbitrary_input_never_panics(raw in ".{0,80}") {
        let query = parse(&raw);
        // 不变量：解析结果自洽
        let _ = query.chips();
        let _ = query.has_filters();
        let _ = to_sql(&query).expect("to_sql 不应失败");
    }

    #[test]
    fn rule_heavy_input_never_panics(raw in rule_word_strategy()) {
        let query = parse(&raw);
        let filter = to_sql(&query).expect("to_sql 不应失败");
        prop_assert_eq!(filter.placeholder_count(), filter.params.len());
    }

    #[test]
    fn consumed_fragments_do_not_leak_into_semantic_text(raw in rule_word_strategy()) {
        let query = parse(&raw);
        for color in query.color.clone().unwrap_or_default() {
            // 语义文本里不该残留被当作过滤条件的颜色中文词
            let word = PrimaryColor::parse(&color).map(|c| c.label_zh()).unwrap_or("");
            prop_assert!(!query.semantic_text.contains(word));
        }
        // 标签与引号的**标记形式**必须整体剥离（同样的字可能合法地出现在别处）
        for tag in &query.tags {
            let hashed = format!("#{}", tag);
            let tagged = format!("tag:{}", tag);
            let leaked =
                query.semantic_text.contains(&hashed) || query.semantic_text.contains(&tagged);
            prop_assert!(!leaked);
        }
        if !query.quoted_terms.is_empty() {
            let ascii_quote = '"';
            let cjk_quote: char = '\u{300c}';
            let leaked =
                query.semantic_text.contains(ascii_quote) || query.semantic_text.contains(cjk_quote);
            prop_assert!(!leaked);
        }
    }

    /// 语义文本再解析只会变短（结构化词逐个被消费，不会「长回来」）。
    #[test]
    fn reparsing_is_monotone(raw in rule_word_strategy()) {
        let first = parse(&raw);
        let second = parse(&first.semantic_text);
        prop_assert!(
            second.semantic_text.chars().count() <= first.semantic_text.chars().count(),
            "first={:?} second={:?}",
            first.semantic_text,
            second.semantic_text
        );
    }

    #[test]
    fn placeholder_count_matches_params(raw in rule_word_strategy()) {
        let query = parse(&raw);
        let filter = to_sql(&query).expect("to_sql 不应失败");
        prop_assert_eq!(filter.placeholder_count(), filter.params.len());
        prop_assert_eq!(filter.is_empty(), !query.has_filters());
    }
}

// --- to_sql 在真实 SQLite 上的候选集正确性 ---

struct Fixture {
    _dir: tempfile::TempDir,
    library: Library,
    ids: Vec<String>,
}

/// `(应用, 捕获时间, 主色, 图表, 界面, 含公式, 类型)`
type Spec<'a> = (
    &'a str,
    i64,
    Option<&'a str>,
    Option<&'a str>,
    Option<&'a str>,
    bool,
    Kind,
);

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let library = Library::open(dir.path().to_path_buf()).unwrap();
    let now = now();
    let mut ids = Vec::new();

    let specs: [Spec<'_>; 3] = [
        (
            "chrome",
            now - 3 * 86_400_000,
            Some("orange"),
            Some("funnel"),
            Some("list"),
            false,
            Kind::Image,
        ),
        (
            "wechat",
            now - 40 * 86_400_000,
            Some("blue"),
            Some("line"),
            Some("chat"),
            false,
            Kind::Image,
        ),
        (
            "chrome",
            now - 2 * 86_400_000,
            Some("black"),
            None,
            Some("code"),
            true,
            Kind::Text,
        ),
    ];

    for (i, (app, captured_at, color, chart, ui, formula, kind)) in specs.into_iter().enumerate() {
        let id = visrecall_domain::new_item_id();
        let path = if kind == Kind::Image {
            format!("originals/images/{id}.png")
        } else {
            format!("texts/{id}.txt")
        };
        let full = library.root().join(&path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(&full, b"payload").unwrap();
        library
            .insert_item_with_id(
                id.clone(),
                NewItem {
                    kind,
                    captured_at,
                    source_app: Some(app.to_string()),
                    source_title: Some(format!("素材{i}")),
                    source_url: None,
                    width: None,
                    height: None,
                    char_count: None,
                    original_path: path,
                    content_hash: format!("h{i}"),
                },
            )
            .unwrap();
        let mut insight = Insight::empty(&id, 0);
        insight.primary_color = color.map(str::to_string);
        insight.chart_type = chart.map(str::to_string);
        insight.ui_type = ui.map(str::to_string);
        insight.has_formula = formula;
        library.insight_upsert(&insight).unwrap();
        if i == 0 {
            library.tag_add(&id, "工作").unwrap();
        }
        ids.push(id);
    }

    Fixture {
        _dir: dir,
        library,
        ids,
    }
}

fn candidates(fixture: &Fixture, raw: &str) -> Vec<String> {
    let query = parse(raw);
    let filter = to_sql(&query).unwrap();
    if filter.is_empty() {
        return fixture.ids.clone();
    }
    let rowids = fixture
        .library
        .candidate_rowids(&filter.clause, &filter.params)
        .unwrap();
    rowids
        .into_iter()
        .filter_map(|rowid| {
            fixture
                .ids
                .iter()
                .find(|id| fixture.library.item_rowid(id).unwrap() == Some(rowid))
                .cloned()
        })
        .collect()
}

#[test]
fn color_filter_selects_only_matching_items() {
    let fixture = fixture();
    let hits = candidates(&fixture, "橙色");
    assert_eq!(hits, vec![fixture.ids[0].clone()]);

    let hits = candidates(&fixture, "深色");
    assert_eq!(hits, vec![fixture.ids[2].clone()], "深色 → black/gray");
}

#[test]
fn app_filter_is_case_insensitive_like() {
    let fixture = fixture();
    let hits = candidates(&fixture, "app:CHROME");
    assert_eq!(hits.len(), 2);
    assert!(!hits.contains(&fixture.ids[1]));
}

#[test]
fn time_filter_uses_captured_at_window() {
    let fixture = fixture();
    // 最近 7 天：第 1、3 条（第 2 条是 40 天前）
    let hits = candidates(&fixture, "最近7天");
    assert_eq!(hits.len(), 2);
    assert!(!hits.contains(&fixture.ids[1]));
}

#[test]
fn tag_filter_requires_exact_name() {
    let fixture = fixture();
    let hits = candidates(&fixture, "#工作");
    assert_eq!(hits, vec![fixture.ids[0].clone()]);
    assert!(candidates(&fixture, "#不存在").is_empty());
}

#[test]
fn formula_and_kind_filters_compose() {
    let fixture = fixture();
    let hits = candidates(&fixture, "公式");
    assert_eq!(hits, vec![fixture.ids[2].clone()]);

    let hits = candidates(&fixture, "截图");
    assert_eq!(hits.len(), 2, "两条 image 素材");
}

#[test]
fn combined_filters_intersect() {
    let fixture = fixture();
    // chrome + 截图：第 1 条（第 3 条是 text）
    let hits = candidates(&fixture, "app:chrome 截图");
    assert_eq!(hits, vec![fixture.ids[0].clone()]);
}

#[test]
fn no_filter_returns_whole_set() {
    let fixture = fixture();
    let hits = candidates(&fixture, "随便看看");
    assert_eq!(hits.len(), 3);
}

#[test]
fn chart_filter_matches_enum_value() {
    let fixture = fixture();
    let hits = candidates(&fixture, "漏斗图");
    assert_eq!(hits, vec![fixture.ids[0].clone()]);
}

#[test]
fn item_kind_enum_covers_chip_labels() {
    for kind in ItemKind::ALL {
        assert!(!kind.label_zh().is_empty());
        assert_eq!(ItemKind::parse(kind.as_str()), Some(kind));
    }
}

#[test]
fn parse_query_uses_current_time_by_default() {
    let query = parse_query("今天");
    let range = query.time.expect("今天应识别");
    assert!(range.contains(visrecall_domain::now_millis()));
}
