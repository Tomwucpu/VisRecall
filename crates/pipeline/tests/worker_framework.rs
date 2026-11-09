//! worker 框架行为：并发不重复、退避、配置错误终止、stale 取消、崩溃恢复、优先级、水位事件。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use visrecall_domain::{ItemKind, NewItem};
use visrecall_library::{Job, JobState, Library};
use visrecall_pipeline::testing::{FakeEmbed, FakeFail, FakeOcr, FakeText, FakeVision};
use visrecall_pipeline::{
    JobCtx, JobHandler, Pipeline, PipelineConfig, PipelineDeps, PipelineEvents, PipelineHandle,
    PRIORITY_NEW, PRIORITY_REBUILD,
};

/// 记录调用次数与执行顺序的假 handler。
struct CountingHandler {
    stage: &'static str,
    calls: Arc<AtomicUsize>,
    order: Arc<Mutex<Vec<String>>>,
    fail: Arc<Mutex<Option<FakeFail>>>,
}

impl CountingHandler {
    fn new(stage: &'static str) -> Self {
        Self {
            stage,
            calls: Arc::new(AtomicUsize::new(0)),
            order: Arc::new(Mutex::new(Vec::new())),
            fail: Arc::new(Mutex::new(None)),
        }
    }

    fn with_fail(stage: &'static str, fail: FakeFail) -> Self {
        let handler = Self::new(stage);
        *handler.fail.lock().unwrap() = Some(fail);
        handler
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl JobHandler for CountingHandler {
    fn stage(&self) -> &'static str {
        self.stage
    }

    fn run(&self, ctx: &JobCtx<'_>) -> Result<(), visrecall_domain::AppError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.order.lock().unwrap().push(ctx.item_id().to_string());
        if let Some(fail) = self.fail.lock().unwrap().as_ref() {
            return Err(fail.to_error());
        }
        Ok(())
    }
}

/// 收集 `pipeline_status` 的假事件接收端。
#[derive(Default)]
struct RecordingEvents {
    statuses: Mutex<Vec<(u64, u64)>>,
    indexed: Mutex<Vec<String>>,
    enriched: Mutex<Vec<String>>,
    failed: Mutex<Vec<String>>,
}

impl PipelineEvents for RecordingEvents {
    fn pipeline_status(&self, queued: u64, running: u64) {
        self.statuses.lock().unwrap().push((queued, running));
    }

    fn item_indexed(&self, item_id: &str) {
        self.indexed.lock().unwrap().push(item_id.to_string());
    }

    fn item_enriched(&self, item_id: &str) {
        self.enriched.lock().unwrap().push(item_id.to_string());
    }

    fn job_failed(&self, job: &Job, _error: &visrecall_domain::AppError) {
        self.failed.lock().unwrap().push(job.stage.clone());
    }
}

struct Harness {
    _dir: tempfile::TempDir,
    library: Arc<Library>,
    runtime: tokio::runtime::Runtime,
    ocr: Arc<FakeOcr>,
    embed: Arc<FakeEmbed>,
    vision: Arc<FakeVision>,
    text: Arc<FakeText>,
    events: Arc<RecordingEvents>,
}

impl Harness {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let library = Arc::new(Library::open(dir.path().to_path_buf()).unwrap());
        // 固定嵌入空间，向量表才能写入
        library.init_embedding_space(FakeEmbed::ID, 8).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        Self {
            _dir: dir,
            library,
            runtime,
            ocr: Arc::new(FakeOcr::with_text("发票号码 00329181")),
            embed: Arc::new(FakeEmbed::new()),
            vision: Arc::new(FakeVision::typical("季度营收漏斗图", &["营收"])),
            text: Arc::new(FakeText::new("会议纪要", &["会议"])),
            events: Arc::new(RecordingEvents::default()),
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

    fn insert_item(&self, hash: &str) -> String {
        self.library
            .insert_item(NewItem {
                kind: ItemKind::Image,
                captured_at: 1_700_000_000_000,
                source_app: None,
                source_title: Some("窗口标题".into()),
                source_url: None,
                width: Some(1920),
                height: Some(1080),
                char_count: None,
                original_path: format!("originals/images/{hash}.png"),
                content_hash: hash.into(),
            })
            .unwrap()
            .id
    }

    fn pipeline(&self, handlers: Vec<Arc<CountingHandler>>, config: PipelineConfig) -> Pipeline {
        let mut pipeline = Pipeline::new(
            self.library.clone(),
            self.deps(),
            self.events.clone() as Arc<dyn PipelineEvents>,
        )
        .with_config(config);
        for handler in handlers {
            pipeline = pipeline.register(handler as Arc<dyn JobHandler>);
        }
        pipeline
    }
}

fn fast_config(workers: usize) -> PipelineConfig {
    PipelineConfig {
        workers,
        poll_interval: Duration::from_millis(5),
        // 测试里退避为 0，避免等待 2/5/15 秒
        backoff_secs: vec![0, 0, 0],
        max_attempts: 3,
        status_debounce: Duration::from_millis(1),
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

fn job_state_of(harness: &Harness, item_id: &str, stage: &str) -> Option<JobState> {
    harness
        .library
        .jobs_for_stage(item_id, stage)
        .unwrap()
        .first()
        .map(|job: &Job| job.state)
}

#[test]
fn two_workers_never_execute_the_same_job_twice() {
    let harness = Harness::new();
    let handler = Arc::new(CountingHandler::new("index"));
    let items: Vec<String> = (0..6)
        .map(|i| harness.insert_item(&format!("h{i}")))
        .collect();
    for item in &items {
        harness
            .library
            .enqueue_stage(item, "index", PRIORITY_NEW)
            .unwrap();
    }

    let handle: PipelineHandle = harness
        .pipeline(vec![handler.clone()], fast_config(2))
        .spawn()
        .unwrap();

    assert!(
        wait_until(|| handler.calls() >= 6, Duration::from_secs(5)),
        "6 条任务应全部执行，实际 {}",
        handler.calls()
    );
    // 给重复执行留出观察窗口
    std::thread::sleep(Duration::from_millis(80));
    handle.shutdown();

    assert_eq!(handler.calls(), 6, "每条任务只能执行一次");
    for item in &items {
        assert_eq!(
            job_state_of(&harness, item, "index"),
            Some(JobState::Succeeded)
        );
    }
}

#[test]
fn provider_failure_retries_until_attempts_exhausted() {
    let harness = Harness::new();
    let handler = Arc::new(CountingHandler::with_fail(
        "index",
        FakeFail::Provider("503".into()),
    ));
    let item = harness.insert_item("h0");
    harness
        .library
        .enqueue_stage(&item, "index", PRIORITY_NEW)
        .unwrap();

    let handle = harness
        .pipeline(vec![handler.clone()], fast_config(1))
        .spawn()
        .unwrap();

    assert!(
        wait_until(
            || job_state_of(&harness, &item, "index") == Some(JobState::Failed),
            Duration::from_secs(5)
        ),
        "3 次尝试后必须进入 failed"
    );
    handle.shutdown();

    assert_eq!(handler.calls(), 3, "尝试上限为 3");
    let job = &harness.library.jobs_for_stage(&item, "index").unwrap()[0];
    assert_eq!(job.attempts, 3);
    assert!(job.last_error.as_deref().unwrap().contains("503"));
    assert_eq!(harness.events.failed.lock().unwrap().len(), 1);
}

#[test]
fn config_error_terminates_without_retry() {
    let harness = Harness::new();
    let handler = Arc::new(CountingHandler::with_fail(
        "vlm",
        FakeFail::Config("HTTP 401".into()),
    ));
    let item = harness.insert_item("h0");
    harness
        .library
        .enqueue_stage(&item, "vlm", PRIORITY_NEW)
        .unwrap();

    let handle = harness
        .pipeline(vec![handler.clone()], fast_config(1))
        .spawn()
        .unwrap();

    assert!(wait_until(
        || job_state_of(&harness, &item, "vlm") == Some(JobState::Failed),
        Duration::from_secs(5)
    ));
    std::thread::sleep(Duration::from_millis(60));
    handle.shutdown();

    assert_eq!(handler.calls(), 1, "配置错误不得重试");
    let job = &harness.library.jobs_for_stage(&item, "vlm").unwrap()[0];
    assert_eq!(job.attempts, 1);
    assert!(job.last_error.as_deref().unwrap().contains("401"));
}

#[test]
fn stale_revision_is_cancelled_before_execution() {
    let harness = Harness::new();
    let handler = Arc::new(CountingHandler::new("index"));
    let item = harness.insert_item("h0");
    harness
        .library
        .enqueue_stage(&item, "index", PRIORITY_NEW)
        .unwrap();
    // 入队后元数据又变了：旧任务作废
    harness.library.bump_revision(&item).unwrap();

    let handle = harness
        .pipeline(vec![handler.clone()], fast_config(1))
        .spawn()
        .unwrap();

    assert!(wait_until(
        || job_state_of(&harness, &item, "index") == Some(JobState::Cancelled),
        Duration::from_secs(5)
    ));
    handle.shutdown();

    assert_eq!(handler.calls(), 0, "过期任务不得执行");
    let job = &harness.library.jobs_for_stage(&item, "index").unwrap()[0];
    assert_eq!(job.last_error.as_deref(), Some("stale revision"));
}

#[test]
fn crash_recovery_requeues_running_job_and_keeps_attempts() {
    let harness = Harness::new();
    let handler = Arc::new(CountingHandler::new("index"));
    let item = harness.insert_item("h0");
    harness
        .library
        .enqueue_stage(&item, "index", PRIORITY_NEW)
        .unwrap();
    // 模拟上次运行在 claim 之后被杀
    let claimed = harness
        .library
        .claim_next_job(visrecall_domain::now_millis())
        .unwrap();
    let claimed = claimed.expect("应能 claim");
    assert_eq!(claimed.attempts, 1);

    let handle = harness
        .pipeline(vec![handler.clone()], fast_config(1))
        .spawn()
        .unwrap();

    assert!(wait_until(
        || job_state_of(&harness, &item, "index") == Some(JobState::Succeeded),
        Duration::from_secs(5)
    ));
    handle.shutdown();

    let job = &harness.library.jobs_for_stage(&item, "index").unwrap()[0];
    assert_eq!(job.id, claimed.id, "恢复的是同一条任务");
    assert_eq!(job.attempts, 2, "attempts 要在崩溃前的基础上继续累加");
}

#[test]
fn priority_zero_runs_before_rebuild() {
    let harness = Harness::new();
    let handler = Arc::new(CountingHandler::new("index"));
    let rebuild_item = harness.insert_item("rebuild");
    let new_item = harness.insert_item("fresh");
    // 先入队低优先级的 rebuild，再入队新捕获
    harness
        .library
        .enqueue_stage(&rebuild_item, "index", PRIORITY_REBUILD)
        .unwrap();
    harness
        .library
        .enqueue_stage(&new_item, "index", PRIORITY_NEW)
        .unwrap();

    let handle = harness
        .pipeline(vec![handler.clone()], fast_config(1))
        .spawn()
        .unwrap();

    assert!(wait_until(|| handler.calls() >= 2, Duration::from_secs(5)));
    handle.shutdown();

    let order = handler.order.lock().unwrap().clone();
    assert_eq!(order, vec![new_item, rebuild_item], "priority 0 必须先执行");
}

#[test]
fn no_handler_for_stage_fails_the_job() {
    let harness = Harness::new();
    let item = harness.insert_item("h0");
    harness
        .library
        .enqueue_stage(&item, "mystery", PRIORITY_NEW)
        .unwrap();

    let handle = harness.pipeline(vec![], fast_config(1)).spawn().unwrap();
    assert!(wait_until(
        || job_state_of(&harness, &item, "mystery") == Some(JobState::Failed),
        Duration::from_secs(5)
    ));
    handle.shutdown();

    let job = &harness.library.jobs_for_stage(&item, "mystery").unwrap()[0];
    assert!(job
        .last_error
        .as_deref()
        .unwrap()
        .contains("no handler for stage"));
}

#[test]
fn pipeline_status_reaches_zero_when_idle() {
    let harness = Harness::new();
    let handler = Arc::new(CountingHandler::new("index"));
    let item = harness.insert_item("h0");
    harness
        .library
        .enqueue_stage(&item, "index", PRIORITY_NEW)
        .unwrap();

    let handle = harness
        .pipeline(vec![handler.clone()], fast_config(1))
        .spawn()
        .unwrap();

    assert!(wait_until(
        || harness.events.statuses.lock().unwrap().last().copied() == Some((0, 0)),
        Duration::from_secs(5)
    ));
    handle.shutdown();

    let statuses = harness.events.statuses.lock().unwrap().clone();
    assert!(!statuses.is_empty(), "必须发出过 pipeline_status");
    assert!(
        statuses.iter().any(|(queued, _)| *queued > 0),
        "执行期间应观察到队列非空：{statuses:?}"
    );
}

#[test]
fn enqueue_helper_uses_current_revision() {
    let harness = Harness::new();
    let item = harness.insert_item("h0");
    visrecall_pipeline::enqueue(&harness.library, &item, "index", PRIORITY_NEW).unwrap();
    harness.library.bump_revision(&item).unwrap();
    visrecall_pipeline::enqueue(&harness.library, &item, "index", PRIORITY_NEW).unwrap();

    let jobs = harness.library.jobs_for_stage(&item, "index").unwrap();
    assert_eq!(jobs.len(), 2);
    assert_eq!(jobs[0].revision, 1);
    assert_eq!(jobs[1].revision, 0);
}
