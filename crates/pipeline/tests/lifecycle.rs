//! 索引生命周期：启动自检修复、pending 重试轮、向量蓝绿重建。

use std::sync::Arc;
use std::time::{Duration, Instant};

use visrecall_domain::providers::VectorIndex;
use visrecall_domain::{ItemKind, ItemStatus, NewItem};
use visrecall_library::{BruteForceInt8, JobState, Library, DEFAULT_VECTOR_TABLE};
use visrecall_pipeline::lifecycle::{
    next_rebuild_table, rebuild_embeddings, rebuild_fts_row, run_startup_checks,
};
use visrecall_pipeline::testing::{FakeEmbed, FakeOcr, FakeText, FakeVision};
use visrecall_pipeline::{
    with_builtin_handlers, NoopEvents, Pipeline, PipelineConfig, PipelineDeps, PipelineHandle,
    PRIORITY_NEW, STAGE_INDEX, STAGE_OCR, STAGE_VLM,
};

struct Harness {
    _dir: tempfile::TempDir,
    library: Arc<Library>,
    runtime: tokio::runtime::Runtime,
    embed: Arc<FakeEmbed>,
}

impl Harness {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let library = Arc::new(Library::open(dir.path().to_path_buf()).unwrap());
        library.init_embedding_space(FakeEmbed::ID, 8).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        Self {
            _dir: dir,
            library,
            runtime,
            embed: Arc::new(FakeEmbed::new()),
        }
    }

    fn deps(&self) -> Arc<PipelineDeps> {
        Arc::new(PipelineDeps {
            ocr: Arc::new(FakeOcr::with_text("营收漏斗图")),
            embed: self.embed.clone(),
            vision: Arc::new(FakeVision::typical("摘要", &["标签"])),
            text: Arc::new(FakeText::new("文本摘要", &[])),
            runtime: self.runtime.handle().clone(),
        })
    }

    fn spawn(&self) -> PipelineHandle {
        with_builtin_handlers(Pipeline::new(
            Arc::clone(&self.library),
            self.deps(),
            Arc::new(NoopEvents),
        ))
        .with_config(PipelineConfig {
            workers: 1,
            poll_interval: Duration::from_millis(5),
            backoff_secs: vec![0, 0, 0],
            max_attempts: 3,
            status_debounce: Duration::from_millis(1),
        })
        .spawn()
        .unwrap()
    }

    fn insert_image(&self, hash: &str, title: &str) -> String {
        let id = visrecall_domain::new_item_id();
        let rel = format!("originals/images/{id}.png");
        let path = self.library.root().join(&rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"fake").unwrap();
        self.library
            .insert_item_with_id(
                id.clone(),
                NewItem {
                    kind: ItemKind::Image,
                    captured_at: 1_700_000_000_000,
                    source_app: None,
                    source_title: Some(title.into()),
                    source_url: None,
                    width: None,
                    height: None,
                    char_count: None,
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

#[test]
fn next_rebuild_table_alternates_between_v2_and_v3() {
    assert_eq!(
        next_rebuild_table(DEFAULT_VECTOR_TABLE),
        "item_vectors_int8_v2"
    );
    assert_eq!(
        next_rebuild_table("item_vectors_int8_v2"),
        "item_vectors_int8_v3"
    );
    assert_eq!(
        next_rebuild_table("item_vectors_int8_v3"),
        "item_vectors_int8_v4"
    );
}

#[test]
fn blue_green_rebuild_switches_pointer_and_keeps_old_table() {
    let harness = Harness::new();
    let items: Vec<String> = (0..3)
        .map(|i| harness.insert_image(&format!("h{i}"), &format!("素材{i}")))
        .collect();
    for item in &items {
        harness
            .library
            .enqueue_stage(item, STAGE_INDEX, PRIORITY_NEW)
            .unwrap();
    }
    let handle = harness.spawn();
    assert!(wait_until(
        || harness.library.vector_count().unwrap() == 3,
        Duration::from_secs(5)
    ));
    handle.shutdown();

    // 重建期间检索可用：先用旧表查一次
    let index = BruteForceInt8::new(Arc::clone(&harness.library));
    let query = FakeEmbed::vector_for("素材1");
    assert_eq!(index.search(&query, 3, None).unwrap().len(), 3);

    let new_embed = FakeEmbed::with_id("fake-embed-v2", 8);
    let report = rebuild_embeddings(
        &harness.library,
        &new_embed,
        harness.runtime.handle(),
        "fake-embed-v2",
        8,
    )
    .unwrap();

    assert!(report.verified, "行数校验必须通过");
    assert!(report.switched, "校验通过后必须切指针");
    assert_eq!(report.target_table, "item_vectors_int8_v2");
    assert_eq!(report.items, 3);
    assert_eq!(
        harness.library.active_vector_table(),
        "item_vectors_int8_v2"
    );
    assert_eq!(
        harness.library.embedding_space(),
        Some(("fake-embed-v2".to_string(), 8))
    );
    // 旧表保留（回滚用）
    assert_eq!(
        harness
            .library
            .vector_count_for(DEFAULT_VECTOR_TABLE)
            .unwrap(),
        3
    );
    assert_eq!(
        harness
            .library
            .vector_count_for("item_vectors_int8_v2")
            .unwrap(),
        3
    );
    // 切换后检索仍然读得到（零中断）
    assert_eq!(index.search(&query, 3, None).unwrap().len(), 3);

    // 第二次重建切到 _v3
    let newer = FakeEmbed::with_id("fake-embed-v3", 8);
    let report = rebuild_embeddings(
        &harness.library,
        &newer,
        harness.runtime.handle(),
        "fake-embed-v3",
        8,
    )
    .unwrap();
    assert_eq!(report.target_table, "item_vectors_int8_v3");
    assert!(report.switched);
}

#[test]
fn rebuild_keeps_old_table_when_row_count_mismatches() {
    let harness = Harness::new();
    let item = harness.insert_image("h0", "素材");
    harness
        .library
        .enqueue_stage(&item, STAGE_INDEX, PRIORITY_NEW)
        .unwrap();
    let handle = harness.spawn();
    assert!(wait_until(
        || harness.library.vector_count().unwrap() == 1,
        Duration::from_secs(5)
    ));
    handle.shutdown();

    // 维度不一致 → rebuild 直接报错，指针不动
    let wrong_dim = FakeEmbed::with_id("fake-embed-16", 16);
    let err = rebuild_embeddings(
        &harness.library,
        &wrong_dim,
        harness.runtime.handle(),
        "fake-embed-16",
        16,
    )
    .expect_err("维度不一致必须报错");
    assert!(matches!(err, visrecall_domain::AppError::InvalidInput(_)));
    assert_eq!(harness.library.active_vector_table(), DEFAULT_VECTOR_TABLE);
}

#[test]
fn startup_checks_repair_index_rows_without_retrying_pending_items() {
    let harness = Harness::new();
    let broken = harness.insert_image("h-broken", "缺索引");
    let pending = harness.insert_image("h-pending", "待重试");
    harness
        .library
        .update_status(&pending, ItemStatus::Pending, None)
        .unwrap();

    let report = run_startup_checks(&harness.library).unwrap();
    assert_eq!(report.repaired, 2, "两条素材都缺派生数据");
    assert!(!report.counts.is_consistent());

    let index_jobs = harness
        .library
        .jobs_for_stage(&broken, STAGE_INDEX)
        .unwrap();
    assert_eq!(index_jobs.len(), 1);
    assert_eq!(index_jobs[0].state, JobState::Queued);
    // 只补派生索引：启动不再把 pending 素材重新入队 vlm。
    assert!(
        harness
            .library
            .jobs_for_stage(&pending, STAGE_VLM)
            .unwrap()
            .is_empty(),
        "pending 素材不应在启动时自动重试"
    );
}

#[test]
fn rebuild_fts_row_restores_a_deleted_row() {
    let harness = Harness::new();
    let item = harness.insert_image("h0", "发票明细");
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

    let rowid = harness.library.item_rowid(&item).unwrap().unwrap();
    harness.library.fts_delete(rowid).unwrap();
    assert_eq!(harness.library.fts_count().unwrap(), 0);

    rebuild_fts_row(&harness.library, &item).unwrap();
    assert_eq!(harness.library.fts_count().unwrap(), 1);
    let expr = visrecall_search::build_match_expr(&visrecall_search::terms_for_query("发票"), &[]);
    assert_eq!(harness.library.fts_search(&expr, None, 5).unwrap().len(), 1);
}

#[test]
fn image_pipeline_from_ocr_only_reaches_ready() {
    let harness = Harness::new();
    harness.library.set_cloud_image_ai(true).unwrap();
    let item = harness.insert_image("h0", "接力链路");
    // capture 只入队 ocr，其余由 handler 接力（index → vlm → index）
    harness
        .library
        .enqueue_stage(&item, STAGE_OCR, PRIORITY_NEW)
        .unwrap();

    let handle = harness.spawn();
    assert!(wait_until(
        || harness.library.get_item(&item).unwrap().unwrap().status == ItemStatus::Ready,
        Duration::from_secs(5)
    ));
    assert!(wait_until(
        || harness.library.fts_count().unwrap() == 1
            && harness.library.vector_count().unwrap() == 1,
        Duration::from_secs(5)
    ));
    handle.shutdown();

    let stages: Vec<String> = ["ocr", "index", "vlm"]
        .iter()
        .filter_map(|stage| {
            harness
                .library
                .jobs_for_stage(&item, stage)
                .unwrap()
                .first()
                .map(|job| format!("{stage}:{}", job.state.as_str()))
        })
        .collect();
    assert_eq!(
        stages,
        vec!["ocr:succeeded", "index:succeeded", "vlm:succeeded"],
        "三阶段都应成功"
    );
}
