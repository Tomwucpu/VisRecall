//! 两阶段编排（全部假 Provider）：ocr → index → item_indexed → vlm → index → item_enriched。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use visrecall_domain::insight::SummarySource;
use visrecall_domain::{AppError, ItemKind, ItemStatus, NewItem};
use visrecall_library::{FtsColumns, Job, JobState, Library};
use visrecall_pipeline::lifecycle::{retry_pending, run_startup_checks};
use visrecall_pipeline::testing::{FakeEmbed, FakeFail, FakeOcr, FakeText, FakeVision};
use visrecall_pipeline::{
    with_builtin_handlers, Pipeline, PipelineConfig, PipelineDeps, PipelineEvents, PipelineHandle,
    PRIORITY_NEW, STAGE_INDEX, STAGE_OCR, STAGE_REBUILD, STAGE_VLM,
};
use visrecall_search::tokenizer::tokenize_for_fts;

/// 记录事件顺序的假接收端。
#[derive(Default)]
struct Events {
    order: Mutex<Vec<String>>,
    statuses: Mutex<Vec<(u64, u64)>>,
    pending: Mutex<Vec<String>>,
}

impl PipelineEvents for Events {
    fn pipeline_status(&self, queued: u64, running: u64) {
        self.statuses.lock().unwrap().push((queued, running));
    }

    fn item_indexed(&self, item_id: &str) {
        self.order
            .lock()
            .unwrap()
            .push(format!("indexed:{item_id}"));
    }

    fn item_enriched(&self, item_id: &str) {
        self.order
            .lock()
            .unwrap()
            .push(format!("enriched:{item_id}"));
    }

    fn item_pending(&self, item_id: &str) {
        self.pending.lock().unwrap().push(item_id.to_string());
    }

    fn job_failed(&self, _job: &Job, _error: &AppError) {}
}

struct Harness {
    _dir: tempfile::TempDir,
    library: Arc<Library>,
    runtime: tokio::runtime::Runtime,
    ocr: Arc<FakeOcr>,
    embed: Arc<FakeEmbed>,
    vision: Arc<FakeVision>,
    text: Arc<FakeText>,
    events: Arc<Events>,
    reindex_calls: Arc<AtomicUsize>,
}

impl Harness {
    fn new(ocr: FakeOcr, vision: FakeVision) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let library = Arc::new(Library::open(dir.path().to_path_buf()).unwrap());
        library.init_embedding_space(FakeEmbed::ID, 8).unwrap();

        // 模拟 app-core 注册的元数据钩子：bump_revision + 入队 index（同事务）
        let reindex_calls = Arc::new(AtomicUsize::new(0));
        {
            let lib = Arc::clone(&library);
            let calls = Arc::clone(&reindex_calls);
            library.set_metadata_hook(Some(Arc::new(move |ids: &[String]| {
                for id in ids {
                    match lib.bump_revision_and_enqueue(id, STAGE_INDEX, PRIORITY_NEW) {
                        Ok(_) => {
                            calls.fetch_add(1, Ordering::SeqCst);
                        }
                        Err(err) => eprintln!("hook enqueue failed: {err}"),
                    }
                }
            })));
        }

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        Self {
            _dir: dir,
            library,
            runtime,
            ocr: Arc::new(ocr),
            embed: Arc::new(FakeEmbed::new()),
            vision: Arc::new(vision),
            text: Arc::new(FakeText::new("会议纪要要点", &["会议"])),
            events: Arc::new(Events::default()),
            reindex_calls,
        }
    }

    fn deps(&self) -> Arc<PipelineDeps> {
        Arc::new(PipelineDeps {
            ocr: self.ocr.clone(),
            embed: self.embed.clone(),
            vision: self.vision.clone(),
            text: self.text.clone(),
            runtime: self.runtime.handle().clone(),
        })
    }

    fn config(&self) -> PipelineConfig {
        PipelineConfig {
            workers: 1,
            poll_interval: Duration::from_millis(5),
            backoff_secs: vec![0, 0, 0],
            max_attempts: 3,
            status_debounce: Duration::from_millis(1),
        }
    }

    fn spawn(&self) -> PipelineHandle {
        with_builtin_handlers(Pipeline::new(
            Arc::clone(&self.library),
            self.deps(),
            self.events.clone() as Arc<dyn PipelineEvents>,
        ))
        .with_config(self.config())
        .spawn()
        .unwrap()
    }

    /// 落一个「图片」素材：文件内容对假 OCR 无意义，但 handler 会真的去读它。
    fn insert_image(&self, hash: &str, title: &str) -> String {
        let id = visrecall_domain::new_item_id();
        let rel = format!("originals/images/{id}.png");
        let path = self.library.root().join(&rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"fake-png-bytes").unwrap();
        self.library
            .insert_item_with_id(
                id.clone(),
                NewItem {
                    kind: ItemKind::Image,
                    captured_at: 1_700_000_000_000,
                    source_app: Some("chrome".into()),
                    source_title: Some(title.into()),
                    source_url: None,
                    width: Some(1920),
                    height: Some(1080),
                    char_count: None,
                    original_path: rel,
                    content_hash: hash.into(),
                },
            )
            .unwrap();
        id
    }

    fn insert_text(&self, hash: &str, body: &str) -> String {
        let id = visrecall_domain::new_item_id();
        let rel = format!("texts/{id}.txt");
        let path = self.library.root().join(&rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, body.as_bytes()).unwrap();
        self.library
            .insert_item_with_id(
                id.clone(),
                NewItem {
                    kind: ItemKind::Text,
                    captured_at: 1_700_000_000_001,
                    source_app: None,
                    source_title: Some("粘贴的会议纪要".into()),
                    source_url: None,
                    width: None,
                    height: None,
                    char_count: Some(body.chars().count() as u32),
                    original_path: rel,
                    content_hash: hash.into(),
                },
            )
            .unwrap();
        id
    }
}

fn wait_until(mut predicate: impl FnMut() -> bool, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if predicate() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    predicate()
}

fn job_state(harness: &Harness, item: &str, stage: &str) -> Option<JobState> {
    harness
        .library
        .jobs_for_stage(item, stage)
        .unwrap()
        .first()
        .map(|job: &Job| job.state)
}

fn fts_hits(harness: &Harness, query: &str) -> Vec<i64> {
    let expr = visrecall_search::build_match_expr(&visrecall_search::terms_for_query(query), &[]);
    harness
        .library
        .fts_search(&expr, None, 10)
        .unwrap()
        .into_iter()
        .map(|hit| hit.rowid)
        .collect()
}

#[test]
fn ocr_then_index_then_vlm_emits_in_order_and_makes_item_searchable() {
    let harness = Harness::new(
        FakeOcr::with_text("季度营收漏斗图 转化率提升"),
        FakeVision::typical("一张季度营收漏斗图", &["营收", "漏斗图"]),
    );
    let item = harness.insert_image("hash-1", "Chrome 中的看板");
    // 只入队 ocr：index 与 vlm 由 ocr handler 接力入队（顺序保证）
    harness
        .library
        .enqueue_stage(&item, STAGE_OCR, PRIORITY_NEW)
        .unwrap();

    let handle = harness.spawn();
    assert!(
        wait_until(
            || harness.library.get_item(&item).unwrap().unwrap().status == ItemStatus::Ready,
            Duration::from_secs(5)
        ),
        "素材应最终 ready"
    );
    // vlm 完成后还会重新入队一次 index（rev+1）：等它落库再断言检索
    assert!(wait_until(
        || harness.library.fts_count().unwrap() == 1
            && harness
                .library
                .insight_get(&item)
                .unwrap()
                .unwrap()
                .summary_source
                == SummarySource::Vlm,
        Duration::from_secs(5)
    ));
    handle.shutdown();

    // 事件顺序：先 indexed 后 enriched
    let order = harness.events.order.lock().unwrap().clone();
    let indexed_pos = order.iter().position(|e| e == &format!("indexed:{item}"));
    let enriched_pos = order.iter().position(|e| e == &format!("enriched:{item}"));
    assert!(indexed_pos.is_some(), "必须发过 item_indexed：{order:?}");
    assert!(enriched_pos.is_some(), "必须发过 item_enriched：{order:?}");
    assert!(
        indexed_pos < enriched_pos,
        "阶段一必须先于阶段二：{order:?}"
    );

    let insight = harness.library.insight_get(&item).unwrap().unwrap();
    assert_eq!(insight.summary_source, SummarySource::Vlm);
    assert_eq!(insight.summary.as_deref(), Some("一张季度营收漏斗图"));
    assert_eq!(insight.primary_color.as_deref(), Some("orange"));
    assert!(insight.ocr_text.is_some());

    // 三路都可查：FTS 命中 + 向量行存在
    let rowid = harness.library.item_rowid(&item).unwrap().unwrap();
    assert!(fts_hits(&harness, "漏斗图").contains(&rowid));
    assert_eq!(harness.library.vector_count().unwrap(), 1);
    // 自动标签已贴
    let tags: Vec<String> = harness
        .library
        .tags_for_item(&item)
        .unwrap()
        .into_iter()
        .map(|tag| tag.tag.name)
        .collect();
    assert!(tags.contains(&"营收".to_string()), "tags={tags:?}");
}

#[test]
fn image_without_text_keeps_summary_null_and_is_still_indexed() {
    let harness = Harness::new(
        FakeOcr::empty(),
        FakeVision::typical("纯色背景的空白页", &["空白"]),
    );
    let item = harness.insert_image("hash-blank", "空白窗口");
    // 只入队 ocr：index 与 vlm 由 ocr handler 接力入队（顺序保证）
    harness
        .library
        .enqueue_stage(&item, STAGE_OCR, PRIORITY_NEW)
        .unwrap();

    let handle = harness.spawn();
    assert!(wait_until(
        || harness.library.get_item(&item).unwrap().unwrap().status == ItemStatus::Ready
            && harness.library.fts_count().unwrap() == 1,
        Duration::from_secs(5)
    ));
    handle.shutdown();

    let insight = harness.library.insight_get(&item).unwrap().unwrap();
    // OCR 空结果：NULL 而不是 0/空串
    assert!(insight.ocr_text.is_none());
    assert!(insight.ocr_boxes.is_empty());
    assert!(insight.ocr_confidence.is_none(), "无文字不得写 0");
    assert!(!insight.low_ocr_confidence(), "无文字不得触发「置信度低」");
    // VLM 完成后摘要来自 VLM（不是占位）
    assert_eq!(insight.summary_source, SummarySource::Vlm);
    // 该素材可被标题命中
    let rowid = harness.library.item_rowid(&item).unwrap().unwrap();
    assert!(fts_hits(&harness, "空白窗口").contains(&rowid));
}

#[test]
fn ocr_failure_still_indexes_metadata() {
    let harness = Harness::new(
        FakeOcr::failing(FakeFail::Internal("both engines failed".into())),
        FakeVision::typical("一张报错截图", &["报错"]),
    );
    let item = harness.insert_image("hash-err", "报错弹窗");
    harness
        .library
        .enqueue_stage(&item, STAGE_OCR, PRIORITY_NEW)
        .unwrap();

    let handle = harness.spawn();
    // OCR 失败后仍要跑到 index（接力入队），因此等 FTS 行出现而不是只等 insights 行
    assert!(
        wait_until(
            || harness.library.fts_count().unwrap() == 1,
            Duration::from_secs(5)
        ),
        "OCR 失败也必须完成索引"
    );
    handle.shutdown();

    let insight = harness.library.insight_get(&item).unwrap().unwrap();
    assert!(insight.ocr_text.is_none());
    assert!(insight.ocr_engine.is_none());
    // index 照常完成（标题可搜）
    let rowid = harness.library.item_rowid(&item).unwrap().unwrap();
    assert!(fts_hits(&harness, "报错弹窗").contains(&rowid));
}

#[test]
fn vlm_retryable_failure_marks_pending_after_three_attempts() {
    let harness = Harness::new(
        FakeOcr::with_text("发票号码 00329181"),
        FakeVision::failing(FakeFail::Provider("503".into())),
    );
    let item = harness.insert_image("hash-retry", "发票页面");
    // 只入队 ocr：index 与 vlm 由 ocr handler 接力入队（顺序保证）
    harness
        .library
        .enqueue_stage(&item, STAGE_OCR, PRIORITY_NEW)
        .unwrap();

    let handle = harness.spawn();
    assert!(
        wait_until(
            || harness.library.get_item(&item).unwrap().unwrap().status == ItemStatus::Pending,
            Duration::from_secs(5)
        ),
        "3 次失败后应 pending"
    );
    handle.shutdown();

    assert_eq!(harness.vision.calls.load(Ordering::SeqCst), 3);
    assert_eq!(
        job_state(&harness, &item, STAGE_VLM),
        Some(JobState::Failed)
    );
    // OCR 摘录已占位（阶段一数据仍在，仍可检索）
    let insight = harness.library.insight_get(&item).unwrap().unwrap();
    assert_eq!(insight.summary_source, SummarySource::Ocr);
    assert!(insight.summary.as_deref().unwrap().contains("发票"));
    let rowid = harness.library.item_rowid(&item).unwrap().unwrap();
    assert!(fts_hits(&harness, "发票").contains(&rowid));
    assert!(!harness.events.pending.lock().unwrap().is_empty());
}

#[test]
fn vlm_config_error_marks_pending_without_retry() {
    let harness = Harness::new(
        FakeOcr::with_text("报错原文"),
        FakeVision::failing(FakeFail::Config("HTTP 401".into())),
    );
    let item = harness.insert_image("hash-401", "登录页");
    harness
        .library
        .enqueue_stage(&item, STAGE_VLM, PRIORITY_NEW)
        .unwrap();

    let handle = harness.spawn();
    assert!(wait_until(
        || harness.library.get_item(&item).unwrap().unwrap().status == ItemStatus::Pending,
        Duration::from_secs(5)
    ));
    handle.shutdown();

    assert_eq!(
        harness.vision.calls.load(Ordering::SeqCst),
        1,
        "401 不得重试"
    );
    let job = &harness.library.jobs_for_stage(&item, STAGE_VLM).unwrap()[0];
    assert_eq!(job.state, JobState::Failed);
    assert!(job.last_error.as_deref().unwrap().contains("401"));
}

#[test]
fn suppressed_auto_tag_is_not_reattached() {
    let harness = Harness::new(
        FakeOcr::with_text("发票"),
        FakeVision::typical("一张发票截图", &["发票", "财务"]),
    );
    let item = harness.insert_image("hash-tags", "发票明细");
    // 用户之前删过自动标签「发票」：先自动贴上再手动移除 → 写抑制记录
    let suppressed = harness
        .library
        .attach_auto_tag_for_test(&item, "发票")
        .unwrap();
    harness.library.tag_detach(&item, &suppressed).unwrap();
    harness
        .library
        .enqueue_stage(&item, STAGE_VLM, PRIORITY_NEW)
        .unwrap();

    let handle = harness.spawn();
    assert!(wait_until(
        || harness.library.get_item(&item).unwrap().unwrap().status == ItemStatus::Ready,
        Duration::from_secs(5)
    ));
    handle.shutdown();

    let tags: Vec<String> = harness
        .library
        .tags_for_item(&item)
        .unwrap()
        .into_iter()
        .map(|tag| tag.tag.name)
        .collect();
    assert!(
        !tags.contains(&"发票".to_string()),
        "被抑制的标签不得回贴：{tags:?}"
    );
    assert!(tags.contains(&"财务".to_string()), "tags={tags:?}");
}

#[test]
fn identical_content_hash_calls_vlm_once() {
    let harness = Harness::new(
        FakeOcr::with_text("同一画面"),
        FakeVision::typical("同一张截图的摘要", &["截图"]),
    );
    let first = harness.insert_image("same-hash", "第一次捕获");
    let second = harness.insert_image("same-hash", "第二次捕获");
    for item in [&first, &second] {
        harness
            .library
            .enqueue_stage(item, STAGE_VLM, PRIORITY_NEW)
            .unwrap();
    }

    let handle = harness.spawn();
    assert!(wait_until(
        || {
            harness.library.get_item(&first).unwrap().unwrap().status == ItemStatus::Ready
                && harness.library.get_item(&second).unwrap().unwrap().status == ItemStatus::Ready
        },
        Duration::from_secs(5)
    ));
    handle.shutdown();

    assert_eq!(
        harness.vision.calls.load(Ordering::SeqCst),
        1,
        "同 hash 的第二次捕获必须复用解析结果"
    );
    let second_insight = harness.library.insight_get(&second).unwrap().unwrap();
    assert_eq!(second_insight.summary_source, SummarySource::Vlm);
    assert_eq!(second_insight.summary.as_deref(), Some("同一张截图的摘要"));
    // 两条素材仍然都在（不去重）
    assert_eq!(harness.library.count_items().unwrap(), 2);
}

#[test]
fn note_change_cancels_old_index_job_and_writes_new_note_to_fts() {
    let harness = Harness::new(
        FakeOcr::with_text("初始文本"),
        FakeVision::typical("摘要", &[]),
    );
    let item = harness.insert_image("hash-note", "窗口");
    harness
        .library
        .enqueue_stage(&item, STAGE_OCR, PRIORITY_NEW)
        .unwrap();

    let handle = harness.spawn();
    assert!(wait_until(
        || fts_hits(&harness, "初始文本").len() == 1,
        Duration::from_secs(5)
    ));

    // 改备注 → 钩子 bump_revision + 入队新 index（同事务）
    harness.library.set_note(&item, "补充备注 关键词").unwrap();
    assert!(wait_until(
        || fts_hits(&harness, "关键词").len() == 1,
        Duration::from_secs(5)
    ));
    handle.shutdown();

    assert_eq!(
        harness.reindex_calls.load(Ordering::SeqCst),
        1,
        "钩子只触发一次"
    );
    let jobs = harness.library.jobs_for_stage(&item, STAGE_INDEX).unwrap();
    assert!(jobs.len() >= 2, "备注变更必须新增一条 index 任务：{jobs:?}");
    // 按 revision 降序：最新 revision 成功落库，且比旧任务大
    assert_eq!(jobs[0].state, JobState::Succeeded);
    assert!(jobs[0].revision > jobs[jobs.len() - 1].revision);
}

#[test]
fn text_item_gets_extractive_summary_and_full_text_in_fts() {
    let harness = Harness::new(FakeOcr::empty(), FakeVision::typical("摘要", &[]));
    let body = "这是一份很长的会议纪要，讨论了季度营收、漏斗图转化率与下一步行动项。";
    let item = harness.insert_text("hash-text", body);
    harness
        .library
        .enqueue_stage(&item, STAGE_INDEX, PRIORITY_NEW)
        .unwrap();
    harness
        .library
        .enqueue_stage(&item, STAGE_VLM, PRIORITY_NEW)
        .unwrap();

    let handle = harness.spawn();
    assert!(wait_until(
        || harness.library.get_item(&item).unwrap().unwrap().status == ItemStatus::Ready,
        Duration::from_secs(5)
    ));
    handle.shutdown();

    // 文本走 TextProvider（不是 Vision）
    assert_eq!(harness.vision.calls.load(Ordering::SeqCst), 0);
    assert_eq!(harness.text.calls.load(Ordering::SeqCst), 1);
    let rowid = harness.library.item_rowid(&item).unwrap().unwrap();
    assert!(
        fts_hits(&harness, "行动项").contains(&rowid),
        "原文全量进 FTS"
    );
    let insight = harness.library.insight_get(&item).unwrap().unwrap();
    assert_eq!(insight.summary_source, SummarySource::Vlm);
}

#[test]
fn startup_checks_repair_missing_fts_rows() {
    let harness = Harness::new(FakeOcr::with_text("原文"), FakeVision::typical("摘要", &[]));
    let item = harness.insert_image("hash-repair", "修复目标");
    harness
        .library
        .enqueue_stage(&item, STAGE_OCR, PRIORITY_NEW)
        .unwrap();
    let handle = harness.spawn();
    assert!(wait_until(
        || harness.library.fts_count().unwrap() == 1,
        Duration::from_secs(5)
    ));
    handle.shutdown();

    // 手工删掉 FTS 行，模拟外部破坏
    let rowid = harness.library.item_rowid(&item).unwrap().unwrap();
    harness.library.fts_delete(rowid).unwrap();
    assert_eq!(harness.library.fts_count().unwrap(), 0);

    let report = run_startup_checks(&harness.library).unwrap();
    assert_eq!(report.repaired, 1);
    assert!(!report.counts.is_consistent());

    let handle = harness.spawn();
    assert!(wait_until(
        || harness.library.fts_count().unwrap() == 1,
        Duration::from_secs(5)
    ));
    handle.shutdown();
    assert!(fts_hits(&harness, "原文").contains(&rowid));
    assert_eq!(harness.library.index_counts().unwrap().fts, 1);
}

#[test]
fn retry_pending_enqueues_vlm_again() {
    let harness = Harness::new(FakeOcr::with_text("x"), FakeVision::typical("摘要", &[]));
    let item = harness.insert_image("hash-pending", "待重试");
    harness
        .library
        .update_status(&item, ItemStatus::Pending, None)
        .unwrap();
    harness
        .library
        .enqueue_stage(&item, STAGE_VLM, PRIORITY_NEW)
        .unwrap();
    // 先把已有任务置为失败终态，才能观察「重试轮重置」
    {
        let job = harness.library.jobs_for_stage(&item, STAGE_VLM).unwrap()[0].clone();
        harness.library.finish_job_failed(&job.id, "boom").unwrap();
    }

    assert_eq!(retry_pending(&harness.library).unwrap(), 1);
    let job = &harness.library.jobs_for_stage(&item, STAGE_VLM).unwrap()[0];
    assert_eq!(job.state, JobState::Queued);
    assert_eq!(job.attempts, 0);
}

#[test]
fn rebuild_stage_reruns_index_for_one_item() {
    let harness = Harness::new(
        FakeOcr::with_text("重建文本"),
        FakeVision::typical("摘要", &[]),
    );
    let item = harness.insert_image("hash-rebuild", "重建");
    let rowid = harness.library.item_rowid(&item).unwrap().unwrap();
    // 预置一条垃圾 FTS 行，验证 rebuild 会覆盖它
    let junk = tokenize_for_fts("垃圾");
    harness
        .library
        .fts_upsert(
            rowid,
            &FtsColumns {
                title: &junk,
                ..Default::default()
            },
        )
        .unwrap();
    harness
        .library
        .enqueue_stage(&item, STAGE_REBUILD, PRIORITY_NEW)
        .unwrap();

    let handle = harness.spawn();
    assert!(wait_until(
        || harness.library.vector_count().unwrap() == 1,
        Duration::from_secs(5)
    ));
    handle.shutdown();

    assert_eq!(
        job_state(&harness, &item, STAGE_REBUILD),
        Some(JobState::Succeeded)
    );
    // 重建后的 FTS 来自 items/insights：标题可搜，垃圾行不再命中
    assert!(fts_hits(&harness, "重建").contains(&rowid));
    assert!(fts_hits(&harness, "垃圾").is_empty(), "旧 FTS 行必须被覆盖");
}

#[test]
fn blank_ocr_without_vlm_leaves_summary_null_but_searchable_by_title() {
    // 阶段一单独执行（不跑 vlm）：无字图片的占位摘要必须不写
    let harness = Harness::new(FakeOcr::empty(), FakeVision::typical("不会被调用", &[]));
    let item = harness.insert_image("hash-blank-only", "纯色面板");
    harness
        .library
        .enqueue_stage(&item, STAGE_OCR, PRIORITY_NEW)
        .unwrap();
    harness.library.set_note(&item, "这是一张空图").unwrap();

    let handle = harness.spawn();
    assert!(wait_until(
        || harness.library.fts_count().unwrap() == 1,
        Duration::from_secs(5)
    ));
    handle.shutdown();

    let insight = harness.library.insight_get(&item).unwrap().unwrap();
    assert!(
        insight.summary.is_none(),
        "无字图片不得写占位摘要：{:?}",
        insight.summary
    );
    assert_eq!(insight.summary_source, SummarySource::None);
    assert!(insight.ocr_text.is_none());
    // 仍可被标题/备注命中
    let rowid = harness.library.item_rowid(&item).unwrap().unwrap();
    assert!(fts_hits(&harness, "纯色面板").contains(&rowid));
    assert!(fts_hits(&harness, "空图").contains(&rowid));
}

#[test]
fn crash_during_vlm_recovers_and_reruns_without_duplicates() {
    let harness = Harness::new(
        FakeOcr::with_text("报错原文"),
        FakeVision::typical("一张报错截图", &["报错"]),
    );
    let item = harness.insert_image("hash-crash", "崩溃恢复");
    harness
        .library
        .enqueue_stage(&item, STAGE_VLM, PRIORITY_NEW)
        .unwrap();
    // 模拟「上次运行在 claim 之后被强杀」
    let claimed = harness
        .library
        .claim_next_job(visrecall_domain::now_millis())
        .unwrap()
        .expect("应能 claim");
    assert_eq!(claimed.attempts, 1);

    let handle = harness.spawn();
    assert!(wait_until(
        || harness.library.get_item(&item).unwrap().unwrap().status == ItemStatus::Ready,
        Duration::from_secs(5)
    ));
    handle.shutdown();

    let jobs = harness.library.jobs_for_stage(&item, STAGE_VLM).unwrap();
    assert_eq!(jobs.len(), 1, "同一 revision 不产生第二条任务");
    assert_eq!(jobs[0].id, claimed.id);
    assert_eq!(jobs[0].attempts, 2, "恢复后续跑，attempts 继续累加");
    assert_eq!(jobs[0].state, JobState::Succeeded);
    // 幂等：只有一条 insights 行、只贴了一次标签
    let conn_count = harness.library.query_log_count().unwrap();
    assert_eq!(conn_count, 0);
    let tags = harness.library.tags_for_item(&item).unwrap();
    assert_eq!(tags.len(), 1, "自动标签不重复贴：{tags:?}");
}
