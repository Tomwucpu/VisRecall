//! 云端 AI 授权的库内策略（本项目里「策略属于这个库」的既有约定，见 `storage.rs` 头部注释）。
//!
//! 目前只有一项：**文本/片段是否上传云端解析**。
//! - 默认 `false`：用户主动开启前，文本不得发起任何云端请求（设计文档「云端 AI 默认关闭」）；
//! - 图片不受它约束（沿用既有模型设置），因此这里不做全局限流，只做文本侧的闸门。
//!
//! 读取宽容：坏值/读失败一律回落 `false`，与 [`crate::settings_kv`] 的语义一致。

use visrecall_domain::AppError;

use crate::db::Library;

/// `settings` 表的键名。
pub const KEY_CLOUD_TEXT_AI: &str = "ai.cloud_text_ai";

impl Library {
    /// 文本/片段是否允许上传云端解析；缺失、损坏或越界一律按关闭处理。
    pub fn cloud_text_ai_enabled(&self) -> bool {
        self.setting_get::<bool>(KEY_CLOUD_TEXT_AI).unwrap_or(false)
    }

    /// 保存文本侧云端解析授权。
    pub fn set_cloud_text_ai(&self, enabled: bool) -> Result<(), AppError> {
        self.setting_set(KEY_CLOUD_TEXT_AI, &enabled)
    }
}
