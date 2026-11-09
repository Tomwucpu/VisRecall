//! 详情聚合的集成测试：原文截断、协议 URL 与标签。

use std::fs;

use visrecall_app_core::detail::{
    load_detail, read_preview, BODY_TEXT_LIMIT, PREVIEW_CHARS, TRUNCATED_MARKER,
};
use visrecall_domain::item::{ItemKind, NewItem};
use visrecall_domain::AppError;
use visrecall_library::Library;

fn item(n: i64, kind: ItemKind, original: &str) -> NewItem {
    NewItem {
        kind,
        captured_at: 1_700_000_000_000 + n,
        source_app: Some("chrome.exe".into()),
        source_title: Some("标题".into()),
        source_url: Some("https://example.com/a".into()),
        width: Some(1920),
        height: Some(1080),
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

fn insert_text(lib: &Library, root: &std::path::Path, body: &str) -> String {
    let created = lib
        .insert_item(item(1, ItemKind::Text, "texts/a.txt"))
        .unwrap();
    fs::create_dir_all(root.join("texts")).unwrap();
    fs::write(root.join("texts/a.txt"), body).unwrap();
    created.id
}

#[test]
fn text_below_limit_is_not_truncated() {
    let (dir, lib) = library();
    let body = "字".repeat(BODY_TEXT_LIMIT - 1);
    let id = insert_text(&lib, dir.path(), &body);

    let detail = load_detail(&lib, &id).unwrap();
    let text = detail.extras.body_text.unwrap();
    assert_eq!(text.chars().count(), BODY_TEXT_LIMIT - 1);
    assert!(!text.ends_with(TRUNCATED_MARKER));
}

#[test]
fn text_exactly_at_limit_is_not_truncated() {
    let (dir, lib) = library();
    let body = "字".repeat(BODY_TEXT_LIMIT);
    let id = insert_text(&lib, dir.path(), &body);

    let detail = load_detail(&lib, &id).unwrap();
    let text = detail.extras.body_text.unwrap();
    assert_eq!(text.chars().count(), BODY_TEXT_LIMIT);
    assert!(!text.contains(TRUNCATED_MARKER));
}

#[test]
fn text_over_limit_is_truncated_with_marker() {
    let (dir, lib) = library();
    let body = "字".repeat(BODY_TEXT_LIMIT + 1);
    let id = insert_text(&lib, dir.path(), &body);

    let detail = load_detail(&lib, &id).unwrap();
    let text = detail.extras.body_text.unwrap();
    assert!(text.ends_with(TRUNCATED_MARKER));
    assert_eq!(
        text.trim_end_matches(TRUNCATED_MARKER).chars().count(),
        BODY_TEXT_LIMIT
    );
}

#[test]
fn truncation_point_survives_emoji() {
    let (dir, lib) = library();
    // 第 20000 个字符是 emoji，后面还有内容：截断必须落在完整字符边界。
    let mut body = "字".repeat(BODY_TEXT_LIMIT - 1);
    body.push('😀');
    body.push_str("后面的内容不应出现");
    let id = insert_text(&lib, dir.path(), &body);

    let detail = load_detail(&lib, &id).unwrap();
    let text = detail.extras.body_text.unwrap();
    assert!(
        !text.contains('\u{fffd}'),
        "must not produce replacement chars"
    );
    let kept = text.trim_end_matches(TRUNCATED_MARKER);
    assert_eq!(kept.chars().count(), BODY_TEXT_LIMIT);
    assert!(kept.ends_with('😀'));
}

#[test]
fn image_has_original_url_and_no_body_text() {
    let (_dir, lib) = library();
    let created = lib
        .insert_item(item(1, ItemKind::Image, "originals/images/a.png"))
        .unwrap();

    let detail = load_detail(&lib, &created.id).unwrap();
    assert_eq!(
        detail.extras.original_url.as_deref(),
        Some(format!("visrecall://original/{}", created.id).as_str())
    );
    assert!(detail.extras.body_text.is_none());
    assert!(detail.extras.summary.is_none());
}

#[test]
fn link_has_neither_body_text_nor_original_url() {
    let (_dir, lib) = library();
    let created = lib
        .insert_item(item(1, ItemKind::Link, "texts/a.txt"))
        .unwrap();

    let detail = load_detail(&lib, &created.id).unwrap();
    assert!(detail.extras.body_text.is_none());
    assert!(detail.extras.original_url.is_none());
    assert_eq!(
        detail.item.source_url.as_deref(),
        Some("https://example.com/a")
    );
}

#[test]
fn missing_original_file_still_returns_detail() {
    let (dir, lib) = library();
    let created = lib
        .insert_item(item(1, ItemKind::Image, "originals/images/missing.png"))
        .unwrap();

    let detail = load_detail(&lib, &created.id).unwrap();
    assert!(detail.extras.original_url.is_some());

    // 文本素材的原文缺失时也不报错。
    let text_id = insert_text(&lib, dir.path(), "临时内容");
    fs::remove_file(dir.path().join("texts/a.txt")).unwrap();
    let detail = load_detail(&lib, &text_id).unwrap();
    assert!(detail.extras.body_text.is_none());
}

#[test]
fn tags_are_included_with_origin() {
    let (_dir, lib) = library();
    let created = lib
        .insert_item(item(1, ItemKind::Image, "originals/images/a.png"))
        .unwrap();
    lib.tag_add(&created.id, "work").unwrap();

    let detail = load_detail(&lib, &created.id).unwrap();
    assert_eq!(detail.extras.tags.len(), 1);
    assert_eq!(detail.extras.tags[0].tag.name, "work");
    assert_eq!(
        detail.extras.tags[0].origin,
        visrecall_domain::tag::TagSource::Manual
    );
    assert_eq!(detail.extras.tags[0].item_count, 0);
}

#[test]
fn missing_item_is_not_found() {
    let (_dir, lib) = library();
    assert!(matches!(
        load_detail(&lib, "no-such-item"),
        Err(AppError::NotFound)
    ));
}

#[test]
fn text_preview_reads_only_the_head_and_truncates_by_chars() {
    let (dir, lib) = library();
    let body = "字".repeat(5_000);
    let id = insert_text(&lib, dir.path(), &body);
    let stored = lib.get_item(&id).unwrap().unwrap();

    let preview = read_preview(&lib, &stored).unwrap();
    assert_eq!(preview.chars().count(), PREVIEW_CHARS);
    assert!(body.starts_with(&preview), "preview must be a prefix");
}

#[test]
fn text_preview_drops_a_duplicated_title_prefix() {
    let (dir, lib) = library();
    let body = format!("标题\n{}", "正文".repeat(50));
    let id = insert_text(&lib, dir.path(), &body);
    let stored = lib.get_item(&id).unwrap().unwrap();

    let preview = read_preview(&lib, &stored).unwrap();
    assert!(preview.starts_with("正文"), "{preview}");
    assert!(!preview.contains("标题"), "{preview}");
}

#[test]
fn preview_is_none_for_images_and_unreadable_text() {
    let (dir, lib) = library();
    let created = lib
        .insert_item(item(1, ItemKind::Image, "originals/images/a.png"))
        .unwrap();
    assert!(read_preview(&lib, &created).is_none());

    let text_id = insert_text(&lib, dir.path(), "临时内容");
    fs::remove_file(dir.path().join("texts/a.txt")).unwrap();
    let stored = lib.get_item(&text_id).unwrap().unwrap();
    assert!(read_preview(&lib, &stored).is_none());
}

#[test]
fn link_preview_uses_the_url_and_skips_links_without_one() {
    let (_dir, lib) = library();
    let link = lib
        .insert_item(item(1, ItemKind::Link, "texts/a.txt"))
        .unwrap();
    assert_eq!(read_preview(&lib, &link).as_deref(), Some("example.com/a"));

    let mut bare = item(2, ItemKind::Link, "");
    bare.source_url = None;
    let bare = lib.insert_item(bare).unwrap();
    assert!(read_preview(&lib, &bare).is_none());
}
