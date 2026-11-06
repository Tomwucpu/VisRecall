//! 存储位置迁移的集成测试（真实 SQLite + 临时库）。
//!
//! 覆盖两条出口的差异：成功时 config 改路径、闸门停在 `RestartPending`、运行时与
//! 热键恢复但捕获/拖放被 `BUSY` 拒绝；失败时 config 不动、闸门回 `Idle`、运行时与
//! 热键同样恢复。
//!
//! 每个用例给库配置**不同的**捕获热键：`cargo test` 在同一进程内并行跑用例，
//! `RegisterHotKey` 的占用是进程/系统级的，同值会让「热键已恢复」的断言互相干扰。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use rusqlite::OpenFlags;
use visrecall_app_core::capture_flow::{CaptureCoordinator, NoopWindowVisibility, OverlayPresenter};
use visrecall_app_core::events::{AppEvent, EventSink, RecordingSink};
use visrecall_app_core::{handle_shell_event, AppCore, CaptureGate};
use visrecall_capture::{FrameGrabber, RawFrame, WindowProbe};
use visrecall_domain::capture::{DisplayInfo, ForegroundInfo};
use visrecall_domain::drop::DropContent;
use visrecall_domain::item::{ItemKind, NewItem};
use visrecall_domain::{AppError, PhysicalPoint, WindowInfo};
use visrecall_library::paths::AppPaths;
use visrecall_library::{AppConfig, Library};
use visrecall_shell::{CaptureTrigger, ShellEvent};

// ---------------------------------------------------------------- fixture

/// 一个临时的「库 + config.json + 空的迁移目标」三件套。
struct Fixture {
    /// 只有它被持有，临时目录才会在用例结束时被删掉。
    _root: tempfile::TempDir,
    library_root: PathBuf,
    config_file: PathBuf,
    target: PathBuf,
    items: u64,
}

fn new_item(n: i64) -> NewItem {
    NewItem {
        kind: ItemKind::Image,
        captured_at: visrecall_domain::now_millis() - n,
        source_app: None,
        source_title: None,
        source_url: None,
        width: Some(10),
        height: Some(10),
        char_count: None,
        original_path: format!("originals/images/2026/11/{n}.png"),
        content_hash: format!("hash-{n}"),
    }
}

/// 建一个含 2 条素材的库，并把 config.json 指向它。
///
/// 库连接在这里建完就释放：`AppCore::open` 会再开自己的连接（WAL 下多连接无害，
/// 但用例里要避免出现一个「空转」的连接干扰 checkpoint 的忙判定）。
fn fixture(hotkey: &str) -> Fixture {
    let root = tempfile::tempdir().expect("temp dir");
    let library_root = root.path().join("library");
    let config_file = root.path().join("config.json");
    let target = root.path().join("target");
    std::fs::create_dir_all(&target).expect("target dir");

    {
        let library = Library::open(library_root.clone()).expect("open library");
        let originals = library_root.join("originals/images/2026/11");
        std::fs::create_dir_all(&originals).expect("originals dir");
        for n in 0..2 {
            // 落一个真实文件，让复制计划里除了元数据/主库还有内容目录。
            std::fs::write(originals.join(format!("{n}.png")), b"png-bytes").expect("write png");
            library.insert_item(new_item(n)).expect("insert item");
        }
    }

    let mut config = AppConfig {
        library_path: library_root.clone(),
        ..AppConfig::default()
    };
    config.hotkeys.capture = hotkey.to_string();
    config.save_atomic(&config_file).expect("write config");

    Fixture {
        _root: root,
        library_root,
        config_file,
        target,
        items: 2,
    }
}

fn paths(fixture: &Fixture) -> AppPaths {
    AppPaths {
        config_file: fixture.config_file.clone(),
        default_library_root: fixture.library_root.clone(),
    }
}

fn config_text(fixture: &Fixture) -> String {
    std::fs::read_to_string(&fixture.config_file).expect("read config")
}

fn dir_entries(path: &Path) -> usize {
    std::fs::read_dir(path)
        .map(|entries| entries.count())
        .unwrap_or(0)
}

fn failed_codes(sink: &RecordingSink) -> Vec<String> {
    sink.events()
        .into_iter()
        .filter_map(|event| match event {
            AppEvent::CaptureFailed(payload) => Some(payload.code),
            _ => None,
        })
        .collect()
}

fn progress_phases(sink: &RecordingSink) -> Vec<String> {
    sink.events()
        .into_iter()
        .filter_map(|event| match event {
            AppEvent::StorageMigrationProgress(payload) => Some(payload.phase),
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------- 捕获/导入的测试替身

/// 闸门必须先把请求挡掉：抓帧被调用就说明漏了检查。
struct ForbiddenGrabber;

impl FrameGrabber for ForbiddenGrabber {
    fn grab(&self, _displays: &[DisplayInfo]) -> Result<Vec<RawFrame>, AppError> {
        Err(AppError::Internal(
            "frame grabbing must not run while the gate is closed".into(),
        ))
    }
}

struct StubProbe;

impl WindowProbe for StubProbe {
    fn foreground(&self) -> Result<ForegroundInfo, AppError> {
        Err(AppError::Internal("probe must not run".into()))
    }

    fn enumerate(&self) -> Result<Vec<WindowInfo>, AppError> {
        Ok(Vec::new())
    }

    fn hit_chain(&self, _point: PhysicalPoint) -> Result<Vec<WindowInfo>, AppError> {
        Ok(Vec::new())
    }

    fn hit_chain_excluding(
        &self,
        _point: PhysicalPoint,
        _excluded_pid: u32,
    ) -> Result<Vec<WindowInfo>, AppError> {
        Ok(Vec::new())
    }
}

struct StubOverlay;

impl OverlayPresenter for StubOverlay {
    fn show(
        &self,
        _frames: Vec<RawFrame>,
        _displays: Vec<DisplayInfo>,
        _foreground: ForegroundInfo,
    ) -> Result<(), AppError> {
        Err(AppError::Internal("overlay must not run".into()))
    }
}

/// 组装一个「除了闸门什么都不可用」的捕获编排器。
fn coordinator_with_gate(core: &AppCore, sink: Arc<dyn EventSink>) -> CaptureCoordinator {
    let library = Arc::new(
        Library::open(core.library_root()).expect("reopen the library for the test coordinator"),
    );
    CaptureCoordinator::with_overlay(
        library,
        Arc::new(ForbiddenGrabber),
        Arc::new(StubProbe),
        Arc::new(StubOverlay),
        Arc::new(NoopWindowVisibility),
        sink,
    )
    .with_gate(core.gate_handle())
}

// ---------------------------------------------------------------- 失败出口

/// 复制中途失败：`config.json` 一个字都不改，闸门回 `Idle`，运行时与热键恢复。
///
/// 注入方式：收到第一个迁移进度事件时，把目标里的 `visrecall.db` 变成**目录**。
/// 这一步发生在目标校验之后（校验要求目标为空），因此 `copy_file` 的 `rename`
/// 必然失败——正好是「复制到一半出错」的真实形态，`library.json` 已经落到目标里。
#[test]
fn copy_failure_keeps_config_and_resets_the_gate() {
    let fixture = fixture("Ctrl+Alt+Shift+F6");
    let inner = Arc::new(RecordingSink::default());
    let sink: Arc<dyn EventSink> = Arc::new(HalfWayFailure {
        inner: Arc::clone(&inner),
        target: fixture.target.clone(),
        armed: AtomicBool::new(false),
    });
    let core = AppCore::open(paths(&fixture), sink);

    let before = config_text(&fixture);
    assert_eq!(core.gate(), CaptureGate::Idle);
    assert_eq!(core.library_root(), fixture.library_root);

    let err = core
        .relocate_library(fixture.target.clone())
        .expect_err("the copy must fail");
    assert!(
        matches!(err, AppError::Io(_)),
        "copy failure should surface as IO, got {err:?}"
    );

    // 1. 配置未改
    assert_eq!(config_text(&fixture), before);
    assert_eq!(core.config().library_path, fixture.library_root);
    // 2. 闸门回 Idle 且不再要求重启
    assert_eq!(core.gate(), CaptureGate::Idle);
    assert!(!core.restart_required());
    assert_eq!(core.library_root(), fixture.library_root);
    // 3. 运行时与热键都恢复（失败不能让用户失去悬浮球与快捷键）
    assert!(
        core.ball_present_for_test(),
        "floating ball must be restored after a failed relocation"
    );
    assert!(
        core.hotkey_spec_for_test().is_some(),
        "capture hotkey must be restored after a failed relocation"
    );
    // 4. 目标残留保留，且确实是复制中途失败的（library.json 已写入）
    assert!(
        fixture.target.join("library.json").exists(),
        "a partial copy is expected to stay behind"
    );
    // 5. 源库仍然可用
    assert_eq!(
        core.library().unwrap().count_items().unwrap(),
        fixture.items
    );
    assert!(!failed_codes(&inner).contains(&"BUSY".to_string()));
}

/// 在复制开始后把目标里的 `visrecall.db` 占成目录，制造 `rename` 失败。
struct HalfWayFailure {
    inner: Arc<RecordingSink>,
    target: PathBuf,
    armed: AtomicBool,
}

impl EventSink for HalfWayFailure {
    fn emit(&self, event: AppEvent) {
        if let AppEvent::StorageMigrationProgress(payload) = &event {
            if payload.phase == "prepare" && !self.armed.swap(true, Ordering::SeqCst) {
                std::fs::create_dir_all(self.target.join("visrecall.db"))
                    .expect("occupy the db path");
            }
        }
        self.inner.emit(event);
    }
}

// ---------------------------------------------------------------- 成功出口

#[test]
fn successful_relocation_switches_config_and_requires_restart() {
    let fixture = fixture("Ctrl+Alt+Shift+F7");
    let sink = Arc::new(RecordingSink::default());
    let core = AppCore::open(paths(&fixture), sink.clone());

    let result = core
        .relocate_library(fixture.target.clone())
        .expect("relocation must succeed");

    assert!(result.restart_required, "relocation never hot-switches");
    assert_eq!(PathBuf::from(&result.new_root), fixture.target);
    assert!(
        result.files_copied >= 3,
        "library.json + visrecall.db + one original, got {}",
        result.files_copied
    );

    // 闸门停在 RestartPending，新路径已经生效（`library_root` 跟着走）。
    assert_eq!(core.gate(), CaptureGate::RestartPending);
    assert!(core.restart_required());
    assert_eq!(core.config().library_path, fixture.target);
    assert_eq!(core.library_root(), fixture.target);

    // config.json 已落盘（重启后据此打开新库）。
    let saved = AppConfig::load_or_default(&fixture.config_file);
    assert_eq!(saved.library_path, fixture.target);

    // 源目录不删：迁移是「复制 + 校验」，失败/重启前旧库仍是权威副本。
    assert!(fixture.library_root.join("visrecall.db").exists());
    assert!(fixture.library_root.join("library.json").exists());
    // 目标内容齐全（含内容目录里的原图）。
    assert!(fixture.target.join("visrecall.db").exists());
    assert!(fixture.target.join("library.json").exists());
    assert!(fixture
        .target
        .join("originals/images/2026/11/0.png")
        .exists());

    // 进度事件顺序：prepare → copy → verify → done。
    let phases = progress_phases(&sink);
    assert_eq!(phases.first().map(String::as_str), Some("prepare"));
    assert_eq!(phases.last().map(String::as_str), Some("done"));
    assert!(phases.iter().any(|phase| phase == "copy"));
    assert!(phases.iter().any(|phase| phase == "verify"));

    // 运行时与热键恢复，但捕获被闸门拒绝（下两个用例验证）。
    assert!(core.ball_present_for_test());
    assert!(core.hotkey_spec_for_test().is_some());
}

#[test]
fn restart_picks_up_the_new_library() {
    let fixture = fixture("Ctrl+Alt+Shift+F8");
    {
        let core = AppCore::open(paths(&fixture), Arc::new(RecordingSink::default()));
        core.relocate_library(fixture.target.clone()).expect("relocate");
        assert!(core.restart_required());
    }

    // 模拟重启：同一份 config 重新构造，闸门应当是干净的 Idle。
    let core = AppCore::open(paths(&fixture), Arc::new(RecordingSink::default()));
    assert_eq!(core.gate(), CaptureGate::Idle);
    assert!(!core.restart_required());
    assert_eq!(core.library_root(), fixture.target);
    assert_eq!(
        core.library().unwrap().count_items().unwrap(),
        fixture.items
    );
}

// ---------------------------------------------------------------- 成功后写入口被拒

#[test]
fn capture_is_rejected_while_a_restart_is_pending() {
    let fixture = fixture("Ctrl+Alt+Shift+F9");
    let sink = Arc::new(RecordingSink::default());
    let core = AppCore::open(paths(&fixture), sink.clone());
    core.relocate_library(fixture.target.clone()).expect("relocate");

    let capture_sink = Arc::new(RecordingSink::default());
    let coordinator = coordinator_with_gate(&core, capture_sink.clone());
    let gate = core.gate_handle();
    let config = Mutex::new(core.config());
    let import = Arc::new(visrecall_app_core::ImportFlow::new(
        Arc::new(Library::open(fixture.library_root.clone()).unwrap()),
        Arc::new(StubProbe),
        capture_sink.clone(),
    ));

    handle_shell_event(
        &coordinator,
        Arc::clone(&import),
        &gate,
        &config,
        &fixture.config_file,
        ShellEvent::CaptureRequest(CaptureTrigger::Hotkey),
    );

    // 捕获循环的分发处挡住了：没有素材产生，错误码是 BUSY。
    assert_eq!(
        core.library().unwrap().count_items().unwrap(),
        fixture.items
    );
    assert_eq!(failed_codes(&capture_sink), vec!["BUSY".to_string()]);

    // 纵深防御：绕过循环直接调捕获入口，同样被拒绝。
    let direct = coordinator.start(CaptureTrigger::UiButton);
    assert!(
        matches!(direct, Err(AppError::Busy(_))),
        "preflight must reject capture too, got {direct:?}"
    );
}

#[test]
fn drop_is_rejected_while_a_restart_is_pending() {
    let fixture = fixture("Ctrl+Alt+Shift+F10");
    let sink = Arc::new(RecordingSink::default());
    let core = AppCore::open(paths(&fixture), sink.clone());
    core.relocate_library(fixture.target.clone()).expect("relocate");

    let capture_sink = Arc::new(RecordingSink::default());
    let coordinator = coordinator_with_gate(&core, capture_sink.clone());
    let gate = core.gate_handle();
    let config = Mutex::new(core.config());
    let import = Arc::new(visrecall_app_core::ImportFlow::new(
        Arc::new(Library::open(fixture.library_root.clone()).unwrap()),
        Arc::new(StubProbe),
        capture_sink.clone(),
    ));

    handle_shell_event(
        &coordinator,
        Arc::clone(&import),
        &gate,
        &config,
        &fixture.config_file,
        ShellEvent::Drop(DropContent::Text {
            plain: "拖进来的文字".into(),
            html: None,
        }),
    );

    // 只查 preflight 的漏洞就在这里暴露：Drop 不经过 `start`，必须在分发处挡。
    assert_eq!(
        core.library().unwrap().count_items().unwrap(),
        fixture.items
    );
    assert_eq!(failed_codes(&capture_sink), vec!["BUSY".to_string()]);
}

// ---------------------------------------------------------------- checkpoint

/// 另有读者持快照时 `wal_checkpoint(TRUNCATE)` 返回 busy：迁移必须在复制前中止。
///
/// 构造要点：WAL 里必须先有**未 checkpoint 的帧**，读者的快照才落在 WAL 上；
/// 否则读者只读主库文件，`TRUNCATE` 照样能完成（`busy == 0`）。因此先用
/// `setting_set` 造两帧，读者读第一帧后再写一帧，把 WAL 撑开。
#[test]
fn blocked_checkpoint_aborts_before_copying() {
    let fixture = fixture("Ctrl+Alt+Shift+F11");
    let core = AppCore::open(paths(&fixture), Arc::new(RecordingSink::default()));

    {
        let library = core.library().unwrap();
        library
            .setting_set("relocate.test.marker", &1u32)
            .expect("write a wal frame");
    }

    let db = fixture.library_root.join("visrecall.db");
    let reader = rusqlite::Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("open a second reader");
    reader
        .execute_batch("BEGIN; SELECT COUNT(*) FROM items;")
        .expect("hold a read snapshot");

    {
        let library = core.library().unwrap();
        library
            .setting_set("relocate.test.marker", &2u32)
            .expect("write another wal frame");
    }

    let before = config_text(&fixture);
    let err = core
        .relocate_library(fixture.target.clone())
        .expect_err("checkpoint must report busy");

    // 提前结束快照，避免影响临时目录清理。
    reader.execute_batch("ROLLBACK").ok();

    assert!(
        matches!(err, AppError::Busy(_)),
        "a blocked checkpoint must surface as BUSY, got {err:?}"
    );
    // 不复制：目标里一个文件都不该有。
    assert_eq!(dir_entries(&fixture.target), 0);
    assert_eq!(config_text(&fixture), before);
    assert_eq!(core.gate(), CaptureGate::Idle);
    assert!(!core.restart_required());
    assert!(core.ball_present_for_test());
    assert!(core.hotkey_spec_for_test().is_some());
}

#[test]
fn a_second_relocation_is_rejected_while_the_gate_is_busy() {
    let fixture = fixture("Ctrl+Alt+Shift+F12");
    let core = AppCore::open(paths(&fixture), Arc::new(RecordingSink::default()));

    core.set_gate(CaptureGate::Relocating);
    let err = core
        .relocate_library(fixture.target.clone())
        .expect_err("the gate is already taken");

    assert!(matches!(err, AppError::Busy(_)), "got {err:?}");
    assert_eq!(dir_entries(&fixture.target), 0);
    assert_eq!(core.gate(), CaptureGate::Relocating);
    core.set_gate(CaptureGate::Idle);
}
