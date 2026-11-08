//! OCR 引擎与责任链：PP-OCR（ONNX，主路径）→ Windows.Media.Ocr（兜底）。
//!
//! 链的构造不做运行时探测：模型目录可解析且校验通过就用 PP-OCR，否则只留 Windows 兜底，
//! 并记一条 `warn`（design §8 的降级矩阵）。

pub mod ppocr;
pub mod windows_ocr;

pub use ppocr::{PpocrOcr, PPOCR_ID};
pub use windows_ocr::{WindowsOcr, WINDOWS_OCR_ID};

use std::path::Path;

use visrecall_domain::providers::{OcrOutput, OcrProvider};
use visrecall_domain::AppError;

/// PP-OCR → Windows.Media.Ocr 责任链。
pub struct OcrChain {
    ppocr: Option<PpocrOcr>,
    windows: WindowsOcr,
}

impl std::fmt::Debug for OcrChain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OcrChain")
            .field("ppocr", &self.ppocr.is_some())
            .finish()
    }
}

impl OcrChain {
    /// 从显式模型目录构造（`None` 或加载失败 → 只用 Windows 兜底）。
    pub fn with_model_dir(model_dir: Option<&Path>) -> Self {
        let ppocr = match model_dir {
            Some(dir) => match PpocrOcr::load(dir) {
                Ok(engine) => Some(engine),
                Err(err) => {
                    tracing::warn!(dir = %dir.display(), %err, "ppocr unavailable; falling back to Windows.Media.Ocr");
                    None
                }
            },
            None => {
                tracing::warn!("model dir not found; falling back to Windows.Media.Ocr");
                None
            }
        };
        Self {
            ppocr,
            windows: WindowsOcr::new(),
        }
    }

    /// 按运行时约定解析模型目录后构造（启动路径用这个）。
    pub fn bootstrap() -> Self {
        let dir = match crate::models::resolve_model_dir() {
            Ok(dir) => {
                match crate::models::verify_manifest(&dir) {
                    Ok(_) => {}
                    Err(err) => {
                        tracing::warn!(dir = %dir.display(), %err, "model manifest verification failed");
                    }
                }
                Some(dir)
            }
            Err(err) => {
                tracing::warn!(%err, "model dir unresolved");
                None
            }
        };
        Self::with_model_dir(dir.as_deref())
    }

    /// PP-OCR 是否可用（详情面板/日志用）。
    pub fn ppocr_available(&self) -> bool {
        self.ppocr.is_some()
    }
}

impl Default for OcrChain {
    fn default() -> Self {
        Self::bootstrap()
    }
}

impl OcrProvider for OcrChain {
    fn id(&self) -> &'static str {
        if self.ppocr.is_some() {
            PPOCR_ID
        } else {
            WINDOWS_OCR_ID
        }
    }

    fn recognize(&self, png: &[u8]) -> Result<OcrOutput, AppError> {
        self.recognize_with_engine(png).map(|(output, _)| output)
    }

    fn recognize_with_engine(&self, png: &[u8]) -> Result<(OcrOutput, &'static str), AppError> {
        if let Some(engine) = &self.ppocr {
            match engine.recognize(png) {
                Ok(output) => return Ok((output, PPOCR_ID)),
                Err(err) => {
                    tracing::warn!(%err, "ppocr inference failed; falling back to Windows.Media.Ocr");
                }
            }
        }
        self.windows
            .recognize(png)
            .map(|output| (output, WINDOWS_OCR_ID))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chain_without_models_still_constructs() {
        let chain = OcrChain::with_model_dir(None);
        assert!(!chain.ppocr_available());
        assert_eq!(chain.id(), WINDOWS_OCR_ID);
    }

    #[test]
    fn chain_with_bad_model_dir_degrades_instead_of_failing() {
        let chain = OcrChain::with_model_dir(Some(Path::new("Z:/definitely/missing")));
        assert!(!chain.ppocr_available());
        assert_eq!(chain.id(), WINDOWS_OCR_ID);
    }

    #[test]
    fn chain_with_real_models_prefers_ppocr() {
        let Ok(dir) = crate::models::resolve_model_dir() else {
            return; // 无模型环境跳过
        };
        let chain = OcrChain::with_model_dir(Some(&dir));
        assert!(chain.ppocr_available());
        assert_eq!(chain.id(), PPOCR_ID);
    }

    #[test]
    fn empty_image_yields_empty_output_from_windows_engine() {
        let chain = OcrChain::with_model_dir(Some(Path::new("Z:/definitely/missing")));
        let out = chain.recognize(&[]).expect("空输入不应报错");
        assert!(!out.has_text());
        assert!(out.boxes.is_empty());
        assert!(out.confidence.is_none());
    }
}
