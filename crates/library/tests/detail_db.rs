//! 备注写入、计数与过期选取的集成测试。

use visrecall_domain::item::{ItemKind, NewItem};
use visrecall_domain::AppError;
use visrecall_library::items::MAX_NOTE_CHARS;
use visrecall_library::Library;

fn new_item(n: i64, captured_at: i64) -> NewItem {
    NewItem {
        kind: ItemKind::Image,
        captured_at,
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

fn library() -> (tempfile::TempDir, Library) {
    let dir = tempfile::tempdir().unwrap();
    let lib = Library::open(dir.path().to_path_buf()).unwrap();
    (dir, lib)
}

#[test]
fn note_accepts_exactly_the_limit_counting_chars() {
    let (_dir, lib) = library();
    let item = lib.insert_item(new_item(1, 1_700_000_000_000)).unwrap();

    // 含 emoji 与汉字：按字符计正好 10000（字节数远超 10000）。
    let note: String = "😀字".repeat(MAX_NOTE_CHARS / 2);
    assert_eq!(note.chars().count(), MAX_NOTE_CHARS);
    lib.set_note(&item.id, &note).unwrap();

    let stored = lib.get_item(&item.id).unwrap().unwrap().note;
    assert_eq!(stored.chars().count(), MAX_NOTE_CHARS);
    assert_eq!(stored, note);
}

#[test]
fn note_rejects_one_char_over_limit_and_keeps_old_value() {
    let (_dir, lib) = library();
    let item = lib.insert_item(new_item(1, 1_700_000_000_000)).unwrap();
    lib.set_note(&item.id, "保留我").unwrap();

    let too_long = "字".repeat(MAX_NOTE_CHARS + 1);
    assert!(matches!(
        lib.set_note(&item.id, &too_long),
        Err(AppError::InvalidInput(_))
    ));
    assert_eq!(lib.get_item(&item.id).unwrap().unwrap().note, "保留我");
}

#[test]
fn empty_note_clears_it() {
    let (_dir, lib) = library();
    let item = lib.insert_item(new_item(1, 1_700_000_000_000)).unwrap();
    lib.set_note(&item.id, "some note").unwrap();
    lib.set_note(&item.id, "").unwrap();
    assert_eq!(lib.get_item(&item.id).unwrap().unwrap().note, "");
}

#[test]
fn note_on_missing_item_is_not_found() {
    let (_dir, lib) = library();
    assert!(matches!(
        lib.set_note("no-such-item", "x"),
        Err(AppError::NotFound)
    ));
}

#[test]
fn note_write_updates_updated_at() {
    let (_dir, lib) = library();
    let item = lib.insert_item(new_item(1, 1_700_000_000_000)).unwrap();
    let (_, before) = lib.item_timestamps_for_test(&item.id).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(2));
    lib.set_note(&item.id, "x").unwrap();
    let (_, after) = lib.item_timestamps_for_test(&item.id).unwrap();
    assert!(after >= before);
}

#[test]
fn counts_track_inserts_and_cutoff() {
    let (_dir, lib) = library();
    lib.insert_item(new_item(1, 100)).unwrap();
    lib.insert_item(new_item(2, 200)).unwrap();
    lib.insert_item(new_item(3, 300)).unwrap();

    assert_eq!(lib.count_items().unwrap(), 3);
    assert_eq!(lib.count_items_before(200).unwrap(), 1);
    assert_eq!(lib.count_items_before(0).unwrap(), 0);
    assert_eq!(lib.count_items_before(1000).unwrap(), 3);
}

#[test]
fn list_expired_returns_oldest_first_within_limit() {
    let (_dir, lib) = library();
    lib.insert_item(new_item(1, 300)).unwrap();
    lib.insert_item(new_item(2, 100)).unwrap();
    lib.insert_item(new_item(3, 200)).unwrap();
    lib.insert_item(new_item(4, 400)).unwrap();

    let expired = lib.list_expired(350, 10).unwrap();
    let order: Vec<i64> = expired.iter().map(|item| item.captured_at).collect();
    assert_eq!(order, vec![100, 200, 300]);

    let limited = lib.list_expired(350, 2).unwrap();
    assert_eq!(limited.len(), 2);
    assert_eq!(limited[0].captured_at, 100);

    assert!(lib.list_expired(0, 10).unwrap().is_empty());
}

#[test]
fn list_expired_clamps_limit_to_at_most_500() {
    let (_dir, lib) = library();
    for n in 0..3 {
        lib.insert_item(new_item(n, 10 + n)).unwrap();
    }
    // limit 0 被夹到 1，limit 远大于总数时返回全部。
    assert_eq!(lib.list_expired(1000, 0).unwrap().len(), 1);
    assert_eq!(lib.list_expired(1000, 10_000).unwrap().len(), 3);
}
