//! `items` 表与 jobs 恢复的集成测试（真实 SQLite 文件 + 临时库根目录）。

use visrecall_domain::item::{ItemKind, ItemStatus, NewItem};
use visrecall_library::Library;

fn new_item(n: i64) -> NewItem {
    NewItem {
        kind: ItemKind::Image,
        captured_at: 1_700_000_000_000 + n,
        source_app: Some("chrome.exe".into()),
        source_title: Some("test".into()),
        source_url: None,
        width: Some(100),
        height: Some(50),
        char_count: None,
        original_path: format!("originals/images/2026/11/{n}.png"),
        content_hash: format!("hash-{n}"),
    }
}

#[test]
fn insert_list_newest_first() {
    let dir = tempfile::tempdir().unwrap();
    let lib = Library::open(dir.path().to_path_buf()).unwrap();
    let a = lib.insert_item(new_item(1)).unwrap();
    let b = lib.insert_item(new_item(2)).unwrap();
    assert_eq!(a.status, ItemStatus::Parsing);

    let rows = lib.list_items(None, 50).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].id, b.id);
    assert_eq!(rows[1].id, a.id);
}

#[test]
fn keyset_pagination_is_stable() {
    let dir = tempfile::tempdir().unwrap();
    let lib = Library::open(dir.path().to_path_buf()).unwrap();
    for n in 0..5 {
        lib.insert_item(new_item(n)).unwrap();
    }

    let page1 = lib.list_items(None, 2).unwrap();
    let cursor = page1.last().unwrap().captured_at;
    let page2 = lib.list_items(Some(cursor), 2).unwrap();
    assert_eq!(page2.len(), 2);
    assert!(page2.iter().all(|i| i.captured_at < cursor));
}

#[test]
fn update_status_sets_thumb() {
    let dir = tempfile::tempdir().unwrap();
    let lib = Library::open(dir.path().to_path_buf()).unwrap();
    let item = lib.insert_item(new_item(1)).unwrap();
    lib.update_status(&item.id, ItemStatus::Ready, Some("thumbs/1.png"))
        .unwrap();
    let got = lib.get_item(&item.id).unwrap().unwrap();
    assert_eq!(got.status, ItemStatus::Ready);
    assert_eq!(got.thumb_path.as_deref(), Some("thumbs/1.png"));
}

#[test]
fn startup_recovery_requeues_running_jobs() {
    let dir = tempfile::tempdir().unwrap();
    let lib = Library::open(dir.path().to_path_buf()).unwrap();
    let item = lib.insert_item(new_item(1)).unwrap();
    lib.enqueue_job(&item.id, "thumbnail", 1).unwrap();
    lib.mark_job_running_for_test(&item.id, "thumbnail")
        .unwrap();

    assert_eq!(lib.recover_jobs().unwrap(), 1);
    assert_eq!(lib.job_state(&item.id, "thumbnail").unwrap(), "queued");
}
