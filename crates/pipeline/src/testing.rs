//! 测试用假 Provider：记录调用次数、可注入结果与失败类型。
//!
//! 用途（design §9.2）：两阶段时序、退避、content_hash 复用、suppressions 等行为都需要
//! 「可控且可计数」的 Provider；真实模型只在 `#[ignore]` 用例里跑。

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

use async_trait::async_trait;
use visrecall_domain::insight::OcrBox;
use visrecall_domain::providers::{
    EmbeddingProvider, OcrOutput, OcrProvider, TextInsight, TextProvider, VisionInsight,
    VisionProvider,
};
use visrecall_domain::AppError;

/// 注入的失败类型（`AppError` 不实现 `Clone`，用这个规格反复构造）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FakeFail {
    /// 可重试的云端失败。
    Provider(String),
    /// 401/403 这类配置错误（终止重试）。
    Config(String),
    /// schema 不合法。
    Schema(String),
    /// 通用内部错误。
    Internal(String),
}

impl FakeFail {
    pub fn to_error(&self) -> AppError {
        match self {
            Self::Provider(msg) => AppError::Provider(msg.clone()),
            Self::Config(msg) => AppError::ProviderConfig(msg.clone()),
            Self::Schema(msg) => AppError::SchemaViolation(msg.clone()),
            Self::Internal(msg) => AppError::Internal(msg.clone()),
        }
    }
}

/// 假 OCR：返回预设结果，或注入错误。
#[derive(Debug)]
pub struct FakeOcr {
    pub calls: AtomicUsize,
    output: Mutex<OcrOutput>,
    fail: Mutex<Option<FakeFail>>,
}

impl FakeOcr {
    pub fn with_text(text: &str) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            output: Mutex::new(OcrOutput {
                text: Some(text.to_string()),
                boxes: vec![OcrBox::new(text, 0.1, 0.1, 0.3, 0.05)],
                confidence: Some(0.95),
            }),
            fail: Mutex::new(None),
        }
    }

    /// 无字图片：`text`/`boxes`/`confidence` 全空（正常结果，不是错误）。
    pub fn empty() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            output: Mutex::new(OcrOutput::empty()),
            fail: Mutex::new(None),
        }
    }

    pub fn failing(fail: FakeFail) -> Self {
        let fake = Self::with_text("");
        *fake.fail.lock().unwrap() = Some(fail);
        fake
    }

    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl OcrProvider for FakeOcr {
    fn id(&self) -> &'static str {
        "fake-ocr"
    }

    fn recognize(&self, _png: &[u8]) -> Result<OcrOutput, AppError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(fail) = self.fail.lock().unwrap().as_ref() {
            return Err(fail.to_error());
        }
        Ok(self.output.lock().unwrap().clone())
    }
}

/// 假嵌入：`hash(text) → 确定向量`（同文本同向量、异文本异向量）。
#[derive(Debug)]
pub struct FakeEmbed {
    pub calls: AtomicUsize,
    ready: AtomicBool,
    dim: usize,
    id: &'static str,
}

impl FakeEmbed {
    pub const ID: &'static str = "fake-embed";

    pub fn new() -> Self {
        Self::with_id(Self::ID, 8)
    }

    /// 指定模型 id 与维度（模拟「换模型 → 蓝绿重建」）。
    pub fn with_id(id: &'static str, dim: usize) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            ready: AtomicBool::new(true),
            dim,
            id,
        }
    }

    /// 模拟「模型未就绪」的降级场景。
    pub fn not_ready() -> Self {
        let fake = Self::new();
        fake.ready.store(false, Ordering::SeqCst);
        fake
    }

    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    /// 确定性向量：同一文本永远得到同一结果。
    pub fn vector_for(text: &str) -> Vec<f32> {
        let mut state: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in text.as_bytes() {
            state ^= u64::from(*byte);
            state = state.wrapping_mul(0x100_0000_01b3);
        }
        (0..8)
            .map(|i| {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1);
                let bits = (state >> 33) as u32;
                let value = (bits as f32 / u32::MAX as f32) * 2.0 - 1.0;
                value + (i as f32) * 0.0
            })
            .collect()
    }
}

impl Default for FakeEmbed {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl EmbeddingProvider for FakeEmbed {
    fn id(&self) -> &str {
        self.id
    }

    fn dim(&self) -> usize {
        self.dim
    }

    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, AppError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if !self.ready.load(Ordering::SeqCst) {
            return Err(AppError::ModelNotReady(FakeEmbed::ID.to_string()));
        }
        Ok(texts
            .iter()
            .map(|text| FakeEmbed::vector_for(text))
            .collect())
    }

    fn ready(&self) -> bool {
        self.ready.load(Ordering::SeqCst)
    }
}

/// 假视觉模型：返回预设属性，或注入错误。
#[derive(Debug)]
pub struct FakeVision {
    pub calls: AtomicUsize,
    insight: Mutex<VisionInsight>,
    fail: Mutex<Option<FakeFail>>,
}

impl FakeVision {
    pub fn new(insight: VisionInsight) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            insight: Mutex::new(insight),
            fail: Mutex::new(None),
        }
    }

    /// 一份「全字段」的典型返回。
    pub fn typical(summary: &str, tags: &[&str]) -> Self {
        Self::new(VisionInsight {
            summary: summary.to_string(),
            tags: tags.iter().map(|t| (*t).to_string()).collect(),
            content_type: Some("screenshot".into()),
            primary_color: Some("orange".into()),
            chart_type: Some("funnel".into()),
            ui_type: Some("list".into()),
            entities: vec!["VisRecall".into()],
            latex: None,
            has_formula: false,
            confidence: Some(0.88),
        })
    }

    pub fn failing(fail: FakeFail) -> Self {
        let fake = Self::typical("", &[]);
        *fake.fail.lock().unwrap() = Some(fail);
        fake
    }

    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl VisionProvider for FakeVision {
    fn id(&self) -> &str {
        "vlm-primary"
    }

    async fn analyze(&self, _png: &[u8]) -> Result<VisionInsight, AppError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(fail) = self.fail.lock().unwrap().as_ref() {
            return Err(fail.to_error());
        }
        Ok(self.insight.lock().unwrap().clone())
    }
}

/// 假文本模型。
#[derive(Debug)]
pub struct FakeText {
    pub calls: AtomicUsize,
    summary: Mutex<TextInsight>,
    fail: Mutex<Option<FakeFail>>,
}

impl FakeText {
    pub fn new(summary: &str, tags: &[&str]) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            summary: Mutex::new(TextInsight {
                summary: summary.to_string(),
                tags: tags.iter().map(|t| (*t).to_string()).collect(),
                confidence: Some(0.9),
            }),
            fail: Mutex::new(None),
        }
    }

    pub fn failing(fail: FakeFail) -> Self {
        let fake = Self::new("", &[]);
        *fake.fail.lock().unwrap() = Some(fail);
        fake
    }

    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl TextProvider for FakeText {
    fn id(&self) -> &str {
        "vlm-primary"
    }

    async fn summarize(&self, _text: &str) -> Result<TextInsight, AppError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(fail) = self.fail.lock().unwrap().as_ref() {
            return Err(fail.to_error());
        }
        Ok(self.summary.lock().unwrap().clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_embed_is_deterministic_and_text_specific() {
        assert_eq!(FakeEmbed::vector_for("发票"), FakeEmbed::vector_for("发票"));
        assert_ne!(FakeEmbed::vector_for("发票"), FakeEmbed::vector_for("报错"));
        assert_eq!(FakeEmbed::vector_for("x").len(), 8);
    }

    #[test]
    fn fail_specs_map_to_expected_variants() {
        assert!(FakeFail::Config("401".into()).to_error().is_config_error());
        assert!(FakeFail::Provider("500".into()).to_error().is_retryable());
        assert!(!FakeFail::Schema("bad".into()).to_error().is_retryable());
    }
}
