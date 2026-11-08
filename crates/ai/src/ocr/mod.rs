//! OCR 引擎：PP-OCR（ONNX，主路径）与 Windows.Media.Ocr（兜底）。

pub mod ppocr;

pub use ppocr::{PpocrOcr, PPOCR_ID};
