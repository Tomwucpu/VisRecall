//! 拖放分类的纯逻辑测试（design §9.2、P1-R09）。
//!
//! 这些用例不接触 OLE/Win32：`classify_drop` 只根据「可用格式 + 纯文本 + 文件路径」
//! 决定 `DropContent`，便于在 CI 上覆盖超限/URL/混合备注等分支。

use std::path::PathBuf;

use visrecall_shell::drop_target::{classify_drop, DropContent, DropFormats};

#[test]
fn url_becomes_link() {
    let formats = DropFormats {
        unicode_text: true,
        ..Default::default()
    };
    let content = classify_drop(&formats, Some("https://example.com"), &[]).unwrap();
    match content {
        DropContent::Url { url } => assert_eq!(url, "https://example.com"),
        other => panic!("expected Url, got {other:?}"),
    }
}

#[test]
fn oversized_text_is_rejected() {
    let big = "a".repeat(10_001);
    let formats = DropFormats {
        unicode_text: true,
        ..Default::default()
    };
    let err = classify_drop(&formats, Some(&big), &[]).unwrap_err();
    assert!(
        format!("{err}").contains("10000"),
        "rejection message must mention the 10000 character limit: {err}"
    );
}

#[test]
fn image_files_are_preferred_over_text() {
    // 资源管理器拖文件时总会附带文件名文本；它不能被当成备注。
    let files = vec![PathBuf::from("a.png")];
    let formats = DropFormats {
        hdrop: true,
        unicode_text: true,
        ..Default::default()
    };
    let content = classify_drop(&formats, Some("note"), &files).unwrap();
    assert!(matches!(content, DropContent::Files(_)), "{content:?}");
}

#[test]
fn files_with_rich_text_become_a_note() {
    // 浏览器的「图片 + 选中文字」拖拽同时提供 CF_HTML：文字作为图片备注。
    let files = vec![PathBuf::from("a.png")];
    let formats = DropFormats {
        hdrop: true,
        unicode_text: true,
        html: true,
        ..Default::default()
    };
    let content = classify_drop(&formats, Some("  图注  "), &files).unwrap();
    match content {
        DropContent::FilesWithNote { files, note } => {
            assert_eq!(files, vec![PathBuf::from("a.png")]);
            assert_eq!(note, "图注");
        }
        other => panic!("expected FilesWithNote, got {other:?}"),
    }
}

#[test]
fn plain_text_becomes_text_content() {
    let formats = DropFormats {
        unicode_text: true,
        ..Default::default()
    };
    let content = classify_drop(&formats, Some("hello\nworld"), &[]).unwrap();
    assert!(matches!(content, DropContent::Text { .. }), "{content:?}");
}

#[test]
fn whitespace_is_not_a_url() {
    let formats = DropFormats {
        unicode_text: true,
        ..Default::default()
    };
    let content = classify_drop(&formats, Some("see https://example.com"), &[]).unwrap();
    assert!(matches!(content, DropContent::Text { .. }), "{content:?}");
}

#[test]
fn non_http_scheme_is_not_a_url() {
    let formats = DropFormats {
        unicode_text: true,
        ..Default::default()
    };
    let content = classify_drop(&formats, Some("file:///C:/temp/a.txt"), &[]).unwrap();
    assert!(matches!(content, DropContent::Text { .. }), "{content:?}");
}

#[test]
fn empty_text_is_rejected() {
    let formats = DropFormats {
        unicode_text: true,
        ..Default::default()
    };
    let err = classify_drop(&formats, Some("   \n\t "), &[]).unwrap_err();
    assert!(format!("{err}").contains("empty"), "{err}");
}
