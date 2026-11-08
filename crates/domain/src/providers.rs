//! Provider trait 定义（design D3-01）：全部放在 domain，让 `search` 只见 trait 不见 `ai`。
//!
//! 实现分布：`crates/ai` 提供嵌入 / OCR / VLM / 文本的真实实现；`crates/library` 提供
//! `VectorIndex` 的暴力 int8 实现。`pipeline` 与 `app-core` 只依赖这里。

use async_trait::async_trait;

use crate::error::AppError;
use crate::insight::OcrBox;

/// OCR 识别结果。**空文本是正常结果**（无字图片），不是错误。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OcrOutput {
    /// 按阅读序拼接的全文（行间 `\n`）；无文字时为 `None`。
    pub text: Option<String>,
    /// 归一化包围盒（`[0,1]`），无文字时为空。
    pub boxes: Vec<OcrBox>,
    /// 逐框平均置信度；无文字时为 `None`（**不是 0**：0 会误触「置信度低」标注）。
    pub confidence: Option<f32>,
}

impl OcrOutput {
    /// 无文字结果（构造空值，避免调用方写 `Some("")`）。
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn has_text(&self) -> bool {
        self.text.as_deref().is_some_and(|t| !t.trim().is_empty())
    }

    /// 逐框文本（行级），供高亮映射与解释使用。
    pub fn box_texts(&self) -> impl Iterator<Item = &str> {
        self.boxes.iter().map(|b| b.text.as_str())
    }
}

/// OCR 引擎（PP-OCR 主路径 → Windows.Media.Ocr 兜底）。
pub trait OcrProvider: Send + Sync {
    /// 稳定引擎标识，写入 `insights.ocr_engine`：`"ppocr-mobile"` / `"windows-ocr"`。
    fn id(&self) -> &'static str;
    /// 识别 PNG 字节；引擎失败返回 `Err`（调用方按「OCR 无文字」矩阵处理）。
    fn recognize(&self, png: &[u8]) -> Result<OcrOutput, AppError>;

    /// 识别并告知**实际**使用的引擎（责任链降级时与 `id()` 不同）。
    ///
    /// 默认实现把 `id()` 当作实际引擎；`OcrChain` 覆盖它以返回真实降级结果，
    /// 这样 `insights.ocr_engine` 记录的是实际跑的那个引擎（P3-R01）。
    fn recognize_with_engine(&self, png: &[u8]) -> Result<(OcrOutput, &'static str), AppError> {
        self.recognize(png).map(|output| (output, self.id()))
    }
}

/// 本地嵌入（查询与索引用同一个 Provider，保证同一向量空间）。
#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    /// 写入向量表 `model_id` 与 `settings['embedding.model_id']`。
    fn id(&self) -> &str;
    /// 维度（bge-small-zh-v1.5 为 512）。
    fn dim(&self) -> usize;
    /// 批量嵌入；同一文本必须给出同一向量（确定性）。
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, AppError>;
    /// 模型是否已加载（懒加载完成）。检索据此决定是否跳过语义通道。
    fn ready(&self) -> bool;
}

/// 图片理解结果（云端 VLM）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VisionInsight {
    pub summary: String,
    pub tags: Vec<String>,
    /// `ContentType` 字面量；模型未给出时为 `None`。
    pub content_type: Option<String>,
    /// `PrimaryColor` 字面量。
    pub primary_color: Option<String>,
    /// `ChartType` 字面量。
    pub chart_type: Option<String>,
    /// `UiType` 字面量。
    pub ui_type: Option<String>,
    pub entities: Vec<String>,
    pub latex: Option<String>,
    pub has_formula: bool,
    pub confidence: Option<f32>,
}

/// 文本理解结果（云端 TextProvider）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TextInsight {
    pub summary: String,
    pub tags: Vec<String>,
    pub confidence: Option<f32>,
}

/// 视觉模型（`image_url` 输入）。
#[async_trait]
pub trait VisionProvider: Send + Sync {
    /// 稳定标识，写入 `insights.model_provider`（如 `"vlm-primary"`）。
    fn id(&self) -> &str;
    async fn analyze(&self, png: &[u8]) -> Result<VisionInsight, AppError>;
}

/// 文本模型（摘要 + 标签）。
#[async_trait]
pub trait TextProvider: Send + Sync {
    fn id(&self) -> &str;
    async fn summarize(&self, text: &str) -> Result<TextInsight, AppError>;
}

/// 向量索引抽象：暴力 int8（V0.1）→ vec0 → HNSW。检索侧只见此 trait。
pub trait VectorIndex: Send + Sync {
    /// 写入/覆盖一条向量（`vec8` 为 `quantize_i8` 的结果）。
    fn add(&self, rowid: i64, vec8: &[u8]) -> Result<(), AppError>;
    fn remove(&self, rowid: i64) -> Result<(), AppError>;
    /// 返回 `(rowid, cosine)` 按相似度降序、最多 `top_k` 条；
    /// `candidates` 为 `Some` 时只在这些 rowid 内检索。
    fn search(
        &self,
        query: &[f32],
        top_k: usize,
        candidates: Option<&[i64]>,
    ) -> Result<Vec<(i64, f32)>, AppError>;
}
