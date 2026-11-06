//! 用量、容量、保留、孤儿与 checkpoint 的集成测试。

use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use visrecall_domain::item::{ItemKind, ItemStatus, NewItem};
use visrecall_domain::storage::{DEFAULT_CAPACITY_GB, MAX_CAPACITY_GB};
use visrecall_domain::AppError;
use visrecall_library::Library;

fn new_item(n: i64, original: &str) -> NewItem {
    NewItem {
        kind: ItemKind::Image,
        captured_at: 1_700_000_000_000 + n,
        source_app: None,
        source_title: None,
        source_url: None,
        width: Some(10),
        height: Some(10),
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

fn write_file(root: &Path, rel: &str, bytes: usize) -> PathBuf {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&path, vec![b'x'; bytes]).unwrap();
    path
}

/// 递归求和，与 `Library::usage` 的统计口径一致（用于手工对账）。
fn sum_dirs(root: &Path, dirs: &[&str]) -> u64 {
    let mut total = 0;
    for dir in dirs {
        for path in walk(&root.join(dir)) {
            total += fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        }
    }
    total
}

fn walk(base: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
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
                files.push(path);
            }
        }
    }
    files
}

fn size_of(path: &Path) -> u64 {
    fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

const GIB: u64 = 1024 * 1024 * 1024;

#[test]
fn usage_matches_manual_sum() {
    let (dir, lib) = library();
    write_file(dir.path(), "originals/images/2026/11/a.png", 1000);
    write_file(dir.path(), "texts/a.txt", 300);
    write_file(dir.path(), "thumbs/a.png", 200);
    lib.insert_item(new_item(1, "originals/images/2026/11/a.png"))
        .unwrap();

    let usage = lib.usage().unwrap();
    let expected = sum_dirs(dir.path(), &["originals", "texts", "thumbs"])
        + size_of(&dir.path().join("visrecall.db"))
        + size_of(&dir.path().join("visrecall.db-wal"));

    assert_eq!(usage.used_bytes, expected);
    assert_eq!(usage.item_count, 1);
    assert_eq!(usage.capacity_bytes, u64::from(DEFAULT_CAPACITY_GB) * GIB);
}

#[test]
fn capacity_range_is_enforced_before_writing() {
    let (_dir, lib) = library();

    assert!(matches!(
        lib.set_capacity_gb(0),
        Err(AppError::InvalidInput(_))
    ));
    assert!(matches!(
        lib.set_capacity_gb(4097),
        Err(AppError::InvalidInput(_))
    ));
    // 越界不落盘。
    assert!(lib
        .raw_setting_for_test("storage.capacity_gb")
        .unwrap()
        .is_none());

    lib.set_capacity_gb(MAX_CAPACITY_GB).unwrap();
    assert_eq!(lib.capacity_bytes(), u64::from(MAX_CAPACITY_GB) * GIB);
    lib.set_capacity_gb(1).unwrap();
    assert_eq!(lib.capacity_bytes(), GIB);
}

#[test]
fn corrupt_capacity_falls_back_to_default() {
    let (_dir, lib) = library();
    lib.setting_set("storage.capacity_gb", &"sixty").unwrap();
    assert_eq!(lib.capacity_bytes(), u64::from(DEFAULT_CAPACITY_GB) * GIB);

    lib.setting_set("storage.capacity_gb", &0u32).unwrap();
    assert_eq!(lib.capacity_bytes(), u64::from(DEFAULT_CAPACITY_GB) * GIB);
}

#[test]
fn retention_range_and_forever_default() {
    let (_dir, lib) = library();
    assert!(lib.retention().is_forever());

    assert!(matches!(
        lib.set_retention(Some(0)),
        Err(AppError::InvalidInput(_))
    ));
    assert!(matches!(
        lib.set_retention(Some(3651)),
        Err(AppError::InvalidInput(_))
    ));

    lib.set_retention(Some(90)).unwrap();
    assert_eq!(lib.retention().days, Some(90));
    lib.set_retention(None).unwrap();
    assert!(lib.retention().is_forever());
}

#[test]
fn orphan_file_is_counted_and_cleaned() {
    let (dir, lib) = library();
    let referenced = write_file(dir.path(), "originals/images/2026/11/ref.png", 10);
    lib.insert_item(new_item(1, "originals/images/2026/11/ref.png"))
        .unwrap();
    let orphan = write_file(dir.path(), "originals/images/2026/11/orphan.png", 64);

    let stats = lib.orphans().unwrap();
    assert_eq!(stats.files, 1);
    assert_eq!(stats.bytes, 64);

    let cleaned = lib.cleanup_orphans().unwrap();
    assert_eq!(cleaned.files, 1);
    assert_eq!(cleaned.bytes, 64);
    assert!(!orphan.exists());
    assert!(referenced.exists(), "referenced file must survive");
}

#[test]
fn missing_thumb_is_counted_not_deleted() {
    let (dir, lib) = library();
    let thumb = write_file(dir.path(), "thumbs/a.png", 8);
    write_file(dir.path(), "originals/images/2026/11/a.png", 8);
    let item = lib
        .insert_item(new_item(1, "originals/images/2026/11/a.png"))
        .unwrap();
    lib.update_status(&item.id, ItemStatus::Ready, Some("thumbs/a.png"))
        .unwrap();

    assert_eq!(lib.orphans().unwrap().missing_thumbs, 0);
    fs::remove_file(&thumb).unwrap();

    let stats = lib.orphans().unwrap();
    assert_eq!(stats.missing_thumbs, 1);
    // 缺失缩略图不算孤儿，清理入口不处理它。
    assert_eq!(stats.files, 0);
}

#[test]
fn cleanup_counts_only_files_present_at_scan_time() {
    let (dir, lib) = library();
    let first = write_file(dir.path(), "originals/images/2026/11/a.png", 32);
    write_file(dir.path(), "originals/images/2026/11/b.png", 16);

    // 用户在资源管理器里先删掉了一个：清理按**扫描结果**计数（这个文件已经不在
    // 计划里），不会报「统计 2 个、清理 1 个」之外的差额。
    // 「扫描后、删除前被删掉」的竞态由 `NotFound => 计为成功` 分支兜住，无法
    // 在单线程测试里确定性复现。
    fs::remove_file(&first).unwrap();

    let cleaned = lib.cleanup_orphans().unwrap();
    assert_eq!(cleaned.files, 1);
    assert_eq!(cleaned.bytes, 16);
    assert!(walk(&dir.path().join("originals")).is_empty());
    // 再清一次：没有可删的，返回全 0（幂等）。
    assert_eq!(lib.cleanup_orphans().unwrap().files, 0);
}

#[test]
fn startup_cleanup_reports_missing_thumbs() {
    let (dir, lib) = library();
    let thumb = write_file(dir.path(), "thumbs/a.png", 8);
    write_file(dir.path(), "originals/images/2026/11/a.png", 8);
    let item = lib
        .insert_item(new_item(1, "originals/images/2026/11/a.png"))
        .unwrap();
    lib.update_status(&item.id, ItemStatus::Ready, Some("thumbs/a.png"))
        .unwrap();
    write_file(dir.path(), "tmp/pending.tmp", 4);
    fs::remove_file(&thumb).unwrap();

    let cleanup = lib.cleanup_startup().unwrap();
    // 启动清理的口径与孤儿统计一致：缺失缩略图只报告、不删除、不阻塞打开库。
    assert_eq!(cleanup.missing_thumbs, 1);
    assert_eq!(cleanup.tmp_removed, 1);
    assert_eq!(cleanup.orphans, 0);
    assert_eq!(lib.orphans().unwrap().missing_thumbs, 1);
}

#[test]
fn cleanup_never_touches_database_or_library_meta() {
    let (dir, lib) = library();
    lib.insert_item(new_item(1, "originals/images/2026/11/a.png"))
        .unwrap();
    // 造成 -wal 残留。
    lib.setting_set("storage.capacity_gb", &10u32).unwrap();
    write_file(dir.path(), "originals/images/2026/11/orphan.png", 5);
    // 库根、backups/、tmp/ 下的文件都不属于孤儿清理的职责。
    let root_junk = write_file(dir.path(), "junk.bin", 3);
    let backup_junk = write_file(dir.path(), "backups/visrecall.db.bak-2", 3);
    let tmp_junk = write_file(dir.path(), "tmp/pending.tmp", 3);

    let watched = [
        dir.path().join("visrecall.db"),
        dir.path().join("visrecall.db-wal"),
        dir.path().join("library.json"),
    ];
    let before: Vec<(bool, u64, Option<std::time::SystemTime>)> = watched
        .iter()
        .map(|path| {
            let meta = fs::metadata(path).ok();
            (
                path.exists(),
                meta.as_ref().map(|m| m.len()).unwrap_or(0),
                meta.and_then(|m| m.modified().ok()),
            )
        })
        .collect();
    let items_before = lib.count_items().unwrap();

    lib.cleanup_orphans().unwrap();

    for (idx, path) in watched.iter().enumerate() {
        let meta = fs::metadata(path).ok();
        assert_eq!(
            path.exists(),
            before[idx].0,
            "{} presence changed",
            path.display()
        );
        assert_eq!(
            meta.as_ref().map(|m| m.len()).unwrap_or(0),
            before[idx].1,
            "{} size changed",
            path.display()
        );
        assert_eq!(
            meta.and_then(|m| m.modified().ok()),
            before[idx].2,
            "{} mtime changed",
            path.display()
        );
    }
    assert_eq!(lib.count_items().unwrap(), items_before);
    assert!(root_junk.exists());
    assert!(backup_junk.exists());
    assert!(tmp_junk.exists());
}

#[test]
fn checkpoint_succeeds_and_truncates_wal() {
    let (_dir, lib) = library();
    for n in 0..3 {
        lib.insert_item(new_item(n, "originals/images/a.png"))
            .unwrap();
    }

    lib.checkpoint().unwrap();
    let wal = lib.root().join("visrecall.db-wal");
    assert_eq!(size_of(&wal), 0, "-wal should be truncated");
}

#[test]
fn checkpoint_reports_busy_when_reader_holds_snapshot() {
    let (dir, lib) = library();
    lib.insert_item(new_item(1, "originals/images/a.png"))
        .unwrap();

    let reader = Connection::open(dir.path().join("visrecall.db")).unwrap();
    reader.execute_batch("BEGIN;").unwrap();
    let _count: i64 = reader
        .query_row("SELECT COUNT(*) FROM items", [], |row| row.get(0))
        .unwrap();

    match lib.checkpoint() {
        Err(AppError::Busy(message)) => assert!(message.contains("busy"), "{message}"),
        other => panic!("expected Busy, got {other:?}"),
    }

    reader.execute_batch("COMMIT;").unwrap();
    // 读者释放后同一调用成功。
    lib.checkpoint().unwrap();
}
