//! Windows.Media.Ocr 兜底引擎（P3-R01）。
//!
//! 只在 PP-OCR 不可用时跑：模型文件缺失/校验失败/初始化失败。语言包缺失（`zh-CN` 与
//! 用户配置语言都建不出引擎）时返回 `Err`，上游按「OCR 无文字」矩阵处理（design §8）。
//!
//! 实现要点：
//! - WinRT 异步在这里同步 block（专用管线线程调用，不在 UI 线程）；
//! - 首次调用初始化 MTA（`RoInitialize(RO_INIT_MULTITHREADED)`），重复初始化返回
//!   `RPC_E_CHANGED_MODE` 视为已就绪；
//! - Windows OCR 不暴露置信度 → `confidence = None`（不会误触「置信度低」标注）。

use std::sync::OnceLock;

use visrecall_domain::insight::OcrBox;
use visrecall_domain::providers::OcrOutput;
use visrecall_domain::AppError;
use windows::core::HSTRING;
use windows::Globalization::Language;
use windows::Graphics::Imaging::{
    BitmapAlphaMode, BitmapDecoder, BitmapPixelFormat, SoftwareBitmap,
};
use windows::Media::Ocr::OcrEngine;
use windows::Storage::Streams::{DataWriter, InMemoryRandomAccessStream};
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};

/// 写入 `insights.ocr_engine` 的兜底引擎标识。
pub const WINDOWS_OCR_ID: &str = "windows-ocr";

/// 优先尝试的语言（中文截图场景）。
const PREFERRED_LANGUAGE: &str = "zh-CN";

static APARTMENT: OnceLock<()> = OnceLock::new();

/// Windows.Media.Ocr 引擎（无状态，可复用）。
#[derive(Debug, Default, Clone, Copy)]
pub struct WindowsOcr;

impl WindowsOcr {
    pub fn new() -> Self {
        Self
    }

    /// 识别 PNG 字节。
    pub fn recognize(&self, png: &[u8]) -> Result<OcrOutput, AppError> {
        if png.is_empty() {
            return Ok(OcrOutput::empty());
        }
        let bitmap = decode_bitmap(png)?;
        let engine = create_engine()?;
        let result = engine
            .RecognizeAsync(&bitmap)
            .and_then(|op| op.join())
            .map_err(|e| AppError::Internal(format!("windows-ocr: recognize: {e}")))?;

        let width = bitmap.PixelWidth().unwrap_or(0) as f32;
        let height = bitmap.PixelHeight().unwrap_or(0) as f32;
        let lines = result
            .Lines()
            .map_err(|e| AppError::Internal(format!("windows-ocr: lines: {e}")))?;

        let mut texts: Vec<String> = Vec::new();
        let mut boxes: Vec<OcrBox> = Vec::new();
        let count = lines
            .Size()
            .map_err(|e| AppError::Internal(format!("windows-ocr: line count: {e}")))?;
        for i in 0..count {
            let line = lines
                .GetAt(i)
                .map_err(|e| AppError::Internal(format!("windows-ocr: line {i}: {e}")))?;
            let text = line
                .Text()
                .map_err(|e| AppError::Internal(format!("windows-ocr: line text: {e}")))?
                .to_string();
            if !text.trim().is_empty() {
                texts.push(text);
            }
            let words = line
                .Words()
                .map_err(|e| AppError::Internal(format!("windows-ocr: words: {e}")))?;
            let word_count = words
                .Size()
                .map_err(|e| AppError::Internal(format!("windows-ocr: word count: {e}")))?;
            for w in 0..word_count {
                let word = words
                    .GetAt(w)
                    .map_err(|e| AppError::Internal(format!("windows-ocr: word {w}: {e}")))?;
                let rect = word
                    .BoundingRect()
                    .map_err(|e| AppError::Internal(format!("windows-ocr: bounding rect: {e}")))?;
                let word_text = word.Text().unwrap_or_default().to_string();
                if let Some(obox) = OcrBox::normalized(
                    word_text,
                    rect.X,
                    rect.Y,
                    rect.Width,
                    rect.Height,
                    width,
                    height,
                ) {
                    boxes.push(obox);
                }
            }
        }

        if texts.is_empty() {
            return Ok(OcrOutput::empty());
        }
        Ok(OcrOutput {
            text: Some(texts.join("\n")),
            boxes,
            // Windows OCR 不给置信度：`None` 表示「未知」，而不是「0 = 很差」。
            confidence: None,
        })
    }
}

fn ensure_apartment() -> Result<(), AppError> {
    if APARTMENT.get().is_some() {
        return Ok(());
    }
    unsafe {
        // 已初始化为其他套间模式时返回 RPC_E_CHANGED_MODE：WinRT 仍可用，忽略。
        let _ = RoInitialize(RO_INIT_MULTITHREADED);
    }
    let _ = APARTMENT.set(());
    Ok(())
}

fn decode_bitmap(png: &[u8]) -> Result<SoftwareBitmap, AppError> {
    ensure_apartment()?;
    let stream = InMemoryRandomAccessStream::new()
        .map_err(|e| AppError::Internal(format!("windows-ocr: stream: {e}")))?;
    let writer = DataWriter::CreateDataWriter(&stream)
        .map_err(|e| AppError::Internal(format!("windows-ocr: writer: {e}")))?;
    writer
        .WriteBytes(png)
        .map_err(|e| AppError::Internal(format!("windows-ocr: write bytes: {e}")))?;
    writer
        .StoreAsync()
        .and_then(|op| op.join())
        .map_err(|e| AppError::Internal(format!("windows-ocr: store: {e}")))?;
    writer
        .FlushAsync()
        .and_then(|op| op.join())
        .map_err(|e| AppError::Internal(format!("windows-ocr: flush: {e}")))?;
    writer
        .DetachStream()
        .map_err(|e| AppError::Internal(format!("windows-ocr: detach: {e}")))?;
    stream
        .Seek(0)
        .map_err(|e| AppError::Internal(format!("windows-ocr: seek: {e}")))?;

    let decoder = BitmapDecoder::CreateAsync(&stream)
        .and_then(|op| op.join())
        .map_err(|e| AppError::Internal(format!("windows-ocr: decode: {e}")))?;
    let bitmap = decoder
        .GetSoftwareBitmapAsync()
        .and_then(|op| op.join())
        .map_err(|e| AppError::Internal(format!("windows-ocr: bitmap: {e}")))?;
    // OcrEngine 只接受 Bgra8 / Gray8
    SoftwareBitmap::ConvertWithAlpha(
        &bitmap,
        BitmapPixelFormat::Bgra8,
        BitmapAlphaMode::Premultiplied,
    )
    .map_err(|e| AppError::Internal(format!("windows-ocr: convert: {e}")))
}

fn create_engine() -> Result<OcrEngine, AppError> {
    ensure_apartment()?;
    if let Ok(language) = Language::CreateLanguage(&HSTRING::from(PREFERRED_LANGUAGE)) {
        if let Ok(engine) = OcrEngine::TryCreateFromLanguage(&language) {
            tracing::debug!(language = PREFERRED_LANGUAGE, "windows-ocr engine ready");
            return Ok(engine);
        }
    }
    OcrEngine::TryCreateFromUserProfileLanguages().map_err(|e| {
        AppError::Internal(format!(
            "windows-ocr: no OCR language available ({e}); install a language pack"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_is_an_empty_result() {
        let ocr = WindowsOcr::new();
        let out = ocr.recognize(&[]).expect("空输入不应报错");
        assert!(!out.has_text());
        assert!(out.confidence.is_none());
    }

    #[test]
    fn invalid_png_returns_error_not_panic() {
        let ocr = WindowsOcr::new();
        let err = ocr.recognize(b"not a png").expect_err("非法图片必须报错");
        assert!(matches!(err, AppError::Internal(_)), "err={err:?}");
    }
}
