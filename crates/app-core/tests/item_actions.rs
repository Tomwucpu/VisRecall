//! 改名与批量删除的编排测试：事件只发一次、`NotFound` 幂等、来源标题不被覆盖。

use std::path::Path;
use std::sync::Arc;

use visrecall_app_core::events::{AppEvent, RecordingSink};
use visrecall_app_core::{AppCore, AppPaths};
use visrecall_domain::item::{ItemKind, NewItem};
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
    // 默认组合键在开发机上常被截图工具占用，测试换一个几乎不会冲突的组合。
    config.hotkeys.capture = "Ctrl+Alt+Shift+F8".to_string();
    config.save_atomic(&paths.config_file).unwrap();

    let sink = Arc::new(RecordingSink::default());
    (AppCore::open(paths, sink.clone()), sink)
}

fn insert_item(core: &AppCore, n: i64) -> String {
    let item = core
        .library()
        .unwrap()
        .insert_item(NewItem {
            kind: ItemKind::Image,
            captured_at: 1_700_000_000_000 + n,
            source_app: Some("chrome.exe".into()),
            source_title: Some(format!("来源标题 {n}")),
            source_url: None,
            width: Some(100),
            height: Some(50),
            char_count: None,
            original_path: format!("originals/images/2026/11/{n}.png"),
            content_hash: format!("hash-{n}"),
        })
        .unwrap();
    item.id
}

#[test]
fn rename_item_updates_title_and_emits_once() {
    let dir = tempfile::tempdir().unwrap();
    let (core, sink) = open(dir.path());
    let id = insert_item(&core, 1);

    let renamed = core.rename_item(&id, Some("季度营收")).unwrap();
    assert_eq!(renamed.title.as_deref(), Some("季度营收"));
    assert_eq!(
        renamed.source_title.as_deref(),
        Some("来源标题 1"),
        "改名不能覆盖来源标题"
    );

    let rename_events: Vec<String> = sink
        .events()
        .into_iter()
        .filter_map(|event| match event {
            AppEvent::ItemRenamed(payload) => Some(payload.id),
            _ => None,
        })
        .collect();
    assert_eq!(rename_events, vec![id.clone()]);

    // 清空标题 = 恢复未改名。
    let cleared = core.rename_item(&id, None).unwrap();
    assert!(cleared.title.is_none());
}

#[test]
fn delete_items_is_idempotent_and_emits_one_batch_event() {
    let dir = tempfile::tempdir().unwrap();
    let (core, sink) = open(dir.path());
    let first = insert_item(&core, 1);
    let second = insert_item(&core, 2);
    let kept = insert_item(&core, 3);

    let report = core
        .delete_items(&[
            first.clone(),
            second.clone(),
            // 重复 id 与不存在的 id：都应计入「已删除」，且不阻断整批。
            second.clone(),
            "missing".to_string(),
        ])
        .unwrap();
    assert_eq!(report.deleted, 3);
    assert_eq!(report.failed, 0);

    let remaining = core.library().unwrap().list_items(None, 10).unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].id, kept);

    let events = sink.events();
    let batch: Vec<(u64, u64)> = events
        .iter()
        .filter_map(|event| match event {
            AppEvent::ItemsDeleted(payload) => Some((payload.deleted, payload.failed)),
            _ => None,
        })
        .collect();
    assert_eq!(batch, vec![(3, 0)], "批量删除只发一次汇总事件");
    assert!(
        !events.iter().any(|event| matches!(event, AppEvent::ItemDeleted(_))),
        "批量路径不应逐条发 item_deleted"
    );
}

#[test]
fn delete_items_ignores_blank_ids_without_failing() {
    let dir = tempfile::tempdir().unwrap();
    let (core, _sink) = open(dir.path());
    let id = insert_item(&core, 1);

    let report = core
        .delete_items(&["".to_string(), "   ".to_string(), id])
        .unwrap();
    assert_eq!(report.deleted, 1);
    assert_eq!(core.library().unwrap().count_items().unwrap(), 0);
}
