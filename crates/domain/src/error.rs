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
    #[error("io: {0}")]
    Io(String),
    #[error("database: {0}")]
    Database(String),
    #[error("internal: {0}")]
    Internal(String),
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
    }
}
