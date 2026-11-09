//! VisRecall AI 层：本地推理（OCR / 嵌入）与云端 Provider（VLM / 文本），不碰数据库。
//!
//! 边界（design §3）：`ai` 只做推理与 HTTP，不依赖 `library`；检索侧通过
//! `domain::providers` 的 trait 注入，因此查询路径不会把 `ai` 拖进依赖图。

pub mod embedder;
pub mod keyring;
pub mod models;
pub mod ocr;
pub mod runtime;
pub mod vlm;

pub use embedder::BgeEmbedder;
pub use keyring::{delete_key, load_key, save_key, KeySlot};
pub use models::{
    load_manifest, locate_model_dir, resolve_model_dir, sha256_file, sha256_hex, verify_manifest,
    ModelManifest, MANIFEST_FILE, MODEL_DIR_ENV,
};
pub use ocr::{OcrChain, PpocrOcr, WindowsOcr};
pub use runtime::{environment, resolve_ort_dylib, ORT_DYLIB};
pub use vlm::{probe, probe_with, OpenAiCompatible, ProbeOutcome, Protocol};
