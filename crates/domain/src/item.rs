//! 素材（item）类型。枚举值序列化为 snake_case，与 SQL CHECK 值一致。

use serde::{Deserialize, Serialize};

/// 素材类型，对应 `items.kind` 的 `image`/`text`/`link`。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    Image,
    Text,
    Link,
}

/// 素材处理状态，对应 `items.status` 的 `parsing`/`ready`/`pending`/`failed`。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemStatus {
    Parsing,
    Ready,
    Pending,
    Failed,
}

/// 待入库的素材。`original_path` 相对库根目录。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewItem {
    pub kind: ItemKind,
    pub captured_at: i64,
    pub source_app: Option<String>,
    pub source_title: Option<String>,
    pub source_url: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub char_count: Option<u32>,
    pub original_path: String,
    pub content_hash: String,
}

/// 已入库的素材。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    pub id: String,
    pub kind: ItemKind,
    pub status: ItemStatus,
    pub captured_at: i64,
    pub source_app: Option<String>,
    pub source_title: Option<String>,
    pub source_url: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub char_count: Option<u32>,
    pub original_path: String,
    pub thumb_path: Option<String>,
    pub note: String,
    pub content_hash: String,
    /// 元数据版本号（阶段 03）：索引任务的幂等依据，任何影响检索文本的变更都会 +1。
    pub revision: i64,
    /// 主色（`insights.primary_color`，LEFT JOIN 而来；未解析或未给色时为 `None`）。
    pub primary_color: Option<String>,
}
