//! 「文本上传云端解析」开关经 `AppCore` façade 的读写测试。
//!
//! 命令层（`cloud_text_ai_set`）只做 DTO 转换，真正落盘在这里；默认值必须保守（关闭）。

use std::sync::Arc;

use visrecall_app_core::events::RecordingSink;
use visrecall_app_core::{AppConfig, AppCore, AppPaths};
use visrecall_library::Library;

fn paths_in(dir: &std::path::Path) -> AppPaths {
    AppPaths {
        config_file: dir.join("config.json"),
        default_library_root: dir.join("library"),
    }
}

fn open(dir: &std::path::Path) -> AppCore {
    let paths = paths_in(dir);
    let config = AppConfig {
        library_path: paths.default_library_root.clone(),
        ..AppConfig::default()
    };
    config.save_atomic(&paths.config_file).unwrap();
    AppCore::open(paths, Arc::new(RecordingSink::default()))
}

#[test]
fn cloud_text_ai_defaults_to_disabled_and_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let core = open(dir.path());

    assert!(
        !core.library().unwrap().cloud_text_ai_enabled(),
        "默认必须关闭：未开启前文本不得上传云端"
    );

    core.set_cloud_text_ai(true).unwrap();
    assert!(core.library().unwrap().cloud_text_ai_enabled());

    // 真正落库：重新打开库仍是开启态（不是只改内存）。
    let reopened = Library::open(dir.path().join("library")).unwrap();
    assert!(reopened.cloud_text_ai_enabled());

    core.set_cloud_text_ai(false).unwrap();
    assert!(!core.library().unwrap().cloud_text_ai_enabled());
}

#[test]
fn cloud_image_ai_defaults_to_disabled_and_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let core = open(dir.path());

    assert!(
        !core.library().unwrap().cloud_image_ai_enabled(),
        "默认必须关闭：未开启前图片不得上传云端"
    );

    core.set_cloud_image_ai(true).unwrap();
    assert!(core.library().unwrap().cloud_image_ai_enabled());

    let reopened = Library::open(dir.path().join("library")).unwrap();
    assert!(reopened.cloud_image_ai_enabled());

    core.set_cloud_image_ai(false).unwrap();
    assert!(!core.library().unwrap().cloud_image_ai_enabled());
}
