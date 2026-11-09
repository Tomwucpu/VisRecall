//! VisRecall 索引管线：jobs 驱动的两阶段索引（OCR → 索引 → VLM 增强）与生命周期。
//!
//! 依赖方向（design §3）：`pipeline → ai / search / library / domain`，**反方向不存在**。
//! 事件通过 [`PipelineEvents`] 抽象上报，由 `app-core` 适配成前端事件——这样 pipeline
//! 不需要（也不允许）依赖 `app-core`。

pub mod testing;
pub mod worker;

pub use worker::{
    enqueue, JobCtx, JobHandler, NoopEvents, Pipeline, PipelineConfig, PipelineDeps,
    PipelineEvents, PipelineHandle, PRIORITY_NEW, PRIORITY_REBUILD, PRIORITY_RETRY,
};

/// 阶段一前半：图片 OCR。
pub const STAGE_OCR: &str = "ocr";
/// 阶段一收口 + 重建共用：search document → 嵌入 → 同事务写三处。
pub const STAGE_INDEX: &str = "index";
/// 阶段二：VLM/文本 Provider 增强。
pub const STAGE_VLM: &str = "vlm";
/// 蓝绿重建 / 启动修复。
pub const STAGE_REBUILD: &str = "rebuild";

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
