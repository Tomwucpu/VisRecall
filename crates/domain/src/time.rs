//! Unix 毫秒时间戳。`captured_at`/`created_at`/`updated_at` 统一使用毫秒。

/// 当前 Unix 时间（毫秒）。
pub fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_millis() as i64
}
