//! Task 10：pipeline 宿主、模型配置热生效、语义就绪与事件接线。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use visrecall_app_core::pipeline_host::{
    enqueue_initial_indexing, EventBridge, ProviderRegistry, RoutedText,
};
use visrecall_app_core::{
    AppCore, AppError, AppEvent, AppPaths, EventSink, ModelConfigPatch, ModelEntryInput,
    RecordingSink,
};
use visrecall_domain::{ItemKind, NewItem};
use visrecall_library::{AppConfig, Library, ModelConfig, ModelEntry};
use visrecall_pipeline::{PipelineEvents, PRIORITY_NEW, STAGE_INDEX, STAGE_OCR, STAGE_VLM};

fn paths_in(dir: &std::path::Path) -> AppPaths {
    AppPaths {
        config_file: dir.join("config.json"),
        default_library_root: dir.join("library"),
    }
}

/// 只关心事件到达、不关心热键的打开方式。
fn open(dir: &std::path::Path) -> (AppCore, Arc<RecordingSink>) {
    let paths = paths_in(dir);
    // 测试库必须位于临时目录，避免误用真实用户库。
    AppConfig {
        library_path: paths.default_library_root.clone(),
        ..AppConfig::default()
    }
    .save_atomic(&paths.config_file)
    .expect("write test config");
    let sink = Arc::new(RecordingSink::default());
    let core = AppCore::open(paths, sink.clone());
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

/// 文本是否进阶段二由「上传云端解析」开关决定（默认关闭）；图片不受影响。
#[test]
fn initial_indexing_enqueues_vlm_for_text_only_when_cloud_text_ai_is_enabled() {
    let dir = tempfile::tempdir().unwrap();
    let library = Library::open(dir.path().join("library")).unwrap();
    let off = insert_item(&library, ItemKind::Text, "off");
    enqueue_initial_indexing(&library, &off);
    assert_eq!(
        library.jobs_for_stage(&off.id, STAGE_INDEX).unwrap().len(),
        1
    );
    assert!(
        library
            .jobs_for_stage(&off.id, STAGE_VLM)
            .unwrap()
            .is_empty(),
        "默认关闭时文本不得排队上云"
    );

    library.set_cloud_text_ai(true).unwrap();
    let on = insert_item(&library, ItemKind::Text, "on");
    enqueue_initial_indexing(&library, &on);
    assert_eq!(
        library.jobs_for_stage(&on.id, STAGE_INDEX).unwrap().len(),
        1
    );
    assert_eq!(
        library.jobs_for_stage(&on.id, STAGE_VLM).unwrap().len(),
        1,
        "开启后文本要进阶段二（摘要 + 自动标签）"
    );

    // 图片仍只入队 ocr：vlm 由 ocr handler 接力，不受该开关影响
    let image = insert_item(&library, ItemKind::Image, "img");
    enqueue_initial_indexing(&library, &image);
    assert_eq!(
        library.jobs_for_stage(&image.id, STAGE_OCR).unwrap().len(),
        1
    );
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
        title: None,
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

fn model_input(id: &str, name: &str) -> ModelEntryInput {
    ModelEntryInput {
        id: id.into(),
        name: name.into(),
        model: "test-model".into(),
        base_url: "https://api.example.com/v1".into(),
        protocol: "openai".into(),
        protocol_id: String::new(),
        api_key: None, // 不碰凭据管理器
    }
}

#[test]
fn model_config_set_persists_and_hot_reloads() {
    let dir = tempfile::tempdir().unwrap();
    let (core, _sink) = open(dir.path());
    assert!(core.model_config().is_empty());

    let patch = ModelConfigPatch {
        entries: vec![model_input("m1", "我的视觉模型")],
        active_id: Some("m1".into()),
    };
    let saved = core.set_model_config(patch).expect("保存应成功");
    assert_eq!(saved.entries[0].model, "test-model");
    assert_eq!(saved.entries[0].name, "我的视觉模型");
    assert_eq!(saved.active_id.as_deref(), Some("m1"));
    // 配置落盘并可重新加载
    let config = visrecall_library::AppConfig::load_or_default(&dir.path().join("config.json"));
    assert_eq!(
        config.models.entries[0].base_url,
        "https://api.example.com/v1"
    );
    assert_eq!(config.models.active_id.as_deref(), Some("m1"));
    // 热生效：注册表已经用新配置构造
    assert_eq!(core.model_config().entries.len(), 1);
}

#[test]
fn model_config_rejects_invalid_base_url() {
    let dir = tempfile::tempdir().unwrap();
    let (core, _sink) = open(dir.path());
    let patch = ModelConfigPatch {
        entries: vec![ModelEntryInput {
            base_url: "api.example.com".into(),
            ..model_input("m1", "坏地址")
        }],
        active_id: None,
    };
    let err = core.set_model_config(patch).expect_err("非法地址必须拒绝");
    assert!(matches!(err, visrecall_app_core::AppError::InvalidInput(_)));
    assert!(core.model_config().is_empty());
}

#[test]
fn model_config_rejects_unknown_protocol_and_missing_rest_id() {
    let dir = tempfile::tempdir().unwrap();
    let (core, _sink) = open(dir.path());
    let unknown = ModelConfigPatch {
        entries: vec![ModelEntryInput {
            protocol: "grpc".into(),
            ..model_input("m1", "未知协议")
        }],
        active_id: None,
    };
    assert!(matches!(
        core.set_model_config(unknown),
        Err(visrecall_app_core::AppError::InvalidInput(_))
    ));
    let rest = ModelConfigPatch {
        entries: vec![ModelEntryInput {
            protocol: "rest".into(),
            ..model_input("m1", "自定义 REST")
        }],
        active_id: None,
    };
    assert!(matches!(
        core.set_model_config(rest),
        Err(visrecall_app_core::AppError::InvalidInput(_))
    ));
    assert!(core.model_config().is_empty());
}

#[test]
fn deleting_all_models_clears_the_config() {
    let dir = tempfile::tempdir().unwrap();
    let (core, _sink) = open(dir.path());
    core.set_model_config(ModelConfigPatch {
        entries: vec![model_input("m1", "视觉模型")],
        active_id: Some("m1".into()),
    })
    .unwrap();
    assert_eq!(core.model_config().entries.len(), 1);

    core.set_model_config(ModelConfigPatch::default()).unwrap();
    assert!(core.model_config().is_empty());
    assert_eq!(core.model_config().active_id, None);
}

#[test]
fn selecting_a_model_swaps_the_active_provider() {
    let dir = tempfile::tempdir().unwrap();
    let (core, _sink) = open(dir.path());
    core.set_model_config(ModelConfigPatch {
        entries: vec![model_input("m1", "甲"), model_input("m2", "乙")],
        active_id: Some("m1".into()),
    })
    .unwrap();
    assert_eq!(
        core.model_config().active_entry().map(|e| e.id.as_str()),
        Some("m1")
    );

    // 切换选用：只换 active 指针，没有「备用模型」概念
    core.set_model_config(ModelConfigPatch {
        entries: vec![model_input("m1", "甲"), model_input("m2", "乙")],
        active_id: Some("m2".into()),
    })
    .unwrap();
    assert_eq!(
        core.model_config().active_entry().map(|e| e.id.as_str()),
        Some("m2")
    );
}

fn model_entry(id: &str, model: &str) -> ModelEntry {
    ModelEntry {
        id: id.into(),
        name: id.into(),
        model: model.into(),
        base_url: "https://api.example.com/v1".into(),
        protocol: "openai".into(),
        protocol_id: String::new(),
    }
}

#[test]
fn registry_reload_swaps_the_active_provider_in_place() {
    let config = ModelConfig {
        entries: vec![model_entry("m1", "model-a"), model_entry("m2", "model-b")],
        active_id: Some("m1".into()),
        ..ModelConfig::default()
    };
    let registry = ProviderRegistry::from_config(&config);
    assert_eq!(registry.active().unwrap().model(), "model-a");
    assert_eq!(registry.active_slot(), "vlm-primary");

    let mut next = config;
    next.active_id = Some("m2".into());
    registry.reload(&next);
    assert_eq!(registry.active().unwrap().model(), "model-b");
    assert_eq!(registry.active_slot(), "vlm-primary");
}

/// 文本 Provider 的「是否有可用模型」跟随注册表：未配置即不排队、不标待重试。
#[test]
fn routed_text_ready_follows_the_registry() {
    use visrecall_domain::providers::TextProvider;

    let text = RoutedText::new(ProviderRegistry::from_config(&ModelConfig::default()));
    assert!(!text.ready(), "未配置模型时文本 Provider 不可用");

    let config = ModelConfig {
        entries: vec![model_entry("m1", "model-a")],
        active_id: Some("m1".into()),
        ..ModelConfig::default()
    };
    let text = RoutedText::new(ProviderRegistry::from_config(&config));
    assert!(text.ready(), "配置了模型即可用");
}

fn sample_job(stage: &str) -> visrecall_library::Job {
    visrecall_library::Job {
        id: "job-1".into(),
        item_id: "item-1".into(),
        stage: stage.into(),
        revision: 0,
        state: visrecall_library::JobState::Running,
        priority: 0,
        attempts: 1,
        next_run_at: None,
        last_error: None,
        created_at: 0,
        updated_at: 0,
    }
}

fn bridge_with(sink: &Arc<RecordingSink>, registry: Arc<ProviderRegistry>) -> EventBridge {
    EventBridge::new(Arc::clone(sink) as Arc<dyn EventSink>, registry)
}

#[test]
fn job_failed_reaches_the_sink_when_model_configured() {
    let sink = Arc::new(RecordingSink::default());
    let config = ModelConfig {
        entries: vec![model_entry("m1", "model-a")],
        active_id: Some("m1".into()),
        ..ModelConfig::default()
    };
    let bridge = bridge_with(&sink, ProviderRegistry::from_config(&config));

    bridge.job_failed(
        &sample_job(STAGE_VLM),
        &AppError::Provider("timeout after 30s".into()),
    );

    let events = sink.events();
    assert_eq!(events.len(), 1);
    match &events[0] {
        AppEvent::JobFailed(payload) => {
            assert_eq!(payload.item_id, "item-1");
            assert_eq!(payload.stage, "vlm");
            assert_eq!(payload.code, "PROVIDER");
            // 细节消息去掉 Display 前缀，语境由前端文案组装
            assert_eq!(payload.message, "timeout after 30s");
            assert_eq!(payload.model.as_deref(), Some("model-a"));
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn job_failed_is_silent_for_vlm_when_no_model_configured() {
    let sink = Arc::new(RecordingSink::default());
    let bridge = bridge_with(
        &sink,
        ProviderRegistry::from_config(&ModelConfig::default()),
    );

    // 未配置模型是预期状态（UI 已注明「仅本地 OCR」），vlm 失败不打扰用户
    bridge.job_failed(
        &sample_job(STAGE_VLM),
        &AppError::ProviderConfig("no vision provider configured".into()),
    );
    assert!(sink.events().is_empty());

    // 非云端阶段（如 index 嵌入失败）与模型配置无关，仍要提示
    bridge.job_failed(
        &sample_job(STAGE_INDEX),
        &AppError::ModelNotReady("bge".into()),
    );
    let events = sink.events();
    assert_eq!(events.len(), 1);
    match &events[0] {
        AppEvent::JobFailed(payload) => {
            assert_eq!(payload.stage, "index");
            assert_eq!(payload.code, "MODEL_NOT_READY");
            assert_eq!(payload.model, None);
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn probe_requires_a_saved_model() {
    let dir = tempfile::tempdir().unwrap();
    let (core, _sink) = open(dir.path());
    let err = core.probe_model("m1").expect_err("未保存必须报错");
    assert!(matches!(err, visrecall_app_core::AppError::ConfigMissing));
}

#[test]
fn reparse_image_marks_parsing_and_enqueues_ocr_at_new_revision() {
    let dir = tempfile::tempdir().unwrap();
    let (core, sink) = open(dir.path());
    let library = core.library().expect("library");
    let image = insert_item(library, ItemKind::Image, "img");
    library
        .update_status(&image.id, visrecall_domain::ItemStatus::Ready, None)
        .unwrap();

    core.reparse_item(&image.id).expect("reparse 应成功");

    // 状态回 parsing，卡片立即出现「解析中」
    let reloaded = library.get_item(&image.id).unwrap().unwrap();
    assert_eq!(reloaded.status, visrecall_domain::ItemStatus::Parsing);
    // bump revision + 入队 ocr（index/vlm 由 ocr handler 接力）
    let jobs = library.jobs_for_stage(&image.id, STAGE_OCR).unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].revision, reloaded.revision);
    assert_eq!(jobs[0].state, visrecall_library::JobState::Queued);
    // 前端列表刷新事件
    assert!(sink.events().iter().any(|event| matches!(
        event,
        AppEvent::ItemUpdated(payload)
            if payload.id == image.id && payload.fields == ["status"]
    )));
}

#[test]
fn reparse_text_enqueues_index_and_vlm_at_same_revision() {
    let dir = tempfile::tempdir().unwrap();
    let (core, _sink) = open(dir.path());
    let library = core.library().expect("library");
    // 文本重新解析要上云，必须先打开「上传云端解析」。
    core.set_cloud_text_ai(true).unwrap();
    let text = insert_item(library, ItemKind::Text, "txt");

    core.reparse_item(&text.id).expect("reparse 应成功");

    let reloaded = library.get_item(&text.id).unwrap().unwrap();
    assert_eq!(reloaded.status, visrecall_domain::ItemStatus::Parsing);
    // 文本没有 ocr 前置，index 也不接力 vlm：两个阶段都要显式入队
    assert!(library
        .jobs_for_stage(&text.id, STAGE_OCR)
        .unwrap()
        .is_empty());
    for stage in [STAGE_INDEX, STAGE_VLM] {
        let jobs = library.jobs_for_stage(&text.id, stage).unwrap();
        assert_eq!(jobs.len(), 1, "{stage} 应恰好一条任务");
        assert_eq!(jobs[0].revision, reloaded.revision);
        assert_eq!(jobs[0].state, visrecall_library::JobState::Queued);
    }
}

/// 开关关闭时点「重新解析」：明确告知去设置里开启，且不改状态、不排任务。
#[test]
fn reparse_text_is_rejected_while_cloud_text_ai_is_disabled() {
    let dir = tempfile::tempdir().unwrap();
    let (core, _sink) = open(dir.path());
    let library = core.library().expect("library");
    let text = insert_item(library, ItemKind::Text, "txt-off");
    library
        .update_status(&text.id, visrecall_domain::ItemStatus::Ready, None)
        .unwrap();

    let err = core.reparse_item(&text.id).expect_err("关闭时应明确拒绝");
    assert!(
        matches!(err, visrecall_app_core::AppError::InvalidInput(_)),
        "err={err:?}"
    );
    let message = err.to_string();
    assert!(message.contains("设置"), "提示要指向设置页：{message}");

    let reloaded = library.get_item(&text.id).unwrap().unwrap();
    assert_eq!(
        reloaded.status,
        visrecall_domain::ItemStatus::Ready,
        "拒绝时不能把素材卡在解析中"
    );
    for stage in [STAGE_INDEX, STAGE_VLM] {
        assert!(
            library.jobs_for_stage(&text.id, stage).unwrap().is_empty(),
            "{stage} 不得入队"
        );
    }

    // 图片重新解析不受该开关约束（保持现状）
    let image = insert_item(library, ItemKind::Image, "img-off");
    core.reparse_item(&image.id).expect("图片 reparse 应成功");
}

#[test]
fn reparse_missing_item_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let (core, _sink) = open(dir.path());
    let err = core.reparse_item("ghost").expect_err("不存在必须报错");
    assert!(matches!(err, visrecall_app_core::AppError::NotFound));
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
    let paths = paths_in(dir.path());
    // 与其他宿主测试一致显式绑定临时库，避免默认配置指向真实用户目录。
    AppConfig {
        library_path: paths.default_library_root.clone(),
        ..AppConfig::default()
    }
    .save_atomic(&paths.config_file)
    .expect("write test config");
    let counting = Arc::new(CountingSink {
        count: AtomicUsize::new(0),
    });
    let core = AppCore::open(paths, counting.clone());
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
