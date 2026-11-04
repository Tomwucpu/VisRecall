//! 标识符生成。

/// 生成新的素材 id（UUID v4 的连字符字符串形式）。
pub fn new_item_id() -> String {
    uuid::Uuid::new_v4().to_string()
}
