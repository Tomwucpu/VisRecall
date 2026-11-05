//! 捕获落盘闭环的集成测试：真实临时库 + 假抓帧数据。
//!
//! 覆盖 design §10.2 的写入顺序（tmp → sync → rename）、PNG 内容（alpha 固定
//! 255）、内容哈希稳定性与数据库里的相对路径。

use std::path::Path;

use sha2::{Digest, Sha256};
use visrecall_app_core::capture_flow::persist_capture;
use visrecall_domain::capture::{CapturedImage, ForegroundInfo};
use visrecall_domain::geometry::PhysicalRect;
use visrecall_domain::item::ItemStatus;
use visrecall_library::Library;

fn test_image() -> CapturedImage {
    // 2×1：红、绿（BGRA，alpha 位故意留 0 以验证编码时固定为 255）。
    CapturedImage {
        width: 2,
        height: 1,
        bgra: vec![0, 0, 255, 0, 0, 255, 0, 0],
    }
}

fn test_foreground() -> ForegroundInfo {
    ForegroundInfo {
        hwnd: 0,
        pid: 1,
        process_name: "chrome.exe".into(),
        display_name: "Chrome".into(),
        title: "Test".into(),
        bounds: PhysicalRect::new(0, 0, 2, 1),
    }
}

fn open_library(dir: &Path) -> Library {
    Library::open(dir.to_path_buf()).expect("open temp library")
}

#[test]
fn persist_writes_png_and_inserts_item() {
    let dir = tempfile::tempdir().unwrap();
    let library = open_library(dir.path());

    let item = persist_capture(test_image(), test_foreground(), &library).unwrap();

    assert_eq!(item.status, ItemStatus::Parsing);
    assert!(item.original_path.starts_with("originals/images/"));
    assert!(dir.path().join(&item.original_path).exists());
    assert!(library.get_item(&item.id).unwrap().is_some());
    assert_eq!(item.width, Some(2));
    assert_eq!(item.height, Some(1));
    assert_eq!(item.source_app.as_deref(), Some("chrome.exe"));
    assert_eq!(item.source_title.as_deref(), Some("Test"));
    // 文件名与素材 id 一致（Task 17 的缩略图按 id 定位原图）。
    assert_eq!(
        Path::new(&item.original_path).file_stem().unwrap(),
        std::ffi::OsStr::new(&item.id)
    );
    // 临时文件已改名，不留残余。
    assert!(!dir
        .path()
        .join("tmp")
        .join(format!("{}.tmp", item.id))
        .exists());
}

#[test]
fn persisted_png_is_rgba_with_opaque_alpha() {
    let dir = tempfile::tempdir().unwrap();
    let library = open_library(dir.path());

    let item = persist_capture(test_image(), test_foreground(), &library).unwrap();
    let bytes = std::fs::read(dir.path().join(&item.original_path)).unwrap();

    // PNG 魔数。
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");

    let decoded = image::load_from_memory(&bytes)
        .expect("decode persisted png")
        .to_rgba8();
    assert_eq!(decoded.width(), 2);
    assert_eq!(decoded.height(), 1);
    // BGRA (B=0,G=0,R=255) → RGBA 红；alpha 固定 255（原始 alpha 位是 0）。
    assert_eq!(decoded.get_pixel(0, 0).0, [255, 0, 0, 255]);
    // BGRA 绿 (0,255,0) → RGBA 绿。
    assert_eq!(decoded.get_pixel(1, 0).0, [0, 255, 0, 255]);
}

#[test]
fn content_hash_matches_file_and_is_stable() {
    let dir = tempfile::tempdir().unwrap();
    let library = open_library(dir.path());

    let first = persist_capture(test_image(), test_foreground(), &library).unwrap();
    let second = persist_capture(test_image(), test_foreground(), &library).unwrap();

    // 不同素材 id、同一画面 → 哈希相同（按内容去重/检索的前提）。
    assert_ne!(first.id, second.id);
    assert_eq!(first.content_hash, second.content_hash);

    let bytes = std::fs::read(dir.path().join(&first.original_path)).unwrap();
    assert_eq!(first.content_hash, format!("{:x}", Sha256::digest(&bytes)));
    assert_eq!(first.content_hash.len(), 64);
}

#[test]
fn database_stores_relative_image_path_under_year_month() {
    let dir = tempfile::tempdir().unwrap();
    let library = open_library(dir.path());

    let item = persist_capture(test_image(), test_foreground(), &library).unwrap();
    let stored = library.get_item(&item.id).unwrap().expect("item row");

    assert_eq!(stored.original_path, item.original_path);
    let path = Path::new(&stored.original_path);
    assert!(path.is_relative(), "original_path must be relative");

    let components: Vec<String> = path
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect();
    assert_eq!(components.len(), 5, "got {components:?}");
    assert_eq!(components[0], "originals");
    assert_eq!(components[1], "images");
    assert_eq!(components[2].len(), 4, "year: {components:?}");
    assert!(components[2].chars().all(|ch| ch.is_ascii_digit()));
    assert_eq!(components[3].len(), 2, "month: {components:?}");
    assert!(components[3].chars().all(|ch| ch.is_ascii_digit()));
    assert_eq!(components[4], format!("{}.png", item.id));
}

#[test]
fn rejects_mismatched_buffer_size() {
    let dir = tempfile::tempdir().unwrap();
    let library = open_library(dir.path());

    let image = CapturedImage {
        width: 4,
        height: 4,
        bgra: vec![0; 8],
    };
    let err = persist_capture(image, test_foreground(), &library).unwrap_err();
    assert!(matches!(err, visrecall_domain::AppError::InvalidInput(_)));
    // 失败不写库、不留临时文件。
    assert!(library.list_items(None, 10).unwrap().is_empty());
    assert!(std::fs::read_dir(dir.path().join("tmp"))
        .unwrap()
        .next()
        .is_none());
}
