//! 库迁移的文件层测试：目标校验、幂等复制与复制后校验。

use std::fs;
use std::path::Path;

use visrecall_domain::item::{ItemKind, NewItem};
use visrecall_domain::AppError;
use visrecall_library::relocate::{copy_library, is_within, validate_target, verify};
use visrecall_library::Library;

fn new_item(n: i64) -> NewItem {
    NewItem {
        kind: ItemKind::Image,
        captured_at: 1_700_000_000_000 + n,
        source_app: None,
        source_title: None,
        source_url: None,
        width: Some(10),
        height: Some(10),
        char_count: None,
        original_path: format!("originals/images/2026/11/{n}.png"),
        content_hash: format!("hash-{n}"),
    }
}

fn write_file(root: &Path, rel: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, b"payload").unwrap();
}

/// 造一个可迁移的源库：2 条素材 + 三个内容目录各一个文件。
fn source_library() -> (tempfile::TempDir, Library, u64) {
    let dir = tempfile::tempdir().unwrap();
    let lib = Library::open(dir.path().to_path_buf()).unwrap();
    for n in 0..2 {
        lib.insert_item(new_item(n)).unwrap();
    }
    lib.checkpoint().unwrap();

    write_file(dir.path(), "originals/images/2026/11/0.png");
    write_file(dir.path(), "texts/0.txt");
    write_file(dir.path(), "thumbs/0.png");

    let items = lib.count_items().unwrap();
    (dir, lib, items)
}

fn library_id(root: &Path) -> String {
    let text = fs::read_to_string(root.join("library.json")).unwrap();
    let meta: visrecall_library::LibraryMeta = serde_json::from_str(&text).unwrap();
    meta.library_id
}

#[test]
fn component_wise_prefix_comparison() {
    let src = Path::new("C:\\lib");
    assert!(is_within(Path::new("C:\\lib\\sub"), src));
    assert!(is_within(src, src));
    // 字符串前缀相同但不是子目录。
    assert!(!is_within(Path::new("C:\\lib2"), src));
    assert!(!is_within(Path::new("C:\\lib2\\sub"), src));
}

#[test]
fn rejects_relative_target() {
    let (_dir, lib, used) = source_library();
    let err = validate_target(lib.root(), Path::new("relative\\lib"), used).unwrap_err();
    assert!(matches!(err, AppError::InvalidInput(_)));
    assert!(err.to_string().contains("absolute"));
}

#[test]
fn rejects_target_inside_source_and_source_inside_target() {
    let (dir, lib, used) = source_library();
    let nested = dir.path().join("nested");
    let err = validate_target(lib.root(), &nested, used).unwrap_err();
    assert!(matches!(err, AppError::InvalidInput(_)));

    let parent = dir.path().parent().unwrap().to_path_buf();
    let err = validate_target(lib.root(), &parent, used).unwrap_err();
    assert!(matches!(err, AppError::InvalidInput(_)));
}

#[test]
fn rejects_same_path_and_non_empty_target() {
    let (dir, lib, used) = source_library();
    let err = validate_target(lib.root(), lib.root(), used).unwrap_err();
    assert!(matches!(err, AppError::InvalidInput(_)));

    let target = tempfile::tempdir().unwrap();
    fs::write(target.path().join("occupied.txt"), b"x").unwrap();
    let err = validate_target(lib.root(), target.path(), used).unwrap_err();
    assert!(err.to_string().contains("not empty"), "{err}");
    assert!(dir.path().exists());
}

#[test]
fn rejects_when_target_volume_lacks_space() {
    let (_dir, lib, _items) = source_library();
    let target = tempfile::tempdir().unwrap();
    // 要求 1e18 字节 → 任何盘都不够。
    let err = validate_target(lib.root(), target.path(), 1_000_000_000_000_000_000).unwrap_err();
    assert!(matches!(err, AppError::InvalidInput(_)));
    assert!(err.to_string().contains("free"), "{err}");
}

#[test]
fn accepts_an_empty_directory() {
    let (_dir, lib, used) = source_library();
    let target = tempfile::tempdir().unwrap();
    validate_target(lib.root(), target.path(), used).unwrap();
}

#[test]
fn copies_main_files_and_content_dirs_then_verifies() {
    let (dir, lib, items) = source_library();
    let target = tempfile::tempdir().unwrap();
    let id = library_id(dir.path());

    let report = copy_library(lib.root(), target.path(), &|_phase, _done, _total| {}).unwrap();
    assert!(report.files_copied >= 5, "{report:?}");

    // 主库与其元数据都在；`-wal` 不在（已 checkpoint，复制它反而带回旧 WAL）。
    assert!(target.path().join("library.json").is_file());
    assert!(target.path().join("visrecall.db").is_file());
    assert!(!target.path().join("visrecall.db-wal").exists());
    assert!(target
        .path()
        .join("originals/images/2026/11/0.png")
        .is_file());

    verify(target.path(), items, &id).unwrap();
}

#[test]
fn second_copy_skips_everything() {
    let (dir, lib, _items) = source_library();
    let target = tempfile::tempdir().unwrap();
    let first = copy_library(lib.root(), target.path(), &|_, _, _| {}).unwrap();
    assert!(first.files_copied > 0);

    let second = copy_library(lib.root(), target.path(), &|_, _, _| {}).unwrap();
    assert_eq!(second.files_copied, 0, "identical files must be skipped");
    assert_eq!(second.bytes_copied, 0);
    let _ = dir;
}

#[test]
fn verify_detects_item_count_mismatch() {
    let (dir, lib, items) = source_library();
    let target = tempfile::tempdir().unwrap();
    let id = library_id(dir.path());
    copy_library(lib.root(), target.path(), &|_, _, _| {}).unwrap();

    let err = verify(target.path(), items + 1, &id).unwrap_err();
    assert!(err.to_string().contains("count"), "{err}");
}

#[test]
fn verify_detects_library_id_mismatch() {
    let (dir, lib, items) = source_library();
    let target = tempfile::tempdir().unwrap();
    copy_library(lib.root(), target.path(), &|_, _, _| {}).unwrap();

    let err = verify(target.path(), items, "another-library").unwrap_err();
    assert!(err.to_string().contains("library_id"), "{err}");
    let _ = dir;
}

/// 只复制主库（不 checkpoint）的错误做法必须被行数校验拦住。
#[test]
fn copying_only_the_db_without_checkpoint_fails_verification() {
    let (dir, lib, items) = source_library();
    let target = tempfile::tempdir().unwrap();
    let id = library_id(dir.path());

    // 模拟「只复制 visrecall.db，且 WAL 里还有未落盘的数据」：
    // 新写入一条素材，不 checkpoint，直接复制 db 文件。
    lib.insert_item(new_item(99)).unwrap();
    fs::copy(
        dir.path().join("visrecall.db"),
        target.path().join("visrecall.db"),
    )
    .unwrap();
    fs::copy(
        dir.path().join("library.json"),
        target.path().join("library.json"),
    )
    .unwrap();

    // 源库现在有 items+1 条，而复制出来的主库缺最后一条（或直接打不开）。
    let err = verify(target.path(), items + 1, &id).unwrap_err();
    let message = err.to_string();
    assert!(
        message.contains("count") || message.contains("no such table") || message.contains("items"),
        "unexpected error: {message}"
    );
}

#[test]
fn progress_reports_prepare_then_copy() {
    let (dir, lib, _items) = source_library();
    let target = tempfile::tempdir().unwrap();
    let phases = std::sync::Mutex::new(Vec::<String>::new());

    copy_library(lib.root(), target.path(), &|phase, _done, _total| {
        phases.lock().unwrap().push(phase.to_string());
    })
    .unwrap();

    let phases = phases.into_inner().unwrap();
    assert_eq!(phases.first().map(String::as_str), Some("prepare"));
    assert!(phases.iter().any(|phase| phase == "copy"));
    let _ = dir;
}
