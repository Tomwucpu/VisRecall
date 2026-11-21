//! 阶段 01 Task 3：路径解析、库元数据与配置读写。
//!
//! 所有测试都只在 `tempfile::TempDir` 内落盘，绝不触碰用户目录。

use std::fs;
use std::path::{Path, PathBuf};

use visrecall_library::config::{
    AppConfig, BallConfig, CaptureConfig, StartupMode, BALL_MAX_SIZE, BALL_MIN_SIZE,
};
use visrecall_library::paths::{ensure_library_root, AppPaths, LibraryMeta};

fn backup_names(dir: &Path, prefix: &str) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .filter_map(|entry| {
            let name = entry.unwrap().file_name().into_string().unwrap();
            name.starts_with(prefix).then_some(name)
        })
        .collect();
    names.sort();
    names
}

#[test]
fn creates_library_metadata_once() {
    let dir = tempfile::tempdir().unwrap();
    let meta = ensure_library_root(dir.path()).unwrap();
    assert_eq!(meta.format_version, 1);
    assert!(!meta.library_id.is_empty());
    assert!(dir.path().join("library.json").exists());

    let again = ensure_library_root(dir.path()).unwrap();
    assert_eq!(meta.library_id, again.library_id);
    assert!(backup_names(dir.path(), "library.json.bad-").is_empty());
}

#[test]
fn creates_library_subdirectories() {
    let dir = tempfile::tempdir().unwrap();
    ensure_library_root(dir.path()).unwrap();
    for rel in ["originals/images", "texts", "thumbs", "tmp", "backups"] {
        assert!(dir.path().join(rel).is_dir(), "missing directory {rel}");
    }
}

#[test]
fn rejects_newer_format_version() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("library.json"),
        r#"{"library_id":"x","format_version":99,"created_at":0}"#,
    )
    .unwrap();
    let err = ensure_library_root(dir.path()).unwrap_err();
    assert!(format!("{err}").contains("format"));
}

#[test]
fn rebuilds_corrupt_library_metadata_with_backup() {
    let dir = tempfile::tempdir().unwrap();
    let meta_path = dir.path().join("library.json");
    fs::write(&meta_path, r#"{"format_version":1,"created_at":0}"#).unwrap();

    let meta = ensure_library_root(dir.path()).unwrap();
    assert_eq!(meta.format_version, 1);
    assert!(!meta.library_id.is_empty());

    let backups = backup_names(dir.path(), "library.json.bad-");
    assert_eq!(backups.len(), 1, "expected one backup, got {backups:?}");
    let raw = fs::read_to_string(&meta_path).unwrap();
    let stored: LibraryMeta = serde_json::from_str(&raw).unwrap();
    assert_eq!(stored.library_id, meta.library_id);
}

#[test]
fn empty_library_id_is_treated_as_corrupt() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("library.json"),
        r#"{"library_id":"  ","format_version":1,"created_at":0}"#,
    )
    .unwrap();

    let meta = ensure_library_root(dir.path()).unwrap();
    assert!(!meta.library_id.trim().is_empty());
    assert_eq!(backup_names(dir.path(), "library.json.bad-").len(), 1);
}

#[test]
fn config_roundtrips_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let cfg = AppConfig::default();
    cfg.save_atomic(&path).unwrap();
    let loaded = AppConfig::load_or_default(&path);
    assert_eq!(loaded.hotkeys.capture, "Ctrl+Shift+S");
    assert_eq!(loaded, cfg);
}

#[test]
fn config_roundtrips_custom_values() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let mut cfg = AppConfig {
        ball: BallConfig {
            size: 64,
            ..BallConfig::default()
        },
        capture: CaptureConfig {
            realtime_fallback: false,
        },
        ..AppConfig::default()
    };
    cfg.ball.positions.insert("display-1".into(), (0.25, 0.75));
    cfg.save_atomic(&path).unwrap();

    let loaded = AppConfig::load_or_default(&path);
    assert_eq!(loaded, cfg);
    assert_eq!(loaded.ball.positions["display-1"], (0.25, 0.75));
}

#[test]
fn startup_modes_roundtrip_with_stable_json_values() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");

    for (startup_mode, json_value) in [
        (StartupMode::Silent, "silent"),
        (StartupMode::ShowWindow, "show_window"),
    ] {
        let cfg = AppConfig {
            startup_mode,
            ..AppConfig::default()
        };
        cfg.save_atomic(&path).unwrap();
        let stored: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(stored["startup_mode"], json_value);
        assert_eq!(AppConfig::load_or_default(&path), cfg);
    }
}

#[test]
fn invalid_startup_mode_preserves_other_config_fields() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let custom_library = dir.path().join("自定义素材库");
    for invalid in [
        serde_json::json!("unknown"),
        serde_json::json!(false),
        serde_json::Value::Null,
    ] {
        let json = serde_json::json!({
            "startup_mode": invalid,
            "library_path": custom_library,
            "hotkeys": {"capture": "Ctrl+Alt+V"},
        });
        let bytes = serde_json::to_vec(&json).unwrap();
        fs::write(&path, &bytes).unwrap();

        let cfg = AppConfig::load_or_default(&path);
        assert_eq!(cfg.startup_mode, StartupMode::Silent);
        assert_eq!(cfg.library_path, custom_library);
        assert_eq!(cfg.hotkeys.capture, "Ctrl+Alt+V");
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert!(backup_names(dir.path(), "config.json.bad-").is_empty());
    }
}

#[test]
fn default_config_matches_design() {
    let cfg = AppConfig::default();
    assert!(cfg.library_path.ends_with(Path::new("VisRecall")));
    assert_eq!(cfg.startup_mode, StartupMode::Silent);
    assert_eq!(cfg.hotkeys.capture, "Ctrl+Shift+S");
    assert!(cfg.ball.enabled);
    assert_eq!(cfg.ball.size, 56);
    assert!(cfg.ball.positions.is_empty());
    assert!(cfg.capture.realtime_fallback);
    assert!(!cfg.privacy.clipboard_capture);
    assert!(!cfg.privacy.telemetry);
}

#[test]
fn loading_clamps_the_ball_size_to_the_supported_range() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let mut cfg = AppConfig::default();

    for (stored, expected) in [(40, BALL_MIN_SIZE), (200, BALL_MAX_SIZE), (100, 100)] {
        cfg.ball.size = stored;
        cfg.save_atomic(&path).unwrap();
        assert_eq!(AppConfig::load_or_default(&path).ball.size, expected);
    }
}

#[test]
fn missing_config_file_returns_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    assert_eq!(AppConfig::load_or_default(&path), AppConfig::default());
    assert!(!path.exists(), "loading must not create the file");
}

#[test]
fn partial_config_fills_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    fs::write(
        &path,
        r#"{"hotkeys":{"capture":"Ctrl+Alt+V"},"ball":{"size":64}}"#,
    )
    .unwrap();

    let cfg = AppConfig::load_or_default(&path);
    assert_eq!(cfg.hotkeys.capture, "Ctrl+Alt+V");
    assert_eq!(cfg.ball.size, 64);
    assert_eq!(cfg.startup_mode, StartupMode::Silent);
    assert!(cfg.ball.enabled);
    assert!(cfg.capture.realtime_fallback);
    assert_eq!(cfg.library_path, AppConfig::default().library_path);
}

#[test]
fn legacy_primary_backup_models_are_migrated() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    fs::write(
        &path,
        r#"{"models":{"primary":{"base_url":"https://a.example.com/v1","model":"m1"},"backup":{"base_url":"https://b.example.com/v1","model":"m2"}}}"#,
    )
    .unwrap();

    let cfg = AppConfig::load_or_default(&path);
    assert_eq!(cfg.models.entries.len(), 2);
    assert_eq!(cfg.models.entries[0].id, "primary");
    assert_eq!(cfg.models.entries[0].name, "主模型");
    assert_eq!(cfg.models.entries[0].protocol, "openai");
    assert_eq!(cfg.models.entries[0].model, "m1");
    assert_eq!(cfg.models.entries[1].id, "backup");
    assert_eq!(cfg.models.active_id.as_deref(), Some("primary"));
    // 迁移只改 models，其它字段仍按默认值补齐
    assert_eq!(cfg.hotkeys.capture, "Ctrl+Shift+S");
    assert!(cfg.capture.realtime_fallback);
}

#[test]
fn corrupt_config_is_backed_up_and_defaults_returned() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    fs::write(&path, "{ this is not json").unwrap();

    let cfg = AppConfig::load_or_default(&path);
    assert_eq!(cfg, AppConfig::default());
    assert!(!path.exists());
    assert_eq!(backup_names(dir.path(), "config.json.bad-").len(), 1);
}

#[test]
fn save_atomic_creates_parent_and_leaves_no_temp_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested").join("config.json");
    AppConfig::default().save_atomic(&path).unwrap();
    assert!(path.exists());
    assert!(!path.with_file_name("config.json.tmp").exists());
    let tmp_files: Vec<PathBuf> = fs::read_dir(path.parent().unwrap())
        .unwrap()
        .filter_map(|entry| {
            let entry = entry.unwrap();
            entry
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")
                .then_some(entry.path())
        })
        .collect();
    assert!(tmp_files.is_empty(), "leftover temp files: {tmp_files:?}");
}

#[test]
fn app_paths_resolve_under_environment_roots() {
    if std::env::var_os("APPDATA").is_none() || std::env::var_os("USERPROFILE").is_none() {
        return;
    }
    let paths = AppPaths::resolve().unwrap();
    assert!(paths
        .config_file
        .ends_with(Path::new("VisRecall/config.json")));
    assert!(paths
        .default_library_root
        .ends_with(Path::new("Documents/VisRecall")));
    assert!(paths.config_file.is_absolute());
}
