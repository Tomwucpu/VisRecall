//! 内容编辑的编排测试：三个方法与 `item_content_updated` 的 field 取值。

use std::fs;
use std::path::Path;
use std::sync::Arc;

use visrecall_app_core::events::{AppEvent, RecordingSink};
use visrecall_app_core::{AppCore, AppPaths};
use visrecall_domain::item::{ItemKind, NewItem};
use visrecall_domain::AppError;
use visrecall_library::AppConfig;

fn paths_in(dir: &Path) -> AppPaths {
    AppPaths {
        config_file: dir.join("config.json"),
        default_library_root: dir.join("library"),
    }
}

fn open(dir: &Path) -> (AppCore, Arc<RecordingSink>) {
    let paths = paths_in(dir);
    let mut config = AppConfig {
        library_path: paths.default_library_root.clone(),
        ..AppConfig::default()
    };
    config.hotkeys.capture = "Ctrl+Alt+Shift+F8".to_string();
    config.save_atomic(&paths.config_file).unwrap();

    let sink = Arc::new(RecordingSink::default());
    (AppCore::open(paths, sink.clone()), sink)
}

fn insert_item(core: &AppCore, kind: ItemKind, n: i64, original_path: &str) -> String {
    core.library()
        .unwrap()
        .insert_item(NewItem {
            kind,
            captured_at: 1_700_000_000_000 + n,
            source_app: Some("chrome.exe".into()),
            source_title: Some(format!("素材 {n}")),
            source_url: None,
            width: None,
            height: None,
            char_count: None,
            original_path: original_path.to_string(),
            content_hash: format!("hash-{n}"),
        })
        .unwrap()
        .id
}

fn content_events(sink: &RecordingSink) -> Vec<(String, String)> {
    sink.events()
        .into_iter()
        .filter_map(|event| match event {
            AppEvent::ItemContentUpdated(payload) => Some((payload.id, payload.field)),
            _ => None,
        })
        .collect()
}

#[test]
fn summary_write_emits_the_summary_field() {
    let dir = tempfile::tempdir().unwrap();
    let (core, sink) = open(dir.path());
    let id = insert_item(&core, ItemKind::Image, 1, "originals/images/a.png");

    core.set_summary(&id, "人工改过的描述").unwrap();

    assert_eq!(
        content_events(&sink),
        vec![(id.clone(), "summary".to_string())]
    );
    let insight = core.library().unwrap().insight_get(&id).unwrap().unwrap();
    assert_eq!(insight.summary.as_deref(), Some("人工改过的描述"));
    assert!(insight.summary_user_edited);
}

#[test]
fn ocr_write_emits_the_ocr_field() {
    let dir = tempfile::tempdir().unwrap();
    let (core, sink) = open(dir.path());
    let id = insert_item(&core, ItemKind::Image, 1, "originals/images/a.png");

    core.set_ocr_text(&id, "人工改过的 OCR").unwrap();

    assert_eq!(content_events(&sink), vec![(id, "ocrText".to_string())]);
}

#[test]
fn body_write_emits_the_body_field_and_updates_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let (core, sink) = open(dir.path());
    let library = core.library().unwrap();
    let relative = "texts/2026/11/note.txt";
    let path = library.root().join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "旧正文").unwrap();
    let id = insert_item(&core, ItemKind::Text, 1, relative);

    core.set_body_text(&id, "新正文").unwrap();

    assert_eq!(content_events(&sink), vec![(id, "bodyText".to_string())]);
    assert_eq!(fs::read_to_string(&path).unwrap(), "新正文");
}

#[test]
fn rejected_writes_do_not_emit_events() {
    let dir = tempfile::tempdir().unwrap();
    let (core, sink) = open(dir.path());
    let id = insert_item(&core, ItemKind::Image, 1, "originals/images/a.png");

    assert!(matches!(
        core.set_body_text(&id, "图片没有正文"),
        Err(AppError::InvalidInput(_))
    ));
    assert!(content_events(&sink).is_empty());
}
