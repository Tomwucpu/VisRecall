//! OCR fixtures：带已知文本的截图 + 1 张无文字纯色图（仓库内置，`tests/fixtures/`）。
//!
//! - `#[ignore]` 用例需要模型文件（PP-OCR 主路径），本地 `cargo test -p visrecall-ai -- --ignored` 必跑；
//! - 非 ignore 用例只验证责任链降级（Windows.Media.Ocr 兜底），无语言包时自动跳过。

use std::path::PathBuf;

use visrecall_ai::ocr::{OcrChain, PpocrOcr, WindowsOcr, PPOCR_ID, WINDOWS_OCR_ID};
use visrecall_domain::providers::OcrProvider;

fn fixture(name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("读取 fixture {} 失败：{e}", path.display()))
}

fn ppocr() -> PpocrOcr {
    let dir = visrecall_ai::models::resolve_model_dir()
        .expect("模型目录缺失：先准备 models/（见 models/manifest.json）");
    PpocrOcr::load(&dir).expect("PP-OCR 模型加载失败")
}

fn assert_key_phrases(text: &str, phrases: &[&str]) {
    for phrase in phrases {
        assert!(
            text.contains(phrase),
            "OCR 文本缺少关键短语 {phrase:?}，实际文本：\n{text}"
        );
    }
}

#[test]
#[ignore = "needs real model files under models/"]
fn ppocr_reads_chinese_dialog_fixture() {
    let ocr = ppocr();
    let output = ocr.recognize(&fixture("zh_dialog.png")).unwrap();
    let text = output.text.clone().unwrap_or_default();
    assert_key_phrases(&text, &["导出文件失败", "请检查磁盘空间", "确定"]);
    assert!(!output.boxes.is_empty());
    assert!(output.confidence.unwrap() > 0.5);
}

#[test]
#[ignore = "needs real model files under models/"]
fn ppocr_reads_chinese_form_fixture() {
    let ocr = ppocr();
    let output = ocr.recognize(&fixture("zh_form.png")).unwrap();
    let text = output.text.clone().unwrap_or_default();
    assert_key_phrases(&text, &["用户名", "admin", "登录"]);
}

#[test]
#[ignore = "needs real model files under models/"]
fn ppocr_reads_english_error_fixture() {
    let ocr = ppocr();
    let output = ocr.recognize(&fixture("en_error.png")).unwrap();
    let text = output.text.clone().unwrap_or_default();
    assert_key_phrases(
        &text,
        &["502", "Bad Gateway", "database connection timeout"],
    );
}

#[test]
#[ignore = "needs real model files under models/"]
fn ppocr_reads_invoice_numbers_and_dates() {
    let ocr = ppocr();
    let output = ocr.recognize(&fixture("zh_invoice.png")).unwrap();
    let text = output.text.clone().unwrap_or_default();
    assert_key_phrases(&text, &["00329181", "2026-10-11", "1280.00"]);
}

#[test]
#[ignore = "needs real model files under models/"]
fn blank_image_is_an_empty_result_not_an_error() {
    let ocr = ppocr();
    let output = ocr
        .recognize(&fixture("blank.png"))
        .expect("无字图片必须返回 Ok");
    assert!(!output.has_text(), "text={:?}", output.text);
    assert!(output.boxes.is_empty(), "boxes={:?}", output.boxes);
    assert!(
        output.confidence.is_none(),
        "无文字时置信度必须是 None（0 会误触「置信度低」标注）"
    );
}

#[test]
#[ignore = "needs real model files under models/"]
fn boxes_are_normalized_and_confidence_in_range() {
    let ocr = ppocr();
    let output = ocr.recognize(&fixture("zh_invoice.png")).unwrap();
    for obox in &output.boxes {
        assert!((0.0..=1.0).contains(&obox.x), "{obox:?}");
        assert!((0.0..=1.0).contains(&obox.y), "{obox:?}");
        assert!(obox.w > 0.0 && obox.x + obox.w <= 1.0 + 1e-4, "{obox:?}");
        assert!(obox.h > 0.0 && obox.y + obox.h <= 1.0 + 1e-4, "{obox:?}");
    }
    let confidence = output.confidence.unwrap();
    assert!((0.0..=1.0).contains(&confidence), "{confidence}");
}

#[test]
#[ignore = "needs real model files under models/"]
fn chain_prefers_ppocr_when_models_exist() {
    let dir = visrecall_ai::models::resolve_model_dir().expect("模型目录缺失");
    let chain = OcrChain::with_model_dir(Some(&dir));
    assert!(chain.ppocr_available());
    let (output, engine) = chain
        .recognize_with_engine(&fixture("zh_dialog.png"))
        .unwrap();
    assert_eq!(engine, PPOCR_ID);
    assert!(output.has_text());
}

#[test]
fn chain_falls_back_to_windows_ocr_when_models_are_missing() {
    // 注入无效模型目录 → PP-OCR 不可用 → Windows.Media.Ocr 兜底
    let chain = OcrChain::with_model_dir(Some(std::path::Path::new("Z:/definitely/missing")));
    assert!(!chain.ppocr_available());

    match chain.recognize_with_engine(&fixture("zh_dialog.png")) {
        Ok((output, engine)) => {
            assert_eq!(engine, WINDOWS_OCR_ID);
            assert!(output.has_text(), "Windows OCR 应识别出文字");
            // Windows OCR 不给置信度，因此不会触发「置信度低」
            assert!(output.confidence.is_none());
        }
        Err(err) => {
            // 本机没有 OCR 语言包：如实跳过（design §8 的「OCR 无文字」路径）
            eprintln!("skip: Windows OCR 不可用（可能缺语言包）：{err}");
        }
    }
}

#[test]
fn windows_engine_reports_error_without_language_pack_or_decodes() {
    // 不依赖语言包：非法字节必须报错而不是 panic
    let engine = WindowsOcr::new();
    assert!(engine.recognize(b"definitely not a png").is_err());
    assert!(engine.recognize(&[]).unwrap().confidence.is_none());
}
