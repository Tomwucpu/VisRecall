//! Task 10：pipeline 宿主、模型配置热生效、语义就绪与事件接线。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use visrecall_app_core::pipeline_host::enqueue_initial_indexing;
use visrecall_app_core::{
    AppCore, AppEvent, AppPaths, EventSink, ModelConfigPatch, ProviderEntryInput, RecordingSink,
};
use visrecall_domain::{ItemKind, NewItem};
use visrecall_library::Library;
use visrecall_pipeline::{PRIORITY_NEW, STAGE_INDEX, STAGE_OCR};

fn paths_in(dir: &std::path::Path) -> AppPaths {
    AppPaths {
        config_file: dir.join("config.json"),
        default_library_root: dir.join("library"),
    }
}

/// 只关心事件到达、不关心热键的打开方式。
fn open(dir: &std::path::Path) -> (AppCore, Arc<RecordingSink>) {
    let sink = Arc::new(RecordingSink::default());
    let core = AppCore::open(paths_in(dir), sink.clone());
    (core, sink)
}

fn insert_item(library: &Library, kind: ItemKind, hash: &str) -> visrecall_domain::Item {
    library
        .insert_item(NewItem {
            kind,
            captured_at: 1_700_000_000_000,
            source_app: None,
            source_title: Some("标题".into()),
            source_url: None,
            width: None,
            height: None,
            char_count: None,
            original_path: format!("originals/images/{hash}.png"),
            content_hash: hash.into(),
        })
        .unwrap()
}

#[test]
fn initial_indexing_enqueues_ocr_for_images_and_index_for_text() {
    let dir = tempfile::tempdir().unwrap();
    let library = Library::open(dir.path().join("library")).unwrap();
    let image = insert_item(&library, ItemKind::Image, "img");
    let text = insert_item(&library, ItemKind::Text, "txt");

    enqueue_initial_indexing(&library, &image);
    enqueue_initial_indexing(&library, &text);

    let image_jobs = library.jobs_for_stage(&image.id, STAGE_OCR).unwrap();
    assert_eq!(image_jobs.len(), 1);
    assert_eq!(image_jobs[0].priority, PRIORITY_NEW);
    // 图片不直接入队 index：由 ocr handler 接力
    assert!(library
        .jobs_for_stage(&image.id, STAGE_INDEX)
        .unwrap()
        .is_empty());

    let text_jobs = library.jobs_for_stage(&text.id, STAGE_INDEX).unwrap();
    assert_eq!(text_jobs.len(), 1);
    assert!(library
        .jobs_for_stage(&text.id, STAGE_OCR)
        .unwrap()
        .is_empty());
}

#[test]
fn initial_indexing_warns_but_does_not_panic_for_missing_item() {
    let dir = tempfile::tempdir().unwrap();
    let library = Library::open(dir.path().join("library")).unwrap();
    let ghost = visrecall_domain::Item {
        id: "does-not-exist".into(),
        kind: ItemKind::Image,
        status: visrecall_domain::ItemStatus::Parsing,
        captured_at: 0,
        source_app: None,
        source_title: None,
        source_url: None,
        width: None,
        height: None,
        char_count: None,
        original_path: "originals/images/x.png".into(),
        thumb_path: None,
        note: String::new(),
        content_hash: "x".into(),
        revision: 0,
        primary_color: None,
    };
    enqueue_initial_indexing(&library, &ghost);
    assert_eq!(library.job_counts().unwrap(), (0, 0));
}

#[test]
fn start_pipeline_is_idempotent_and_reports_semantic_status() {
    let dir = tempfile::tempdir().unwrap();
    let (core, sink) = open(dir.path());
    assert!(!core.pipeline_running());

    core.start_pipeline().expect("pipeline 应启动");
    assert!(core.pipeline_running());
    // 幂等：再启动不报错也不重复起线程
    core.start_pipeline().expect("重复启动应幂等");

    // 预热线程完成（或失败）后必定发一条 semantic_status
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut found = false;
    while Instant::now() < deadline {
        if sink
            .events()
            .iter()
            .any(|event| matches!(event, AppEvent::SemanticStatus(_)))
        {
            found = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(found, "必须发过 semantic_status");
    // 语义就绪状态与事件载荷一致（有模型文件时 ready=true，无模型时 false）
    let reported = sink
        .events()
        .into_iter()
        .find_map(|event| match event {
            AppEvent::SemanticStatus(payload) => Some(payload.ready),
            _ => None,
        })
        .expect("semantic_status payload");
    assert_eq!(reported, core.semantic_ready());

    core.stop_pipeline();
    assert!(!core.pipeline_running());
}

#[test]
fn model_config_set_persists_and_hot_reloads() {
    let dir = tempfile::tempdir().unwrap();
    let (core, _sink) = open(dir.path());
    assert!(core.model_config().is_empty());

    let patch = ModelConfigPatch {
        primary: Some(ProviderEntryInput {
            base_url: "https://api.example.com/v1".into(),
            model: "test-model".into(),
            api_key: None, // 不碰凭据管理器
        }),
        backup: None,
    };
    let saved = core.set_model_config(patch).expect("保存应成功");
    assert_eq!(
        saved.primary.as_ref().map(|entry| entry.model.as_str()),
        Some("test-model")
    );
    // 配置落盘并可重新加载
    let config = visrecall_library::AppConfig::load_or_default(&dir.path().join("config.json"));
    assert_eq!(
        config.models.primary.as_ref().map(|e| e.base_url.as_str()),
        Some("https://api.example.com/v1")
    );
    // 热生效：注册表已经用新配置构造（主槽位存在）
    assert!(core.model_config().primary.is_some());
}

#[test]
fn model_config_rejects_invalid_base_url() {
    let dir = tempfile::tempdir().unwrap();
    let (core, _sink) = open(dir.path());
    let patch = ModelConfigPatch {
        primary: Some(ProviderEntryInput {
            base_url: "api.example.com".into(),
            model: "m".into(),
            api_key: None,
        }),
        backup: None,
    };
    let err = core.set_model_config(patch).expect_err("非法地址必须拒绝");
    assert!(matches!(err, visrecall_app_core::AppError::InvalidInput(_)));
    assert!(core.model_config().is_empty());
}

#[test]
fn clearing_a_slot_removes_it_from_config() {
    let dir = tempfile::tempdir().unwrap();
    let (core, _sink) = open(dir.path());
    core.set_model_config(ModelConfigPatch {
        primary: Some(ProviderEntryInput {
            base_url: "https://a.example.com/v1".into(),
            model: "m".into(),
            api_key: None,
        }),
        backup: None,
    })
    .unwrap();
    assert!(core.model_config().primary.is_some());

    // 地址与模型名都为空 = 清空槽位
    core.set_model_config(ModelConfigPatch {
        primary: Some(ProviderEntryInput::default()),
        backup: None,
    })
    .unwrap();
    assert!(core.model_config().is_empty());
}

#[test]
fn probe_requires_configured_slot() {
    let dir = tempfile::tempdir().unwrap();
    let (core, _sink) = open(dir.path());
    let err = core.probe_model("primary").expect_err("未配置必须报错");
    assert!(matches!(err, visrecall_app_core::AppError::ConfigMissing));
    let err = core.probe_model("nope").expect_err("未知槽位必须报错");
    assert!(matches!(err, visrecall_app_core::AppError::InvalidInput(_)));
}

/// 计数型 sink：验证事件数量（不改 RecordingSink 的语义）。
struct CountingSink {
    count: AtomicUsize,
}

impl EventSink for CountingSink {
    fn emit(&self, _event: AppEvent) {
        self.count.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn pipeline_status_events_reach_the_sink() {
    let dir = tempfile::tempdir().unwrap();
    let counting = Arc::new(CountingSink {
        count: AtomicUsize::new(0),
    });
    let core = AppCore::open(paths_in(dir.path()), counting.clone());
    core.start_pipeline().unwrap();

    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline && counting.count.load(Ordering::SeqCst) == 0 {
        std::thread::sleep(Duration::from_millis(20));
    }
    core.stop_pipeline();
    assert!(
        counting.count.load(Ordering::SeqCst) > 0,
        "启动后至少要有一条事件（pipeline_status 或 semantic_status）"
    );
}
