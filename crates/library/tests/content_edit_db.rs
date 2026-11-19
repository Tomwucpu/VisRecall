//! 手工编辑 AI 描述 / OCR 文本 / 正文的集成测试（P2-R15、P2-R16）。

use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use visrecall_domain::insight::{OcrBox, SummarySource};
use visrecall_domain::item::{ItemKind, NewItem};
use visrecall_domain::{AppError, Insight};
use visrecall_library::insights::{MAX_OCR_CHARS, MAX_SUMMARY_CHARS};
use visrecall_library::items::MAX_BODY_CHARS;
use visrecall_library::{Library, NewImportSource};

fn library() -> (tempfile::TempDir, Library) {
    let dir = tempfile::tempdir().unwrap();
    let lib = Library::open(dir.path().to_path_buf()).unwrap();
    (dir, lib)
}

fn new_item(kind: ItemKind, n: i64, original_path: &str) -> NewItem {
    NewItem {
        kind,
        captured_at: 1_700_000_000_000,
        source_app: Some("test".into()),
        source_title: Some("test".into()),
        source_url: None,
        width: None,
        height: None,
        char_count: None,
        original_path: original_path.to_string(),
        content_hash: format!("hash-{n}"),
    }
}

fn image_item(lib: &Library, n: i64) -> String {
    lib.insert_item(new_item(
        ItemKind::Image,
        n,
        &format!("originals/images/2026/11/{n}.png"),
    ))
    .unwrap()
    .id
}

/// 写一个真实的库内文本文件再入库，保证正文读写走真实路径。
fn text_item(lib: &Library, n: i64, body: &str) -> String {
    let relative = format!("texts/2026/11/{n}.txt");
    let path = lib.root().join(&relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, body).unwrap();
    lib.insert_item(new_item(ItemKind::Text, n, &relative))
        .unwrap()
        .id
}

fn hook_calls(lib: &Library) -> Arc<AtomicUsize> {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    lib.set_metadata_hook(Some(Arc::new(move |_ids: &[String]| {
        counter.fetch_add(1, Ordering::SeqCst);
    })));
    calls
}

#[test]
fn summary_write_sets_user_edited_and_keeps_source() {
    let (_dir, lib) = library();
    let id = image_item(&lib, 1);
    let existing = Insight {
        summary: Some("模型写的描述".into()),
        summary_source: SummarySource::Vlm,
        ..Insight::empty(id.as_str(), 0)
    };
    lib.insight_upsert(&existing).unwrap();

    lib.set_summary(&id, "  用户改的描述  ").unwrap();

    let insight = lib.insight_get(&id).unwrap().unwrap();
    assert_eq!(insight.summary.as_deref(), Some("用户改的描述"));
    assert_eq!(insight.summary_source, SummarySource::Vlm, "来源标记不变");
    assert!(insight.summary_user_edited);
}

#[test]
fn summary_accepts_exactly_the_limit_and_rejects_overflow() {
    let (_dir, lib) = library();
    let id = image_item(&lib, 1);

    let exact = "字".repeat(MAX_SUMMARY_CHARS);
    lib.set_summary(&id, &exact).unwrap();
    assert_eq!(
        lib.insight_get(&id)
            .unwrap()
            .unwrap()
            .summary
            .unwrap()
            .chars()
            .count(),
        MAX_SUMMARY_CHARS
    );

    let too_long = "字".repeat(MAX_SUMMARY_CHARS + 1);
    assert!(matches!(
        lib.set_summary(&id, &too_long),
        Err(AppError::InvalidInput(_))
    ));
    assert_eq!(
        lib.insight_get(&id).unwrap().unwrap().summary.as_deref(),
        Some(exact.as_str())
    );
}

#[test]
fn empty_summary_clears_it() {
    let (_dir, lib) = library();
    let id = image_item(&lib, 1);
    lib.set_summary(&id, "先写一段").unwrap();
    lib.set_summary(&id, "   ").unwrap();

    let insight = lib.insight_get(&id).unwrap().unwrap();
    assert!(insight.summary.is_none());
    assert!(insight.summary_user_edited);
}

#[test]
fn summary_on_missing_item_is_not_found() {
    let (_dir, lib) = library();
    assert!(matches!(
        lib.set_summary("no-such-item", "x"),
        Err(AppError::NotFound)
    ));
}

#[test]
fn ocr_write_keeps_boxes_and_sets_flag() {
    let (_dir, lib) = library();
    let id = image_item(&lib, 1);
    let existing = Insight {
        ocr_text: Some("识别错的文字".into()),
        ocr_boxes: vec![OcrBox::new("识别错的文字", 0.1, 0.1, 0.2, 0.05)],
        ocr_engine: Some("test-ocr".into()),
        ocr_confidence: Some(0.92),
        ..Insight::empty(id.as_str(), 0)
    };
    lib.insight_upsert(&existing).unwrap();

    lib.set_ocr_text(&id, "改对的文字").unwrap();

    let insight = lib.insight_get(&id).unwrap().unwrap();
    assert_eq!(insight.ocr_text.as_deref(), Some("改对的文字"));
    assert_eq!(insight.ocr_boxes.len(), 1, "文字框保留，不重算");
    assert_eq!(insight.ocr_boxes[0].text, "识别错的文字");
    assert_eq!(insight.ocr_engine.as_deref(), Some("test-ocr"));
    assert!(insight.ocr_user_edited);
}

#[test]
fn ocr_write_rejects_non_image_items() {
    let (_dir, lib) = library();
    let id = text_item(&lib, 1, "正文");
    assert!(matches!(
        lib.set_ocr_text(&id, "x"),
        Err(AppError::InvalidInput(_))
    ));
}

#[test]
fn ocr_write_rejects_overflow_and_keeps_old_value() {
    let (_dir, lib) = library();
    let id = image_item(&lib, 1);
    lib.set_ocr_text(&id, "保留我").unwrap();

    let too_long = "字".repeat(MAX_OCR_CHARS + 1);
    assert!(matches!(
        lib.set_ocr_text(&id, &too_long),
        Err(AppError::InvalidInput(_))
    ));
    assert_eq!(
        lib.insight_get(&id).unwrap().unwrap().ocr_text.as_deref(),
        Some("保留我")
    );
}

#[test]
fn body_write_replaces_the_file_and_fires_the_hook() {
    let (_dir, lib) = library();
    let id = text_item(&lib, 1, "旧正文");
    let calls = hook_calls(&lib);

    lib.set_body_text(&id, "新正文").unwrap();

    let item = lib.get_item(&id).unwrap().unwrap();
    let stored = fs::read_to_string(lib.root().join(&item.original_path)).unwrap();
    assert_eq!(stored, "新正文");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "正文变更必须触发重索引");
}

#[test]
fn body_write_rejects_external_reference_items() {
    let (_dir, lib) = library();
    let source = lib
        .create_import_source(NewImportSource {
            root_path: "C:\\pics".into(),
            include_subdirectories: true,
            include_images: true,
            include_text: true,
        })
        .unwrap();
    let item = lib
        .insert_reference_item(
            &source.id,
            "notes/a.txt",
            3,
            0,
            new_item(ItemKind::Text, 1, ""),
        )
        .unwrap();

    assert!(matches!(
        lib.set_body_text(&item.id, "回写外部原件"),
        Err(AppError::InvalidInput(_))
    ));
}

#[test]
fn body_write_rejects_documents_longer_than_the_display_limit() {
    let (_dir, lib) = library();
    let long = "字".repeat(MAX_BODY_CHARS + 1);
    let id = text_item(&lib, 1, &long);

    assert!(matches!(
        lib.set_body_text(&id, "试图写回"),
        Err(AppError::InvalidInput(_))
    ));
    // 文件与库都保持原样
    let item = lib.get_item(&id).unwrap().unwrap();
    let stored = fs::read_to_string(lib.root().join(&item.original_path)).unwrap();
    assert_eq!(stored.chars().count(), MAX_BODY_CHARS + 1);
}

#[test]
fn body_write_rejects_non_text_items() {
    let (_dir, lib) = library();
    let id = image_item(&lib, 1);
    assert!(matches!(
        lib.set_body_text(&id, "x"),
        Err(AppError::InvalidInput(_))
    ));
}

#[test]
fn body_write_recreates_a_missing_file() {
    let (_dir, lib) = library();
    let id = text_item(&lib, 1, "旧正文");
    let item = lib.get_item(&id).unwrap().unwrap();
    fs::remove_file(lib.root().join(&item.original_path)).unwrap();

    lib.set_body_text(&id, "补录的正文").unwrap();

    let stored = fs::read_to_string(lib.root().join(&item.original_path)).unwrap();
    assert_eq!(stored, "补录的正文");
}

#[test]
fn body_write_on_missing_item_is_not_found() {
    let (_dir, lib) = library();
    assert!(matches!(
        lib.set_body_text("no-such-item", "x"),
        Err(AppError::NotFound)
    ));
}

#[test]
fn summary_and_ocr_writes_fire_the_hook() {
    let (_dir, lib) = library();
    let id = image_item(&lib, 1);
    let calls = hook_calls(&lib);

    lib.set_summary(&id, "人工描述").unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1, "摘要变更必须重索引");

    lib.set_ocr_text(&id, "人工 OCR").unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2, "OCR 变更必须重索引");
}

#[test]
fn empty_body_clears_the_file() {
    let (_dir, lib) = library();
    let id = text_item(&lib, 1, "旧正文");
    let item = lib.get_item(&id).unwrap().unwrap();

    lib.set_body_text(&id, "").unwrap();

    assert_eq!(
        fs::read_to_string(lib.root().join(&item.original_path)).unwrap(),
        ""
    );
}

#[test]
fn empty_ocr_clears_the_text_but_keeps_boxes() {
    let (_dir, lib) = library();
    let id = image_item(&lib, 1);
    let existing = Insight {
        ocr_text: Some("旧文字".into()),
        ocr_boxes: vec![OcrBox::new("旧文字", 0.1, 0.1, 0.2, 0.05)],
        ..Insight::empty(id.as_str(), 0)
    };
    lib.insight_upsert(&existing).unwrap();

    lib.set_ocr_text(&id, "").unwrap();

    let insight = lib.insight_get(&id).unwrap().unwrap();
    assert_eq!(insight.ocr_text.as_deref(), Some(""));
    assert_eq!(insight.ocr_boxes.len(), 1, "清空文本不动文字框");
}

#[test]
fn body_write_failure_does_not_fire_the_hook() {
    let (_dir, lib) = library();
    let id = text_item(&lib, 1, "旧正文");
    // 把 texts 目录换成同名文件：create_parent_dir 必然失败，模拟磁盘/权限错误。
    fs::remove_dir_all(lib.root().join("texts")).unwrap();
    fs::write(lib.root().join("texts"), b"blocked").unwrap();
    let calls = hook_calls(&lib);

    let err = lib.set_body_text(&id, "新正文").unwrap_err();
    assert!(
        matches!(err, AppError::Io(_)),
        "写文件失败应返回 IO：{err:?}"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0, "写文件失败不得触发重索引");
}
