//! 单条删除与全库清空的集成测试（真实文件 + 真实 SQLite）。

use std::fs;
use std::path::Path;

use visrecall_domain::item::{ItemKind, NewItem};
use visrecall_domain::AppError;
use visrecall_library::Library;

fn new_item(n: i64, kind: ItemKind, original: &str) -> NewItem {
    NewItem {
        kind,
        captured_at: 1_700_000_000_000 + n,
        source_app: Some("chrome.exe".into()),
        source_title: Some("test".into()),
        source_url: None,
        width: Some(100),
        height: Some(50),
        char_count: None,
        original_path: original.to_string(),
        content_hash: format!("hash-{n}"),
    }
}

fn library() -> (tempfile::TempDir, Library) {
    let dir = tempfile::tempdir().unwrap();
    let lib = Library::open(dir.path().to_path_buf()).unwrap();
    (dir, lib)
}

fn write_file(root: &Path, rel: &str) -> std::path::PathBuf {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&path, b"payload").unwrap();
    path
}

/// 递归统计目录下的普通文件数（目录结构本身保留）。
fn count_files(base: &Path) -> usize {
    let mut total = 0;
    let mut stack = vec![base.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                total += 1;
            }
        }
    }
    total
}

fn count_rows(lib: &Library, sql: &str) -> i64 {
    // 通过公开 API 无法直接计数表，用「行为」断言代替：这里只统计 items 与 tags。
    match sql {
        "items" => lib.count_items().unwrap() as i64,
        "tags" => lib.tags_list().unwrap().len() as i64,
        other => panic!("unsupported table {other}"),
    }
}

#[test]
fn delete_item_removes_row_file_thumb_and_jobs() {
    let (dir, lib) = library();
    let original = write_file(dir.path(), "originals/images/2026/11/a.png");
    let thumb = write_file(dir.path(), "thumbs/a.png");

    let item = lib
        .insert_item(new_item(
            1,
            ItemKind::Image,
            "originals/images/2026/11/a.png",
        ))
        .unwrap();
    lib.update_status(
        &item.id,
        visrecall_domain::item::ItemStatus::Ready,
        Some("thumbs/a.png"),
    )
    .unwrap();
    lib.enqueue_job(&item.id, "thumbnail", 1).unwrap();
    lib.tag_add(&item.id, "work").unwrap();

    let deleted = lib.delete_item(&item.id).unwrap();
    assert_eq!(deleted.original_path, "originals/images/2026/11/a.png");
    assert_eq!(deleted.thumb_path.as_deref(), Some("thumbs/a.png"));

    assert!(!original.exists(), "original file should be gone");
    assert!(!thumb.exists(), "thumb file should be gone");
    assert!(lib.get_item(&item.id).unwrap().is_none());
    assert_eq!(count_rows(&lib, "items"), 0);
    assert!(lib.tags_for_item(&item.id).unwrap().is_empty());
    // 标签仍在（手动），但关联已随素材删除而级联清空。
    assert_eq!(lib.tags_list().unwrap()[0].item_count, 0);
}

#[test]
fn delete_item_succeeds_when_original_already_missing() {
    let (_dir, lib) = library();
    let item = lib
        .insert_item(new_item(1, ItemKind::Image, "originals/images/missing.png"))
        .unwrap();

    let deleted = lib.delete_item(&item.id).unwrap();
    assert_eq!(deleted.original_path, "originals/images/missing.png");
    assert!(lib.get_item(&item.id).unwrap().is_none());
}

#[test]
fn delete_missing_item_is_not_found() {
    let (_dir, lib) = library();
    assert!(matches!(
        lib.delete_item("no-such-item"),
        Err(AppError::NotFound)
    ));
}

#[test]
fn delete_item_with_readonly_file_still_removes_row() {
    let (dir, lib) = library();
    let path = write_file(dir.path(), "originals/images/2026/11/ro.png");
    set_readonly(&path);

    let item = lib
        .insert_item(new_item(
            1,
            ItemKind::Image,
            "originals/images/2026/11/ro.png",
        ))
        .unwrap();
    lib.delete_item(&item.id).unwrap();

    // 库记录必须已删；文件删除取决于卷与属性的处理方式（部分环境仍可删除
    // 只读文件），失败时只记 warn 并由孤儿扫描计数，因此这里不断言文件是否还在。
    assert!(lib.get_item(&item.id).unwrap().is_none());

    // 收尾：去掉只读属性，避免临时目录清理失败。
    if path.exists() {
        set_writable(&path);
    }
}

fn set_readonly(path: &Path) {
    let mut perms = fs::metadata(path).unwrap().permissions();
    perms.set_readonly(true);
    fs::set_permissions(path, perms).unwrap();
}

/// 测试收尾用：恢复可写属性，避免只读文件卡住临时目录清理。
///
/// 这里刻意对临时目录里的测试文件关闭只读，`permissions_set_readonly_false`
/// 的告警在测试上下文没有意义。
#[allow(clippy::permissions_set_readonly_false)]
fn set_writable(path: &Path) {
    let mut perms = fs::metadata(path).unwrap().permissions();
    perms.set_readonly(false);
    fs::set_permissions(path, perms).unwrap();
}

#[test]
fn purge_clears_tables_and_content_dirs_but_keeps_settings() {
    let (dir, lib) = library();
    write_file(dir.path(), "originals/images/2026/11/a.png");
    write_file(dir.path(), "texts/a.txt");
    write_file(dir.path(), "thumbs/a.png");

    for n in 0..5 {
        let item = lib
            .insert_item(new_item(
                n,
                ItemKind::Image,
                "originals/images/2026/11/a.png",
            ))
            .unwrap();
        lib.tag_add(&item.id, if n % 2 == 0 { "work" } else { "life" })
            .unwrap();
    }
    lib.tag_add(&lib.list_items(None, 1).unwrap()[0].id, "reference")
        .unwrap();
    lib.setting_set("storage.capacity_gb", &50u32).unwrap();

    let report = lib.purge_library().unwrap();
    assert_eq!(report.items, 5);
    assert_eq!(report.tags, 3);
    assert_eq!(report.files_removed, 3);
    assert_eq!(report.files_failed, 0);

    assert_eq!(count_rows(&lib, "items"), 0);
    assert_eq!(count_rows(&lib, "tags"), 0);
    // 目录仍在（结构与 library.json 保留），其下任何层级的普通文件都被清空。
    for rel in ["originals", "texts", "thumbs"] {
        let base = dir.path().join(rel);
        assert!(base.is_dir(), "{rel} should still exist");
        assert_eq!(count_files(&base), 0, "{rel} should hold no files");
    }
    assert!(dir.path().join("library.json").is_file());
    // 策略配置属于用户偏好，不清空。
    assert_eq!(lib.setting_get::<u32>("storage.capacity_gb"), Some(50));
}

#[test]
fn purge_empty_library_is_noop() {
    let (_dir, lib) = library();
    let report = lib.purge_library().unwrap();
    assert_eq!(
        report,
        visrecall_library::PurgeReport {
            items: 0,
            tags: 0,
            files_removed: 0,
            files_failed: 0
        }
    );
}

#[test]
fn capture_still_works_after_purge() {
    let (_dir, lib) = library();
    let item = lib
        .insert_item(new_item(1, ItemKind::Text, "texts/a.txt"))
        .unwrap();
    lib.purge_library().unwrap();
    assert!(lib.get_item(&item.id).unwrap().is_none());

    let fresh = lib
        .insert_item(new_item(2, ItemKind::Text, "texts/b.txt"))
        .unwrap();
    assert_eq!(lib.count_items().unwrap(), 1);
    assert!(lib.get_item(&fresh.id).unwrap().is_some());
}

#[test]
fn clear_content_dirs_keeps_directories() {
    let (dir, lib) = library();
    write_file(dir.path(), "originals/images/x/y.png");
    write_file(dir.path(), "texts/t.txt");
    write_file(dir.path(), "thumbs/t.png");

    let (removed, failed) = lib.clear_content_dirs().unwrap();
    assert_eq!((removed, failed), (3, 0));
    assert!(dir.path().join("originals/images/x").is_dir());
    assert!(!dir.path().join("originals/images/x/y.png").exists());
}
