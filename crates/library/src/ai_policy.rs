//! 云端 AI 授权的库内策略（本项目里「策略属于这个库」的既有约定，见 `storage.rs` 头部注释）。
//!
//! 两项开关，默认均关闭（设计文档「默认不上传」）：
//! - `ai.cloud_image_ai`：图片是否允许把原图交给云端 VLM；
//! - `ai.cloud_text_ai`：文本/片段是否允许把原文交给云端模型。
//!
//! [`Library::cloud_parse_allowed`] 是所有入队、执行与重新解析路径的唯一判定入口：
//! 链接不参与自动云端解析；引用素材还要求来源目录当前可用。
//!
//! 读取宽容：坏值/读失败一律回落 `false`，与 [`crate::settings_kv`] 的语义一致。

use visrecall_domain::item::{Item, ItemKind};
use visrecall_domain::AppError;

use crate::db::Library;

/// `settings` 表的键名。
pub const KEY_CLOUD_IMAGE_AI: &str = "ai.cloud_image_ai";
pub const KEY_CLOUD_TEXT_AI: &str = "ai.cloud_text_ai";

impl Library {
    /// 图片是否允许上传云端解析；缺失、损坏或越界一律按关闭处理。
    pub fn cloud_image_ai_enabled(&self) -> bool {
        self.setting_get::<bool>(KEY_CLOUD_IMAGE_AI)
            .unwrap_or(false)
    }

    /// 保存图片侧云端解析授权。
    pub fn set_cloud_image_ai(&self, enabled: bool) -> Result<(), AppError> {
        self.setting_set(KEY_CLOUD_IMAGE_AI, &enabled)
    }

    /// 文本/片段是否允许上传云端解析；缺失、损坏或越界一律按关闭处理。
    pub fn cloud_text_ai_enabled(&self) -> bool {
        self.setting_get::<bool>(KEY_CLOUD_TEXT_AI).unwrap_or(false)
    }

    /// 保存文本侧云端解析授权。
    pub fn set_cloud_text_ai(&self, enabled: bool) -> Result<(), AppError> {
        self.setting_set(KEY_CLOUD_TEXT_AI, &enabled)
    }

    /// 该素材当前是否允许进入云端阶段。
    ///
    /// 开关只表达「允许上传」的意愿；模型未配置时由调用方跳过云端阶段，
    /// 不把素材标成待重试。
    pub fn cloud_parse_allowed(&self, item: &Item) -> Result<bool, AppError> {
        if !self.cloud_source_available(&item.id)? {
            return Ok(false);
        }
        Ok(match item.kind {
            ItemKind::Image => self.cloud_image_ai_enabled(),
            ItemKind::Text => self.cloud_text_ai_enabled(),
            ItemKind::Link => false,
        })
    }
}
