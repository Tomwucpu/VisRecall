//! 统一错误类型，可直接序列化给前端做错误码映射。

use serde::Serialize;

#[derive(Debug, thiserror::Error, Serialize)]
pub enum AppError {
    #[error("config missing")]
    ConfigMissing,
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("not found")]
    NotFound,
    #[error("storage full")]
    StorageFull,
    #[error("capture failed: {0}")]
    CaptureFailed(String),
    #[error("capture cancelled")]
    CaptureCancelled,
    #[error("drm protected")]
    DrmProtected,
    #[error("file missing: {0}")]
    FileMissing(String),
    #[error("clipboard busy")]
    ClipboardBusy,
    #[error("clipboard failed: {0}")]
    ClipboardFailed(String),
    #[error("busy: {0}")]
    Busy(String),
    #[error("io: {0}")]
    Io(String),
    #[error("database: {0}")]
    Database(String),
    #[error("internal: {0}")]
    Internal(String),

    // --- 阶段 03：模型与云端 Provider ---
    /// 本地模型（嵌入/OCR）未就绪或加载失败：检索降级、索引任务退避重试。
    #[error("model not ready: {0}")]
    ModelNotReady(String),
    /// 可重试的云端失败（连接/超时/5xx/429）：同模型退避重试，不做主备降级。
    #[error("provider unavailable: {0}")]
    Provider(String),
    /// 配置错误（401/403、地址非法）：**终止重试**，要求用户改配置。
    #[error("provider config error: {0}")]
    ProviderConfig(String),
    /// 模型返回的 JSON 不符合 schema 且修复一次后仍失败：按本次调用失败处理。
    #[error("schema violation: {0}")]
    SchemaViolation(String),
}

impl AppError {
    /// 是否值得重试（指数退避）。配置错误与模型未就绪都不该无限重试。
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            AppError::Provider(_) | AppError::Io(_) | AppError::Database(_) | AppError::Busy(_)
        )
    }

    /// 是否需要用户改配置（UI 明示「密钥或地址配置错误」）。
    pub fn is_config_error(&self) -> bool {
        matches!(self, AppError::ProviderConfig(_) | AppError::ConfigMissing)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_messages_are_stable() {
        assert_eq!(AppError::ConfigMissing.to_string(), "config missing");
        assert_eq!(AppError::NotFound.to_string(), "not found");
        assert_eq!(
            AppError::CaptureFailed("no display".into()).to_string(),
            "capture failed: no display"
        );
        assert_eq!(
            AppError::FileMissing("originals/a.png".into()).to_string(),
            "file missing: originals/a.png"
        );
        assert_eq!(AppError::ClipboardBusy.to_string(), "clipboard busy");
        assert_eq!(
            AppError::ClipboardFailed("GlobalAlloc".into()).to_string(),
            "clipboard failed: GlobalAlloc"
        );
        assert_eq!(
            AppError::Busy("migrating".into()).to_string(),
            "busy: migrating"
        );
        assert_eq!(
            AppError::ProviderConfig("401".into()).to_string(),
            "provider config error: 401"
        );
        assert_eq!(
            AppError::SchemaViolation("missing summary".into()).to_string(),
            "schema violation: missing summary"
        );
        assert_eq!(
            AppError::ModelNotReady("bge".into()).to_string(),
            "model not ready: bge"
        );
    }

    #[test]
    fn retry_and_config_classification_is_stable() {
        assert!(AppError::Provider("timeout".into()).is_retryable());
        assert!(AppError::Io("disk".into()).is_retryable());
        assert!(!AppError::ProviderConfig("401".into()).is_retryable());
        assert!(!AppError::ModelNotReady("bge".into()).is_retryable());
        assert!(!AppError::SchemaViolation("bad".into()).is_retryable());

        assert!(AppError::ProviderConfig("403".into()).is_config_error());
        assert!(AppError::ConfigMissing.is_config_error());
        assert!(!AppError::Provider("500".into()).is_config_error());
    }
}
