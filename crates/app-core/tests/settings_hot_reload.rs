//! 设置热生效的集成测试：热键重载顺序、占用拒绝、尺寸三档与落盘失败。
//!
//! 热键不用 mock：`RegisterHotKey` 是线程级的，在测试进程里用第二个线程占用目标组合
//! 就是最接近生产的「被占用」模拟（design §9.2）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use visrecall_app_core::events::{AppEvent, RecordingSink};
use visrecall_app_core::{AppCore, AppPaths, SettingsPatchInput};
use visrecall_domain::AppError;
use visrecall_library::AppConfig;
use visrecall_shell::{spawn_hotkey_thread, HotkeyHandle, HotkeySpec};

fn paths_in(dir: &std::path::Path) -> AppPaths {
    AppPaths {
        config_file: dir.join("config.json"),
        default_library_root: dir.join("library"),
    }
}

fn open(dir: &std::path::Path) -> (AppCore, Arc<RecordingSink>) {
    // 默认的 Ctrl+Shift+S 在开发机上常被截图工具占用，测试统一换成本机几乎不会用到的组合。
    open_with_hotkey(dir, "Ctrl+Alt+Shift+F6")
}

fn open_with_hotkey(dir: &std::path::Path, hotkey: &str) -> (AppCore, Arc<RecordingSink>) {
    let paths = paths_in(dir);
    let config = AppConfig {
        library_path: paths.default_library_root.clone(),
        hotkeys: visrecall_app_core::AppConfig::default().hotkeys,
        ..AppConfig::default()
    };
    let mut config = config;
    config.hotkeys.capture = hotkey.to_string();
    config.save_atomic(&paths.config_file).unwrap();
    let sink = Arc::new(RecordingSink::default());
    (AppCore::open(paths, sink.clone()), sink)
}

/// 用一个独立线程占用组合键，直到 drop。
struct OccupiedHotkey {
    handle: Option<HotkeyHandle>,
    stop: Arc<AtomicBool>,
}

impl OccupiedHotkey {
    fn occupy(combo: &str) -> OccupiedHotkey {
        let spec = HotkeySpec::parse(combo).unwrap();
        let (tx, rx) = crossbeam_channel::unbounded();
        let handle = loop {
            // 另一个线程刚释放时可能短暂占用，重试几次再放弃。
            match spawn_hotkey_thread(spec, tx.clone()) {
                Ok(handle) => break handle,
                Err(_) => std::thread::sleep(Duration::from_millis(50)),
            }
        };
        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = Arc::clone(&stop);
        std::thread::spawn(move || {
            while !stop_flag.load(Ordering::SeqCst) {
                let _ = rx.recv_timeout(Duration::from_millis(50));
            }
        });
        OccupiedHotkey {
            handle: Some(handle),
            stop,
        }
    }
}

impl Drop for OccupiedHotkey {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.stop();
        }
        self.stop.store(true, Ordering::SeqCst);
        // 让系统完成注销，避免下一个用例撞上「刚被占用」。
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn invalid_hotkey_text_is_rejected_before_touching_the_disk() {
    let dir = tempfile::tempdir().unwrap();
    let (core, _sink) = open(dir.path());
    let before = std::fs::metadata(&core.paths().config_file)
        .unwrap()
        .modified()
        .unwrap();

    let err = core
        .update_settings(SettingsPatchInput {
            capture_hotkey: Some("Ctrl+".into()),
            ..SettingsPatchInput::default()
        })
        .unwrap_err();
    assert!(matches!(err, AppError::InvalidInput(_)), "{err:?}");

    let after = std::fs::metadata(&core.paths().config_file)
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(after, before, "config must not be rewritten");
    assert_eq!(core.config().hotkeys.capture, "Ctrl+Alt+Shift+F6");
}

#[test]
fn ball_size_accepts_the_supported_range() {
    let dir = tempfile::tempdir().unwrap();
    let (core, _sink) = open(dir.path());

    for size in [49u32, 151] {
        let err = core
            .update_settings(SettingsPatchInput {
                ball_size: Some(size),
                ..SettingsPatchInput::default()
            })
            .unwrap_err();
        assert!(matches!(err, AppError::InvalidInput(_)));
    }

    for size in [50u32, 100, 150] {
        let updated = core
            .update_settings(SettingsPatchInput {
                ball_size: Some(size),
                ..SettingsPatchInput::default()
            })
            .unwrap();
        assert_eq!(updated.ball.size, size);
    }
}

#[test]
fn hotkey_change_applies_immediately_and_emits_restored() {
    let dir = tempfile::tempdir().unwrap();
    let (core, sink) = open(dir.path());
    // 先按当前配置起一个热键线程，模拟正常运行态。
    core.start_hotkey().expect("initial hotkey");
    let original = core.hotkey_spec_for_test().expect("hotkey thread");

    // 选一个几乎不可能被占用的组合。
    let updated = core
        .update_settings(SettingsPatchInput {
            capture_hotkey: Some("Ctrl+Alt+Shift+F9".into()),
            ..SettingsPatchInput::default()
        })
        .expect("hotkey reload");

    // 落盘用 `HotkeySpec::format()` 的规范顺序，避免大小写/顺序来回跳。
    assert_eq!(
        updated.hotkeys.capture,
        HotkeySpec::parse("Ctrl+Alt+Shift+F9").unwrap().format()
    );
    let current = core.hotkey_spec_for_test().expect("hotkey thread");
    assert_ne!(current, original);
    assert_eq!(current, HotkeySpec::parse("Ctrl+Alt+Shift+F9").unwrap());

    // 托盘冲突提示要被清掉。
    assert!(sink
        .events()
        .iter()
        .any(|event| matches!(event, AppEvent::HotkeyRestored)));
}

#[test]
fn occupied_hotkey_is_rejected_and_the_old_one_survives() {
    let dir = tempfile::tempdir().unwrap();
    let (core, _sink) = open_with_hotkey(dir.path(), "Ctrl+Alt+Shift+F5");
    core.start_hotkey().expect("initial hotkey");
    let original = core.hotkey_spec_for_test().expect("hotkey thread");

    let occupied = OccupiedHotkey::occupy("Ctrl+Alt+Shift+F8");
    let err = core
        .update_settings(SettingsPatchInput {
            capture_hotkey: Some("Ctrl+Alt+Shift+F8".into()),
            ..SettingsPatchInput::default()
        })
        .unwrap_err();
    assert!(matches!(err, AppError::InvalidInput(_)), "{err:?}");

    // 配置未改（保留原始写法）、旧句柄仍在。
    assert_eq!(core.config().hotkeys.capture, "Ctrl+Alt+Shift+F5");
    assert_eq!(
        core.hotkey_spec_for_test().expect("hotkey thread"),
        original
    );
    let reloaded = AppConfig::load_or_default(&core.paths().config_file);
    assert_eq!(reloaded.hotkeys.capture, "Ctrl+Alt+Shift+F5");
    drop(occupied);
}

/// 同值不重注册：先用另一个线程占用「当前组合」，再提交相同值时必须成功——
/// 实现若重新注册，这一步必然失败。
#[test]
fn submitting_the_same_hotkey_does_not_re_register() {
    let dir = tempfile::tempdir().unwrap();
    let (core, _sink) = open_with_hotkey(dir.path(), "Ctrl+Alt+Shift+F4");
    let combo = core.config().hotkeys.capture;

    let occupied = OccupiedHotkey::occupy(&combo);
    let updated = core
        .update_settings(SettingsPatchInput {
            capture_hotkey: Some(combo.clone()),
            ball_size: Some(56),
            ..SettingsPatchInput::default()
        })
        .expect("same hotkey must not be re-registered");
    assert_eq!(
        updated.hotkeys.capture,
        HotkeySpec::parse(&combo).unwrap().format()
    );
    drop(occupied);
}

#[test]
fn config_write_failure_keeps_memory_values() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths_in(dir.path());
    let config = AppConfig {
        library_path: paths.default_library_root.clone(),
        ..AppConfig::default()
    };
    config.save_atomic(&paths.config_file).unwrap();

    // 把配置文件路径换成目录：`write_atomic` 的 rename 必然失败。
    std::fs::remove_file(&paths.config_file).unwrap();
    std::fs::create_dir(&paths.config_file).unwrap();

    let core = AppCore::open(paths, Arc::new(RecordingSink::default()));
    let before = core.config();

    let err = core
        .update_settings(SettingsPatchInput {
            ball_enabled: Some(!before.ball.enabled),
            ..SettingsPatchInput::default()
        })
        .unwrap_err();
    assert!(matches!(err, AppError::Io(_)), "{err:?}");
    assert_eq!(
        core.config().ball,
        before.ball,
        "memory must stay unchanged"
    );
}

/// 尺寸与开关改动会下发到悬浮球线程；这里只断言调用链不 panic 且配置生效
/// （真实窗口行为在手工验收里看）。
#[test]
fn ball_settings_changes_are_persisted() {
    let dir = tempfile::tempdir().unwrap();
    let (core, _sink) = open(dir.path());

    let updated = core
        .update_settings(SettingsPatchInput {
            ball_size: Some(50),
            ball_edge_hide: Some(false),
            hide_in_fullscreen: Some(false),
            ..SettingsPatchInput::default()
        })
        .unwrap();

    assert_eq!(updated.ball.size, 50);
    assert!(!updated.ball.edge_hide);
    assert!(!updated.ball.hide_in_fullscreen);

    let reloaded = AppConfig::load_or_default(&core.paths().config_file);
    assert_eq!(reloaded.ball.size, 50);
    assert!(!reloaded.ball.edge_hide);
    assert!(!reloaded.ball.hide_in_fullscreen);
}

/// 热键线程未启动时改键只写配置：下次 `start_hotkey` 才真正注册。
#[test]
fn hotkey_change_without_running_thread_only_updates_config() {
    let dir = tempfile::tempdir().unwrap();
    let (core, _sink) = open(dir.path());
    assert!(core.hotkey_spec_for_test().is_none());

    let start = Instant::now();
    let updated = core
        .update_settings(SettingsPatchInput {
            capture_hotkey: Some("Ctrl+Shift+F7".into()),
            ..SettingsPatchInput::default()
        })
        .unwrap();
    assert!(start.elapsed() < Duration::from_secs(2));
    assert_eq!(
        updated.hotkeys.capture,
        HotkeySpec::parse("Ctrl+Shift+F7").unwrap().format()
    );
    assert!(core.hotkey_spec_for_test().is_none());
}
