//! 云端解析开关的库内策略测试。
//!
//! 开关存库内 `settings` 表（与容量/保留同源）：属于「这个库」，
//! `config.json` 跟安装走，换库后会张冠李戴。

use visrecall_domain::item::{ItemKind, NewItem};
use visrecall_library::{Library, NewImportSource, SourceStatus};

fn new_item(kind: ItemKind, hash: &str) -> NewItem {
    NewItem {
        kind,
        captured_at: 0,
        source_app: None,
        source_title: Some("标题".into()),
        source_url: None,
        width: None,
        height: None,
        char_count: None,
        original_path: format!("originals/images/{hash}.png"),
        content_hash: hash.into(),
    }
}

fn library() -> (tempfile::TempDir, Library) {
    let dir = tempfile::tempdir().unwrap();
    let lib = Library::open(dir.path().to_path_buf()).unwrap();
    (dir, lib)
}

/// 默认关闭：未写入过任何值时必须保守（不发云端请求）。
#[test]
fn cloud_text_ai_defaults_to_disabled() {
    let (_dir, lib) = library();
    assert!(!lib.cloud_text_ai_enabled());
}

/// 写入后立即读回，并且跨「重开库」保持（值真的落盘而非只存内存）。
#[test]
fn cloud_text_ai_round_trips_and_persists() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    {
        let lib = Library::open(root.clone()).unwrap();
        lib.set_cloud_text_ai(true).unwrap();
        assert!(lib.cloud_text_ai_enabled());
    }

    let reopened = Library::open(root).unwrap();
    assert!(reopened.cloud_text_ai_enabled());

    // 关闭也要真的写回去（否则用户关不掉）。
    reopened.set_cloud_text_ai(false).unwrap();
    assert!(!reopened.cloud_text_ai_enabled());
}

/// 坏值回落默认 false，而不是把整个设置页卡住。
#[test]
fn cloud_text_ai_corrupted_value_falls_back_to_disabled() {
    let (_dir, lib) = library();
    lib.setting_set(
        visrecall_library::ai_policy::KEY_CLOUD_TEXT_AI,
        &"not-a-bool",
    )
    .unwrap();
    assert!(!lib.cloud_text_ai_enabled());
}

#[test]
fn cloud_image_ai_defaults_to_disabled_and_persists() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    {
        let lib = Library::open(root.clone()).unwrap();
        assert!(
            !lib.cloud_image_ai_enabled(),
            "默认必须关闭：图片不得自动上云"
        );
        lib.set_cloud_image_ai(true).unwrap();
        assert!(lib.cloud_image_ai_enabled());
    }

    let reopened = Library::open(root).unwrap();
    assert!(reopened.cloud_image_ai_enabled());
    reopened.set_cloud_image_ai(false).unwrap();
    assert!(!reopened.cloud_image_ai_enabled());
}

#[test]
fn cloud_image_ai_corrupted_value_falls_back_to_disabled() {
    let (_dir, lib) = library();
    lib.setting_set(
        visrecall_library::ai_policy::KEY_CLOUD_IMAGE_AI,
        &"not-a-bool",
    )
    .unwrap();
    assert!(!lib.cloud_image_ai_enabled());
}

/// 统一判定：链接永不上云；图片/文本各看自己的开关；来源离线时一律 false。
#[test]
fn cloud_parse_allowed_combines_kind_switch_and_source_status() {
    let dir = tempfile::tempdir().unwrap();
    let lib = Library::open(dir.path().join("library")).unwrap();
    lib.set_cloud_image_ai(true).unwrap();
    lib.set_cloud_text_ai(true).unwrap();

    let image = lib.insert_item(new_item(ItemKind::Image, "img")).unwrap();
    let text = lib.insert_item(new_item(ItemKind::Text, "txt")).unwrap();
    let link = lib.insert_item(new_item(ItemKind::Link, "link")).unwrap();

    assert!(lib.cloud_parse_allowed(&image).unwrap());
    assert!(lib.cloud_parse_allowed(&text).unwrap());
    assert!(
        !lib.cloud_parse_allowed(&link).unwrap(),
        "链接不参与自动云端解析"
    );

    // 引用素材：来源离线时即使开关开启也不得进入云端阶段。
    let source = lib
        .create_import_source(NewImportSource {
            root_path: dir.path().join("source").display().to_string(),
            include_subdirectories: true,
            include_images: true,
            include_text: true,
        })
        .unwrap();
    let referenced = lib
        .insert_reference_item(
            &source.id,
            "note.txt",
            1,
            0,
            new_item(ItemKind::Text, "ref"),
        )
        .unwrap();
    assert!(lib.cloud_parse_allowed(&referenced).unwrap());

    lib.set_import_source_status(&source.id, SourceStatus::Offline)
        .unwrap();
    assert!(!lib.cloud_parse_allowed(&referenced).unwrap());
    assert!(!lib.cloud_source_available(&referenced.id).unwrap());
}
