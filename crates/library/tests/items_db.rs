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

#[test]
fn set_item_note_updates_note_and_timestamp() {
    let dir = tempfile::tempdir().unwrap();
    let lib = Library::open(dir.path().to_path_buf()).unwrap();
    let item = lib.insert_item(new_item(1)).unwrap();
    assert_eq!(item.note, "");

    let (created_at, before) = lib.item_timestamps_for_test(&item.id).unwrap();
    assert_eq!(created_at, before);
    // 时间戳是毫秒：确保 `now_millis()` 一定前进（否则断言可能在同一毫秒内假负）。
    std::thread::sleep(std::time::Duration::from_millis(2));

    lib.set_item_note(&item.id, "混合拖放的备注").unwrap();
    let got = lib.get_item(&item.id).unwrap().unwrap();
    assert_eq!(got.note, "混合拖放的备注");
    assert_eq!(got.id, item.id);

    let (created_at_after, after) = lib.item_timestamps_for_test(&item.id).unwrap();
    assert_eq!(created_at_after, created_at, "created_at must not change");
    assert!(
        after > before,
        "set_item_note must refresh updated_at ({after} should be newer than {before})"
    );
}

#[test]
fn set_item_note_rejects_unknown_id() {
    let dir = tempfile::tempdir().unwrap();
    let lib = Library::open(dir.path().to_path_buf()).unwrap();
    let err = lib.set_item_note("missing", "note").unwrap_err();
    assert!(matches!(err, visrecall_domain::AppError::NotFound));
}

fn text_item(n: i64) -> NewItem {
    NewItem {
        kind: ItemKind::Text,
        char_count: Some(12),
        original_path: format!("texts/{n}.txt"),
        ..new_item(n)
    }
}

#[test]
fn open_clears_tmp_files() {
    let dir = tempfile::tempdir().unwrap();
    drop(Library::open(dir.path().to_path_buf()).unwrap());

    let tmp = dir.path().join("tmp");
    std::fs::write(tmp.join("a.png.tmp"), b"a").unwrap();
    std::fs::write(tmp.join("b.txt.tmp"), b"b").unwrap();

    let lib = Library::open(dir.path().to_path_buf()).unwrap();
    assert!(std::fs::read_dir(&tmp).unwrap().next().is_none());
    assert!(lib.list_items(None, 10).unwrap().is_empty());
}

#[test]
fn cleanup_startup_counts_tmp_removed() {
    let dir = tempfile::tempdir().unwrap();
    let lib = Library::open(dir.path().to_path_buf()).unwrap();

    let tmp = dir.path().join("tmp");
    std::fs::write(tmp.join("a.png.tmp"), b"a").unwrap();
    std::fs::write(tmp.join("b.txt.tmp"), b"b").unwrap();

    let stats = lib.cleanup_startup().unwrap();
    assert_eq!(stats.tmp_removed, 2);
    assert!(std::fs::read_dir(&tmp).unwrap().next().is_none());
}

#[test]
fn cleanup_startup_keeps_referenced_files() {
    let dir = tempfile::tempdir().unwrap();
    let lib = Library::open(dir.path().to_path_buf()).unwrap();
    let image = lib.insert_item(new_item(1)).unwrap();
    let text = lib.insert_item(text_item(2)).unwrap();

    let image_path = dir.path().join(&image.original_path);
    let text_path = dir.path().join(&text.original_path);
    std::fs::create_dir_all(image_path.parent().unwrap()).unwrap();
    std::fs::write(&image_path, b"png").unwrap();
    std::fs::write(&text_path, b"text").unwrap();

    let stats = lib.cleanup_startup().unwrap();
    assert_eq!(stats.orphans, 0);
    assert!(image_path.exists());
    assert!(text_path.exists());
}

#[test]
fn cleanup_startup_counts_orphans_without_deleting() {
    let dir = tempfile::tempdir().unwrap();
    let lib = Library::open(dir.path().to_path_buf()).unwrap();

    let orphan_image = dir.path().join("originals/images/2026/11/orphan.png");
    let orphan_text = dir.path().join("texts/orphan.txt");
    std::fs::create_dir_all(orphan_image.parent().unwrap()).unwrap();
    std::fs::write(&orphan_image, b"png").unwrap();
    std::fs::write(&orphan_text, b"text").unwrap();

    let stats = lib.cleanup_startup().unwrap();
    assert_eq!(stats.orphans, 2);
    assert!(orphan_image.exists(), "orphans must not be deleted");
    assert!(orphan_text.exists(), "orphans must not be deleted");
}

#[test]
fn cleanup_startup_without_tmp_dir_returns_zero() {
    let dir = tempfile::tempdir().unwrap();
    let lib = Library::open(dir.path().to_path_buf()).unwrap();
    std::fs::remove_dir_all(dir.path().join("tmp")).unwrap();

    let stats = lib.cleanup_startup().unwrap();
    assert_eq!(stats.tmp_removed, 0);
}
