//! 验证安装选择交给应用配置后，由常规启动流程在选定位置建库。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use visrecall_app_core::{AppConfig, AppCore, AppError, AppPaths, NullEventSink};
use visrecall_library::db::DB_FILE;
use visrecall_library::paths::LIBRARY_META_FILE;

struct Fixture {
    _dir: tempfile::TempDir,
    paths: AppPaths,
    install: PathBuf,
    selected: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let paths = AppPaths {
            config_file: dir.path().join("config/config.json"),
            default_library_root: dir.path().join("default"),
        };
        let install = dir.path().join("program");
        let selected = dir.path().join("目记 数据").join("素材库");
        fs::create_dir_all(&install).unwrap();
        fs::create_dir_all(&selected).unwrap();
        Self {
            _dir: dir,
            paths,
            install,
            selected,
        }
    }

    fn prepare(&self, path: &Path) -> Result<bool, AppError> {
        AppCore::prepare_installer_library(&self.paths, &self.install, || Ok(Some(path.to_owned())))
    }

    fn assert_no_new_library(&self) {
        assert!(!self.paths.config_file.exists());
        assert!(!self.paths.default_library_root.exists());
        assert!(!self.selected.join(LIBRARY_META_FILE).exists());
        assert!(!self.selected.join(DB_FILE).exists());
    }
}

#[test]
fn first_launch_uses_saved_unicode_path() {
    let fixture = Fixture::new();
    assert!(fixture.prepare(&fixture.selected).unwrap());
    // 保存配置时不能提前建库或留下写入探针。
    assert_eq!(fs::read_dir(&fixture.selected).unwrap().count(), 0);
    let core = AppCore::open(fixture.paths.clone(), Arc::new(NullEventSink));
    assert!(core.library_ok());
    assert_eq!(core.config().library_path, fixture.selected);
    assert!(fixture.selected.join(LIBRARY_META_FILE).is_file());
    assert!(fixture.selected.join(DB_FILE).is_file());
    assert!(!fixture.paths.default_library_root.exists());
}

#[test]
fn existing_config_takes_priority() {
    let fixture = Fixture::new();
    let config = AppConfig {
        library_path: fixture.paths.default_library_root.clone(),
        ..AppConfig::default()
    };
    config.save_atomic(&fixture.paths.config_file).unwrap();
    let saved = fs::read(&fixture.paths.config_file).unwrap();

    assert!(fixture.prepare(&fixture.selected).unwrap());
    assert_eq!(fs::read(&fixture.paths.config_file).unwrap(), saved);
    assert_eq!(fs::read_dir(&fixture.selected).unwrap().count(), 0);
}

#[test]
fn existing_default_library_markers_preserve_legacy_library() {
    for marker in [LIBRARY_META_FILE, DB_FILE] {
        let fixture = Fixture::new();
        fs::create_dir(&fixture.paths.default_library_root).unwrap();
        let existing = fixture.paths.default_library_root.join(marker);
        fs::write(&existing, b"legacy").unwrap();

        assert!(fixture.prepare(&fixture.selected).unwrap());
        assert_eq!(fs::read(existing).unwrap(), b"legacy");
        assert!(!fixture.paths.config_file.exists());
        assert_eq!(fs::read_dir(&fixture.selected).unwrap().count(), 0);
    }
}

#[test]
fn missing_folder_allows_retry_after_restoring_it() {
    let fixture = Fixture::new();
    fs::remove_dir(&fixture.selected).unwrap();
    let err = fixture.prepare(&fixture.selected).unwrap_err();
    assert!(err
        .to_string()
        .contains(&fixture.selected.display().to_string()));
    fixture.assert_no_new_library();

    fs::create_dir(&fixture.selected).unwrap();
    assert!(fixture.prepare(&fixture.selected).unwrap());
}

#[test]
fn relative_nonempty_and_program_paths_are_rejected() {
    let fixture = Fixture::new();
    fs::write(fixture.selected.join("已有文件.txt"), b"keep").unwrap();

    let nested = fixture.install.join("library");
    fs::create_dir(&nested).unwrap();
    for path in [
        Path::new("relative"),
        &fixture.selected,
        &fixture.install,
        &nested,
    ] {
        assert!(matches!(
            fixture.prepare(path),
            Err(AppError::InvalidInput(_))
        ));
    }
    assert_eq!(
        fs::read(fixture.selected.join("已有文件.txt")).unwrap(),
        b"keep"
    );
    fixture.assert_no_new_library();
}

#[test]
fn config_save_failure_leaves_library_untouched() {
    let fixture = Fixture::new();
    let tmp = fixture.paths.config_file.with_extension("json.tmp");
    fs::create_dir_all(&tmp).unwrap();
    fs::write(tmp.join("keep"), b"data").unwrap();

    let err = fixture.prepare(&fixture.selected).unwrap_err();
    assert!(err.to_string().contains("无法保存素材库"));
    fixture.assert_no_new_library();
    assert_eq!(fs::read_dir(&fixture.selected).unwrap().count(), 0);

    fs::remove_file(tmp.join("keep")).unwrap();
    fs::remove_dir(tmp).unwrap();
    assert!(fixture.prepare(&fixture.selected).unwrap());
}

#[test]
fn no_selection_leaves_normal_startup_to_app_core() {
    let fixture = Fixture::new();
    assert!(
        !AppCore::prepare_installer_library(&fixture.paths, &fixture.install, || { Ok(None) })
            .unwrap()
    );
    fixture.assert_no_new_library();
}
