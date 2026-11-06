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
    }
}
