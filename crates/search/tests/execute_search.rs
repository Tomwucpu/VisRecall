//! 三路召回 + RRF + 解释装配（Task 12 Step 4）。
//!
//! 用「概念桶」假嵌入（`MapEmbed`）制造可确定复现的语义命中：
//! `报错/崩溃` → 第 0 维、`发票/报销` → 第 1 维、`会议/纪要` → 第 2 维、`营收/漏斗` → 第 3 维。
//! 这样「报错」能在**没有任何字面命中**时召回标题为「客户端崩溃日志」的素材。

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use visrecall_domain::insight::{ChartType, Insight, PrimaryColor, SummarySource};
use visrecall_domain::providers::{EmbeddingProvider, VectorIndex};
use visrecall_domain::vector_math::quantize_i8;
use visrecall_domain::{new_item_id, AppError, ItemFilters, ItemKind, ItemStatus, NewItem};
use visrecall_library::{BruteForceInt8, FtsColumns, Library};
use visrecall_search::build_search_document;
use visrecall_search::document::DocumentFields;
use visrecall_search::execute::{search, SearchDeps, SearchOutcome, SearchRequest};
use visrecall_search::rank::RrfConfig;
use visrecall_search::tokenizer::{terms_for_query, tokenize_for_fts};

const DIM: usize = 4;
const MODEL_ID: &str = "map-embed";

/// 概念桶假嵌入：按关键词计数成向量，L2 归一化。
struct MapEmbed {
    ready: AtomicBool,
}

impl MapEmbed {
    fn new() -> Self {
        Self {
            ready: AtomicBool::new(true),
        }
    }

    fn vector(text: &str) -> Vec<f32> {
        let buckets: [(&[&str], usize); 4] = [
            (&["报错", "崩溃", "crash", "churn"], 0),
            (&["发票", "报销", "金额"], 1),
            (&["会议", "纪要", "周报"], 2),
            (&["漏斗", "营收", "图表"], 3),
        ];
        let mut vector = vec![0.0f32; DIM];
        for (words, dim) in buckets {
            let hits = words.iter().filter(|word| text.contains(*word)).count();
            #[allow(clippy::cast_precision_loss)]
            {
                vector[dim] += hits as f32;
            }
        }
        if vector.iter().all(|value| *value == 0.0) {
            vector[0] = 1.0;
        }
        let norm = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
        for value in &mut vector {
            *value /= norm;
        }
        vector
    }
}

#[async_trait]
impl EmbeddingProvider for MapEmbed {
    fn id(&self) -> &str {
        MODEL_ID
    }

    fn dim(&self) -> usize {
        DIM
    }

    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, AppError> {
        Ok(texts.iter().map(|text| Self::vector(text)).collect())
    }

    fn ready(&self) -> bool {
        self.ready.load(Ordering::SeqCst)
    }
}

/// 造一条素材的全部输入（参数太多，收成一个结构体便于读）。
struct SeedItem<'a> {
    key: &'static str,
    kind: ItemKind,
    title: &'a str,
    body: &'a str,
    color: Option<PrimaryColor>,
    chart: Option<ChartType>,
    app: &'a str,
}

struct Fixture {
    _dir: tempfile::TempDir,
    library: Arc<Library>,
    vectors: BruteForceInt8,
    embed: MapEmbed,
    runtime: tokio::runtime::Runtime,
    ids: BTreeMap<&'static str, String>,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let library = Arc::new(Library::open(dir.path().to_path_buf()).unwrap());
        library.init_embedding_space(MODEL_ID, DIM as u32).unwrap();
        let vectors = BruteForceInt8::new(Arc::clone(&library));

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        let mut fixture = Self {
            _dir: dir,
            library,
            vectors,
            embed: MapEmbed::new(),
            runtime,
            ids: BTreeMap::new(),
        };
        fixture.seed();
        fixture
    }

    /// R2 验收库（A–F）：软命中、孤词降级与 `来自X` 的回归场景。
    fn facet_case() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let library = Arc::new(Library::open(dir.path().to_path_buf()).unwrap());
        library.init_embedding_space(MODEL_ID, DIM as u32).unwrap();
        let vectors = BruteForceInt8::new(Arc::clone(&library));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let mut fixture = Self {
            _dir: dir,
            library,
            vectors,
            embed: MapEmbed::new(),
            runtime,
            ids: BTreeMap::new(),
        };
        fixture.seed_facet_case();
        fixture
    }

    /// A–F：主色与正文故意错位（正文写「红色预警」但主色为蓝），以及 chrome/vscode 两条同类报错。
    fn seed_facet_case(&mut self) {
        self.add(SeedItem {
            key: "warning_blue",
            kind: ItemKind::Image,
            title: "监控预警面板",
            body: "红色预警：温度超限",
            color: Some(PrimaryColor::Blue),
            chart: None,
            app: "chrome",
        });
        self.add(SeedItem {
            key: "report_red",
            kind: ItemKind::Image,
            title: "季度报表",
            body: "季度报表 汇总",
            color: Some(PrimaryColor::Red),
            chart: None,
            app: "chrome",
        });
        self.add(SeedItem {
            key: "unrelated_blue",
            kind: ItemKind::Text,
            title: "随手笔记",
            body: "今天天气不错",
            color: Some(PrimaryColor::Blue),
            chart: None,
            app: "chrome",
        });
        self.add(SeedItem {
            key: "lou_blue",
            kind: ItemKind::Image,
            title: "红楼人物关系",
            body: "贾宝玉 林黛玉",
            color: Some(PrimaryColor::Blue),
            chart: None,
            app: "chrome",
        });
        self.add(SeedItem {
            key: "err_chrome",
            kind: ItemKind::Image,
            title: "浏览器报错日志",
            body: "报错日志 502",
            color: Some(PrimaryColor::Gray),
            chart: None,
            app: "chrome",
        });
        self.add(SeedItem {
            key: "err_vscode",
            kind: ItemKind::Image,
            title: "编辑器报错日志",
            body: "报错日志 断点",
            color: Some(PrimaryColor::Gray),
            chart: None,
            app: "vscode",
        });
        // G：正文写「表格」，但洞察里没有图表类型 —— 孤词 `表格` 硬过滤 0 命中后应放宽重跑
        self.add(SeedItem {
            key: "table_text",
            kind: ItemKind::Text,
            title: "导出表格说明",
            body: "如何把表格导出成 CSV",
            color: Some(PrimaryColor::Gray),
            chart: None,
            app: "chrome",
        });
    }

    fn seed(&mut self) {
        self.add_item(
            "invoice",
            ItemKind::Image,
            "发票报销明细",
            "发票号码 00329181 金额 1280.00",
            Some(PrimaryColor::Orange),
            Some(ChartType::Funnel),
        );
        // 中文「报错」在 OCR/标题里**不出现**，只能靠语义通道命中
        self.add_item(
            "crash",
            ItemKind::Image,
            "客户端崩溃日志",
            "the app crashed unexpectedly at startup",
            Some(PrimaryColor::Blue),
            None,
        );
        self.add_item(
            "meeting",
            ItemKind::Text,
            "周会纪要",
            "本周周报与会议纪要要点",
            Some(PrimaryColor::Gray),
            None,
        );
    }

    fn add_item(
        &mut self,
        key: &'static str,
        kind: ItemKind,
        title: &str,
        body: &str,
        color: Option<PrimaryColor>,
        chart: Option<ChartType>,
    ) {
        self.add(SeedItem {
            key,
            kind,
            title,
            body,
            color,
            chart,
            app: "chrome",
        });
    }

    fn add(&mut self, seed: SeedItem<'_>) {
        let SeedItem {
            key,
            kind,
            title,
            body,
            color,
            chart,
            app,
        } = seed;
        let id = new_item_id();
        let original = if kind == ItemKind::Image {
            format!("originals/images/{id}.png")
        } else {
            format!("texts/{id}.txt")
        };
        let full = self.library.root().join(&original);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(&full, body.as_bytes()).unwrap();

        self.library
            .insert_item_with_id(
                id.clone(),
                NewItem {
                    kind,
                    captured_at: 1_700_000_000_000,
                    source_app: Some(app.into()),
                    source_title: Some(title.into()),
                    source_url: None,
                    width: None,
                    height: None,
                    char_count: None,
                    original_path: original,
                    content_hash: key.into(),
                },
            )
            .unwrap();

        let mut insight = Insight::empty(&id, 0);
        insight.summary = Some(format!("{title} 摘要"));
        insight.summary_source = SummarySource::Vlm;
        insight.primary_color = color.map(|c| c.as_str().to_string());
        insight.chart_type = chart.map(|c| c.as_str().to_string());
        if kind == ItemKind::Image {
            insight.ocr_text = Some(body.to_string());
            insight.ocr_engine = Some("fake-ocr".into());
            insight.ocr_confidence = Some(0.9);
            insight.ocr_boxes = vec![visrecall_domain::insight::OcrBox::new(
                body, 0.1, 0.1, 0.4, 0.05,
            )];
        }
        self.library.insight_upsert(&insight).unwrap();

        let rowid = self.library.item_rowid(&id).unwrap().unwrap();
        let summary = insight.summary.clone().unwrap_or_default();
        self.library
            .fts_upsert(
                rowid,
                &FtsColumns {
                    title: &tokenize_for_fts(title),
                    summary: &tokenize_for_fts(&summary),
                    ocr_text: &tokenize_for_fts(if kind == ItemKind::Image { body } else { "" }),
                    note: "",
                    tags: "",
                    entities: "",
                    original_text: &tokenize_for_fts(if kind == ItemKind::Text {
                        body
                    } else {
                        ""
                    }),
                },
            )
            .unwrap();

        let document = build_search_document(&DocumentFields {
            title,
            summary: &summary,
            ocr_or_original: body,
            ..Default::default()
        });
        let vector = MapEmbed::vector(&document);
        self.vectors.add(rowid, &quantize_i8(&vector)).unwrap();

        self.ids.insert(key, id);
    }

    fn run(&self, raw: &str, limit: u32) -> SearchOutcome {
        search(
            SearchDeps {
                library: &self.library,
                vectors: &self.vectors,
                embed: Some(&self.embed),
                runtime: Some(self.runtime.handle()),
                rrf: RrfConfig::default(),
            },
            SearchRequest::new(raw, limit),
        )
        .unwrap()
    }

    fn run_filtered(&self, raw: &str, filters: ItemFilters) -> SearchOutcome {
        search(
            SearchDeps {
                library: &self.library,
                vectors: &self.vectors,
                embed: Some(&self.embed),
                runtime: Some(self.runtime.handle()),
                rrf: RrfConfig::default(),
            },
            SearchRequest::new(raw, 10).with_filters(filters),
        )
        .unwrap()
    }

    /// 语义未就绪（P3-R09 降级）：不注入嵌入也不注入运行时。
    fn run_without_embed(&self, raw: &str) -> SearchOutcome {
        search(
            SearchDeps {
                library: &self.library,
                vectors: &self.vectors,
                embed: None,
                runtime: None,
                rrf: RrfConfig::default(),
            },
            SearchRequest::new(raw, 10),
        )
        .unwrap()
    }

    fn keys(&self, outcome: &SearchOutcome) -> Vec<&'static str> {
        outcome
            .hits
            .iter()
            .filter_map(|hit| {
                self.ids
                    .iter()
                    .find(|(_, id)| **id == hit.item.id)
                    .map(|(key, _)| *key)
            })
            .collect()
    }
}

#[test]
fn pure_literal_query_uses_fts_only_and_explains_terms() {
    let fixture = Fixture::new();
    let outcome = fixture.run("发票", 10);

    assert_eq!(fixture.keys(&outcome), vec!["invoice"]);
    let hit = &outcome.hits[0];
    assert!(!hit.explanation.semantic, "字面命中不应标成语义命中");
    assert!(hit
        .explanation
        .terms
        .iter()
        .any(|term| term.contains("发票")));
    assert!(!hit.explanation.highlight_boxes.is_empty(), "OCR 框应命中");
    assert!(outcome.semantic_used, "有语义文本且模型就绪 → 双通道");
}

#[test]
fn pure_semantic_query_hits_without_any_literal_match() {
    let fixture = Fixture::new();
    let outcome = fixture.run("报错", 10);

    // 「报错」在任何素材的字面文本里都不存在 → 只有向量通道能召回崩溃日志
    let expr = visrecall_search::build_match_expr(&terms_for_query("报错"), &[]);
    let literal = fixture.library.fts_search(&expr, None, 10).unwrap();
    assert!(literal.is_empty(), "前提：字面通道为空");

    assert_eq!(fixture.keys(&outcome), vec!["crash"]);
    let hit = &outcome.hits[0];
    assert!(
        hit.explanation.semantic,
        "无字面命中且无语义过滤 → semantic=true"
    );
    assert!(outcome.semantic_used);
}

#[test]
fn mixed_query_combines_soft_facet_and_literal() {
    let fixture = Fixture::new();
    // 橙色（软命中）+ 发票（字面）
    let outcome = fixture.run("橙色 发票", 10);
    assert_eq!(fixture.keys(&outcome), vec!["invoice"]);
    let hit = &outcome.hits[0];
    assert!(
        hit.explanation
            .facets
            .iter()
            .any(|label| label.contains("橙")),
        "解释里要出现命中的软条件：{:?}",
        hit.explanation.facets
    );
    assert!(
        hit.explanation.filters.is_empty(),
        "软条件不该写进硬过滤行：{:?}",
        hit.explanation.filters
    );
    assert!(hit
        .explanation
        .terms
        .iter()
        .any(|term| term.contains("发票")));
}

#[test]
fn filters_only_query_returns_candidates_with_zero_score() {
    let fixture = Fixture::new();
    // 孤词：软条件退回硬过滤，行为与改造前一致
    let outcome = fixture.run("橙色", 10);
    assert_eq!(fixture.keys(&outcome), vec!["invoice"]);
    assert_eq!(outcome.hits[0].score, 0.0, "纯过滤命中分数为 0");
    // 软词**不剥离**：`橙色` 仍在语义文本里，因此语义通道照常参与
    assert!(outcome.semantic_used, "软词仍参与查询嵌入（D3-Q03）");
    assert!(!outcome.hits[0].explanation.facets.is_empty());
}

#[test]
fn semantic_channel_degrades_when_embedder_not_ready() {
    let fixture = Fixture::new();
    fixture.embed.ready.store(false, Ordering::SeqCst);
    let outcome = fixture.run("报错 发票", 10);
    assert!(!outcome.semantic_used, "模型未就绪必须降级");
    // 字面可得的那条仍在
    assert!(fixture.keys(&outcome).contains(&"invoice"));
    assert!(!fixture.keys(&outcome).contains(&"crash"));
}

#[test]
fn tie_order_is_stable_across_calls() {
    let fixture = Fixture::new();
    let first = fixture.run("橙色", 10);
    let second = fixture.run("橙色", 10);
    assert_eq!(
        fixture.keys(&first),
        fixture.keys(&second),
        "同分结果顺序必须可回放"
    );

    // 全过滤候选（多条同时 score=0）也稳定
    let all = fixture.run("截图", 10);
    let all_again = fixture.run("截图", 10);
    assert_eq!(fixture.keys(&all), fixture.keys(&all_again));
}

#[test]
fn query_log_is_written_and_feedback_roundtrips() {
    let fixture = Fixture::new();
    let outcome = fixture.run("发票", 10);

    let log = fixture.library.query_log_get(&outcome.query_id).unwrap();
    let log = log.expect("必须写入 queries 行");
    assert_eq!(log.raw, "发票");
    assert!(log.parsed_json.is_some());
    assert!(log.feedback.is_none());
    assert!(log
        .result_ids_json
        .unwrap()
        .contains(&outcome.hits[0].item.id));

    fixture
        .library
        .query_log_set_feedback(&outcome.query_id, "not-this")
        .unwrap();
    assert_eq!(
        fixture
            .library
            .query_log_get(&outcome.query_id)
            .unwrap()
            .unwrap()
            .feedback
            .as_deref(),
        Some("not-this")
    );

    assert!(matches!(
        fixture
            .library
            .query_log_set_feedback("missing", "not-this"),
        Err(AppError::NotFound)
    ));
}

#[test]
fn limit_is_clamped_and_total_reports_untruncated_count() {
    let fixture = Fixture::new();
    let outcome = fixture.run("截图", 1);
    assert_eq!(outcome.hits.len(), 1);
    assert!(outcome.total >= outcome.hits.len());

    let clamped = fixture.run("截图", 10_000);
    assert!(clamped.hits.len() <= visrecall_search::execute::MAX_LIMIT as usize);
}

#[test]
fn empty_query_returns_recent_items_without_embedding() {
    let fixture = Fixture::new();
    let outcome = fixture.run("", 10);
    assert!(!outcome.semantic_used);
    assert!(outcome.hits.is_empty(), "空查询无过滤器 → 无候选");
    // 空查询仍写 queries 行（评测口径）
    assert!(fixture
        .library
        .query_log_get(&outcome.query_id)
        .unwrap()
        .is_some());
}

#[test]
fn ui_filters_intersect_literal_and_semantic_channels() {
    let fixture = Fixture::new();
    // 「发票」字面命中 invoice（图片），限定类型为文本后该命中被筛掉。
    let only_text = ItemFilters {
        kinds: vec![ItemKind::Text],
        ..ItemFilters::default()
    };
    let filtered = fixture.run_filtered("发票", only_text);
    assert!(
        !fixture.keys(&filtered).contains(&"invoice"),
        "类型筛选后不应再返回图片素材：{:?}",
        fixture.keys(&filtered)
    );

    // 「报错」只能靠语义命中 crash（图片）；限定类型为文本后语义命中也失效。
    let semantic_text = ItemFilters {
        kinds: vec![ItemKind::Text],
        ..ItemFilters::default()
    };
    let semantic_filtered = fixture.run_filtered("报错", semantic_text);
    assert!(!fixture.keys(&semantic_filtered).contains(&"crash"));
    if let Some(hit) = semantic_filtered.hits.first() {
        assert!(!hit.explanation.semantic, "有筛选时不算纯语义命中");
    }

    // 不限制类型时，语义通道仍能召回。
    assert_eq!(fixture.run("报错", 10).hits.len(), 1);
}

#[test]
fn ui_filters_only_query_returns_candidates_without_embedding() {
    let fixture = Fixture::new();
    let filters = ItemFilters {
        kinds: vec![ItemKind::Text],
        tags: vec![],
        statuses: vec![ItemStatus::Parsing],
        time_from: None,
        time_to: None,
    };
    let outcome = fixture.run_filtered("", filters);
    assert!(!outcome.semantic_used, "纯筛选查询不应调用嵌入");
    assert_eq!(fixture.keys(&outcome), vec!["meeting"]);
    let hit = &outcome.hits[0];
    assert_eq!(hit.score, 0.0);
    assert!(
        hit.explanation.filters.iter().any(|label| label == "文本"),
        "解释里要出现命中的类型条件：{:?}",
        hit.explanation.filters
    );
    assert!(
        hit.explanation
            .filters
            .iter()
            .any(|label| label == "解析中"),
        "解释里要出现命中的状态条件：{:?}",
        hit.explanation.filters
    );
}

#[test]
fn explanation_boxes_only_match_terms() {
    let fixture = Fixture::new();
    let outcome = fixture.run("金额", 10);
    let hit = &outcome.hits[0];
    assert_eq!(hit.item.id, fixture.ids["invoice"]);
    assert!(!hit.explanation.highlight_boxes.is_empty());
    for obox in &hit.explanation.highlight_boxes {
        assert!(
            hit.explanation.terms.iter().any(|term| obox.contains(term)),
            "框必须由命中词触发"
        );
    }
}

// --- 迭代 R2：软命中 / 孤词降级 / 应用名不解析（§6 的 G5–G12） ---

/// G5：正文写「红色预警」但主色为蓝的素材不能被主色排除。
#[test]
fn soft_color_facet_does_not_exclude_a_blue_item_whose_text_matches() {
    let fixture = Fixture::facet_case();
    let outcome = fixture.run("红色预警", 10);
    let keys = fixture.keys(&outcome);
    let top3 = &keys[..keys.len().min(3)];
    assert!(
        top3.contains(&"warning_blue"),
        "主色不符但正文命中的素材必须进 Top-3：{keys:?}"
    );
    // 软条件只加分不排除：结果集不该被「红色」筛成只有红图
    assert!(keys.len() > 1, "候选集不应收缩成纯红素材：{keys:?}");
    let hit = outcome
        .hits
        .iter()
        .find(|hit| hit.item.id == fixture.ids["warning_blue"])
        .expect("warning_blue 应在结果里");
    assert!(
        !hit.explanation
            .facets
            .iter()
            .any(|label| label.contains("主色")),
        "它并不命中主色条件：{:?}",
        hit.explanation.facets
    );
}

/// G6：孤词「红色」仍是硬过滤（列出红色素材）。
#[test]
fn solo_color_word_falls_back_to_a_hard_filter() {
    let fixture = Fixture::facet_case();
    let outcome = fixture.run("红色", 10);
    assert_eq!(fixture.keys(&outcome), vec!["report_red"]);
    assert!(
        outcome.hits[0]
            .explanation
            .facets
            .iter()
            .any(|label| label.contains("主色")),
        "孤词路径也要能解释命中：{:?}",
        outcome.hits[0].explanation.facets
    );
}

/// G6 另一面 + D3-Q07：孤词硬过滤 0 命中时清空软条件重跑，并标注「条件已放宽」。
#[test]
fn solo_facet_with_no_hit_relaxes_and_reruns() {
    let fixture = Fixture::facet_case();
    // 「表格」是图表词，但库里没有 chart_type=table 的素材；正文里有「表格」
    let outcome = fixture.run("表格", 10);
    let keys = fixture.keys(&outcome);
    assert_eq!(
        keys.first().copied(),
        Some("table_text"),
        "放宽后必须靠字面通道找回：{keys:?}"
    );
    let hit = outcome
        .hits
        .iter()
        .find(|hit| hit.item.id == fixture.ids["table_text"])
        .expect("table_text 应在结果里");
    assert!(
        hit.explanation
            .facets
            .iter()
            .any(|label| label == "条件已放宽"),
        "放宽重跑必须标注：{:?}",
        hit.explanation.facets
    );
    // 放宽后软条件被清空（chips 不再显示「参考」条件）
    assert!(outcome.parsed.facet_words.is_empty());
    assert!(outcome.parsed.chart.is_none());
}

/// G7：语义未就绪（`embed: None`）时软命中与孤词降级仍成立。
#[test]
fn soft_facets_work_without_the_semantic_channel() {
    let fixture = Fixture::facet_case();
    let outcome = fixture.run_without_embed("红色预警");
    assert!(!outcome.semantic_used, "未注入嵌入 → 语义通道跳过");
    assert!(
        fixture.keys(&outcome).contains(&"warning_blue"),
        "字面通道仍应召回：{:?}",
        fixture.keys(&outcome)
    );

    let solo = fixture.run_without_embed("红色");
    assert_eq!(fixture.keys(&solo), vec!["report_red"]);
}

/// G11：`来自Chrome的报错` 不再按应用过滤，chrome 与 vscode 两条都被召回。
#[test]
fn app_phrase_is_plain_text_and_does_not_exclude_other_apps() {
    let fixture = Fixture::facet_case();
    let outcome = fixture.run("来自Chrome的报错", 10);
    let keys = fixture.keys(&outcome);
    assert!(keys.contains(&"err_chrome"), "{keys:?}");
    assert!(keys.contains(&"err_vscode"), "{keys:?}");
    assert!(
        outcome.parsed.app.is_none(),
        "自然语言形式不该产生 app 条件"
    );
}

/// G12：`app:X` 仍是硬过滤（vscode 那条排除）。
#[test]
fn explicit_app_prefix_is_still_a_hard_filter() {
    let fixture = Fixture::facet_case();
    let outcome = fixture.run("app:chrome 报错", 10);
    let keys = fixture.keys(&outcome);
    assert_eq!(keys.first().copied(), Some("err_chrome"), "{keys:?}");
    assert!(!keys.contains(&"err_vscode"), "{keys:?}");
    assert_eq!(outcome.parsed.app.as_deref(), Some("chrome"));
}

/// `存在文件里` 不再误判成 `app=文件`：候选集是全集，能正常召回。
#[test]
fn existential_phrase_is_not_an_app_filter() {
    let fixture = Fixture::facet_case();
    let outcome = fixture.run("存在文件里", 10);
    assert!(outcome.parsed.app.is_none());
    assert!(!outcome.hits.is_empty(), "不该因为误判而 0 结果");
}
