//! Task 17：缩略图生成（长边 480）与 `load_thumb_bytes` 的读取/错误分支。

use std::fs;
use std::path::{Path, PathBuf};

use image::{Rgba, RgbaImage};
use visrecall_app_core::thumbnail::{generate_thumbnail, load_thumb_bytes, THUMB_MAX_EDGE};
use visrecall_domain::item::{ItemKind, ItemStatus, NewItem};
use visrecall_domain::AppError;
use visrecall_library::Library;

#[test]
fn thumbnail_long_edge_is_480() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.png");
    let dst = dir.path().join("dst.png");
    let img = RgbaImage::from_pixel(1000, 500, Rgba([255, 0, 0, 255]));
    img.save(&src).unwrap();

    generate_thumbnail(&src, &dst, 480).unwrap();
    let out = image::open(&dst).unwrap();
    assert_eq!(out.width(), 480);
    assert_eq!(out.height(), 240);
}

/// 生成缩略图时父目录不存在也要建好，且不留临时文件。
#[test]
fn thumbnail_creates_the_parent_directory() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.png");
    RgbaImage::from_pixel(60, 30, Rgba([0, 0, 255, 255]))
        .save(&src)
        .unwrap();

    let dst = dir.path().join("thumbs/nested/out.png");
    generate_thumbnail(&src, &dst, THUMB_MAX_EDGE).unwrap();

    assert!(dst.exists());
    let leftovers: Vec<PathBuf> = fs::read_dir(dst.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "tmp"))
        .collect();
    assert!(leftovers.is_empty(), "temp file left behind: {leftovers:?}");
}

/// 源图不存在：返回错误且不产生目标文件。
#[test]
fn missing_source_fails_without_writing() {
    let dir = tempfile::tempdir().unwrap();
    let dst = dir.path().join("dst.png");

    let err = generate_thumbnail(&dir.path().join("nope.png"), &dst, 480).unwrap_err();
    assert!(matches!(err, AppError::Io(_)), "unexpected error: {err:?}");
    assert!(!dst.exists());
}

fn open_library(dir: &Path) -> Library {
    Library::open(dir.to_path_buf()).expect("open temp library")
}

/// 插入一条图片素材，写入库内的原图，并返回 `(library, item_id, thumb_relative)`。
fn image_with_thumb(dir: &Path) -> (Library, String) {
    let library = open_library(dir);
    let id = "thumb-item".to_string();
    let original = format!("originals/images/2026/01/{id}.png");
    let rel_thumb = format!("thumbs/{id}.png");

    library
        .insert_item_with_id(
            id.clone(),
            NewItem {
                kind: ItemKind::Image,
                captured_at: 1,
                source_app: None,
                source_title: None,
                source_url: None,
                width: Some(1000),
                height: Some(500),
                char_count: None,
                original_path: original.clone(),
                content_hash: "hash".into(),
            },
        )
        .unwrap();

    let source = dir.join("source.png");
    RgbaImage::from_pixel(1000, 500, Rgba([9, 9, 9, 255]))
        .save(&source)
        .unwrap();
    fs::create_dir_all(dir.join(&original).parent().unwrap()).unwrap();
    fs::copy(&source, dir.join(&original)).unwrap();

    generate_thumbnail(&dir.join(&original), &dir.join(&rel_thumb), THUMB_MAX_EDGE).unwrap();
    library
        .update_status(&id, ItemStatus::Ready, Some(&rel_thumb))
        .unwrap();

    (library, id)
}

#[test]
fn load_thumb_bytes_returns_the_png() {
    let dir = tempfile::tempdir().unwrap();
    let (library, id) = image_with_thumb(dir.path());

    let bytes = load_thumb_bytes(&library, &id).expect("read thumbnail");
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
    assert_eq!(
        bytes,
        fs::read(dir.path().join(format!("thumbs/{id}.png"))).unwrap()
    );
}

#[test]
fn load_thumb_bytes_reports_unknown_items() {
    let dir = tempfile::tempdir().unwrap();
    let library = open_library(dir.path());

    let err = load_thumb_bytes(&library, "does-not-exist").unwrap_err();
    assert!(
        matches!(err, AppError::NotFound),
        "unexpected error: {err:?}"
    );
}

/// 素材没有缩略图（`thumb_path` 为空，例如仍在 `parsing`）→ NotFound。
#[test]
fn load_thumb_bytes_reports_items_without_a_thumb() {
    let dir = tempfile::tempdir().unwrap();
    let library = open_library(dir.path());
    let item = library
        .insert_item(NewItem {
            kind: ItemKind::Image,
            captured_at: 1,
            source_app: None,
            source_title: None,
            source_url: None,
            width: Some(10),
            height: Some(10),
            char_count: None,
            original_path: "originals/images/2026/01/a.png".into(),
            content_hash: "hash".into(),
        })
        .unwrap();

    let err = load_thumb_bytes(&library, &item.id).unwrap_err();
    assert!(
        matches!(err, AppError::NotFound),
        "unexpected error: {err:?}"
    );
}

/// `thumb_path` 有值但文件已被删：NotFound，不 panic。
#[test]
fn load_thumb_bytes_reports_missing_files() {
    let dir = tempfile::tempdir().unwrap();
    let (library, id) = image_with_thumb(dir.path());
    fs::remove_file(dir.path().join(format!("thumbs/{id}.png"))).unwrap();

    let err = load_thumb_bytes(&library, &id).unwrap_err();
    assert!(
        matches!(err, AppError::NotFound),
        "unexpected error: {err:?}"
    );
}

/// 库里被写坏的绝对路径不得读到库外文件。
#[test]
fn load_thumb_bytes_rejects_paths_outside_the_library() {
    let dir = tempfile::tempdir().unwrap();
    let library = open_library(dir.path());
    let outside = dir.path().join("outside.png");
    fs::write(&outside, b"secret").unwrap();

    let item = library
        .insert_item(NewItem {
            kind: ItemKind::Image,
            captured_at: 1,
            source_app: None,
            source_title: None,
            source_url: None,
            width: None,
            height: None,
            char_count: None,
            original_path: "originals/images/2026/01/a.png".into(),
            content_hash: "hash".into(),
        })
        .unwrap();
    library
        .update_status(&item.id, ItemStatus::Ready, Some("../outside.png"))
        .unwrap();

    let err = load_thumb_bytes(&library, &item.id).unwrap_err();
    assert!(
        matches!(err, AppError::InvalidInput(_)),
        "unexpected error: {err:?}"
    );
}

/// 纵深防御：`thumb_path` 指向库内的 `texts/` 等非缩略图文件时同样拒绝（不读库内任意文件）。
#[test]
fn load_thumb_bytes_rejects_paths_outside_thumbs() {
    let dir = tempfile::tempdir().unwrap();
    let library = open_library(dir.path());
    let item = library
        .insert_item(NewItem {
            kind: ItemKind::Image,
            captured_at: 1,
            source_app: None,
            source_title: None,
            source_url: None,
            width: None,
            height: None,
            char_count: None,
            original_path: "originals/images/2026/01/a.png".into(),
            content_hash: "hash".into(),
        })
        .unwrap();

    for thumb in [
        format!("texts/{}.txt", item.id),
        format!("thumbs\\{}.png", item.id),
        "thumbs".to_string(),
        "thumbs/".to_string(),
        "thumbs-other/a.png".to_string(),
    ] {
        let text_target = dir.path().join(&thumb);
        if let Some(parent) = text_target.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&text_target, b"secret").ok();

        library
            .update_status(&item.id, ItemStatus::Ready, Some(&thumb))
            .unwrap();
        let err = load_thumb_bytes(&library, &item.id).unwrap_err();
        assert!(
            matches!(err, AppError::InvalidInput(_)),
            "thumb {thumb:?} must be rejected, got {err:?}"
        );
    }
}
