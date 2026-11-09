//! 冒烟：空库不崩 + 3 条素材小库跑通全流程（Task 15 Step 4）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use visrecall_domain::insight::{Insight, PrimaryColor, SummarySource};
use visrecall_domain::providers::{EmbeddingProvider, VectorIndex};
use visrecall_domain::vector_math::quantize_i8;
use visrecall_domain::{new_item_id, AppError, ItemKind, NewItem};
use visrecall_eval::render_report;
use visrecall_eval::summarize_by_bucket;
use visrecall_eval::{evaluate, load_queries, summarize, EvalQuery, ReportContext};
use visrecall_library::{BruteForceInt8, FtsColumns, Library};
use visrecall_search::build_search_document;
use visrecall_search::document::DocumentFields;
use visrecall_search::tokenizer::tokenize_for_fts;

const DIM: usize = 4;
const MODEL_ID: &str = "eval-fake-embed";

/// 概念桶假嵌入（与 `search` 的集成测试同一口径）。
struct FakeEmbed {
    ready: AtomicBool,
}

impl FakeEmbed {
    fn new() -> Self {
        Self {
            ready: AtomicBool::new(true),
        }
    }

    fn vector(text: &str) -> Vec<f32> {
        let buckets: [(&[&str], usize); 4] = [
            (&["报错", "崩溃", "失败"], 0),
            (&["发票", "报销"], 1),
            (&["会议", "纪要"], 2),
            (&["漏斗", "营收"], 3),
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
impl EmbeddingProvider for FakeEmbed {
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

struct Harness {
    _dir: tempfile::TempDir,
    library: Arc<Library>,
    vectors: BruteForceInt8,
    embed: FakeEmbed,
    runtime: tokio::runtime::Runtime,
}

impl Harness {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let library = Arc::new(Library::open(dir.path().to_path_buf()).unwrap());
        library.init_embedding_space(MODEL_ID, DIM as u32).unwrap();
        let vectors = BruteForceInt8::new(Arc::clone(&library));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        Self {
            _dir: dir,
            library,
            vectors,
            embed: FakeEmbed::new(),
            runtime,
        }
    }

    fn add(&mut self, title: &str, body: &str, color: PrimaryColor) -> String {
        let id = new_item_id();
        let rel = format!("originals/images/{id}.png");
        let full = self.library.root().join(&rel);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(&full, b"x").unwrap();
        self.library
            .insert_item_with_id(
                id.clone(),
                NewItem {
                    kind: ItemKind::Image,
                    captured_at: 1_700_000_000_000,
                    source_app: Some("chrome".into()),
                    source_title: Some(title.into()),
                    source_url: None,
                    width: None,
                    height: None,
                    char_count: None,
                    original_path: rel,
                    content_hash: format!("h-{id}"),
                },
            )
            .unwrap();

        let mut insight = Insight::empty(&id, 0);
        insight.summary = Some(format!("{title} 摘要"));
        insight.summary_source = SummarySource::Vlm;
        insight.primary_color = Some(color.as_str().to_string());
        insight.ocr_text = Some(body.to_string());
        self.library.insight_upsert(&insight).unwrap();

        let rowid = self.library.item_rowid(&id).unwrap().unwrap();
        self.library
            .fts_upsert(
                rowid,
                &FtsColumns {
                    title: &tokenize_for_fts(title),
                    summary: &tokenize_for_fts(insight.summary.as_deref().unwrap_or("")),
                    ocr_text: &tokenize_for_fts(body),
                    ..Default::default()
                },
            )
            .unwrap();
        let document = build_search_document(&DocumentFields {
            title,
            summary: insight.summary.as_deref().unwrap_or(""),
            ocr_or_original: body,
            ..Default::default()
        });
        self.vectors
            .add(rowid, &quantize_i8(&FakeEmbed::vector(&document)))
            .unwrap();
        id
    }
}

#[test]
fn empty_library_does_not_break_metrics() {
    let harness = Harness::new();
    let queries = vec![EvalQuery {
        query: "发票".into(),
        relevant: vec![],
        bucket: "literal".into(),
    }];
    let outcomes = evaluate(
        &harness.library,
        &harness.vectors,
        &harness.embed,
        harness.runtime.handle(),
        &queries,
        10,
    )
    .unwrap();
    assert_eq!(outcomes.len(), 1);
    assert!(outcomes[0].top.is_empty());
    let metrics = summarize(&outcomes);
    assert_eq!(metrics.queries, 1);
    // 空库 + 无相关标注 + 无结果 → 视为「正确的不召回」
    assert!((metrics.recall_at_10 - 1.0).abs() < 1e-9);
    // 报告仍可渲染
    let report = render_report(
        &ReportContext {
            database: "mem".into(),
            item_count: 0,
            model_id: MODEL_ID.into(),
            model_dim: DIM,
            rrf: "k=60".into(),
            queries_file: "inline".into(),
        },
        &metrics,
        &summarize_by_bucket(&outcomes),
        &outcomes,
        "空库冒烟",
    );
    assert!(report.contains("空库冒烟"));
}

#[test]
fn three_item_library_runs_end_to_end() {
    let mut harness = Harness::new();
    let invoice = harness.add("发票报销明细", "发票号码 00329181", PrimaryColor::Orange);
    let crash = harness.add("客户端崩溃日志", "the app crashed", PrimaryColor::Blue);
    let meeting = harness.add("周会纪要", "本周周报要点", PrimaryColor::Gray);

    let queries = vec![
        EvalQuery {
            query: "发票".into(),
            relevant: vec![invoice.clone()],
            bucket: "literal".into(),
        },
        EvalQuery {
            query: "橙色 发票".into(),
            relevant: vec![invoice.clone()],
            bucket: "screenshot".into(),
        },
        EvalQuery {
            query: "会议纪要".into(),
            relevant: vec![meeting.clone()],
            bucket: "scene".into(),
        },
        EvalQuery {
            // 「营收」在库里没有任何字面或语义对应（概念桶 3 全为 0）→ 应该空召回
            query: "营收".into(),
            relevant: vec![crash.clone()],
            bucket: "scene".into(),
        },
    ];

    let outcomes = evaluate(
        &harness.library,
        &harness.vectors,
        &harness.embed,
        harness.runtime.handle(),
        &queries,
        10,
    )
    .unwrap();
    assert_eq!(outcomes.len(), 4);
    // 前三条应命中标注的相关素材
    assert!(outcomes[0].top3_hit);
    assert_eq!(outcomes[0].top[0], invoice);
    assert!(outcomes[1].top3_hit);
    assert!(outcomes[2].top3_hit);
    assert_eq!(outcomes[2].top[0], meeting);

    let metrics = summarize(&outcomes);
    assert_eq!(metrics.queries, 4);
    // 4 条里 3 条 Top-3 命中 → 75%，未达 80% 门禁
    assert!((metrics.top3_hit_rate - 0.75).abs() < 1e-9);
    assert!(!metrics.passes_gate());
    assert!(metrics.p95_ms < 5_000);
    assert!(metrics.channel_counts.contains_key("fts"));
}

#[test]
fn template_file_parses_and_comments_are_skipped() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/eval/queries.template.jsonl");
    let queries = load_queries(&path).expect("模板必须可解析");
    assert!(!queries.is_empty());
    assert!(queries.iter().all(|query| !query.query.trim().is_empty()));
}
