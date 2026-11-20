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

/// 素材原件的存储归属。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageMode {
    /// 原件由 VisRecall 保存在素材库内。
    #[default]
    Managed,
    /// 原件由用户保管，素材库只保存只读路径引用。
    Reference,
}

impl StorageMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Managed => "managed",
            Self::Reference => "reference",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "managed" => Some(Self::Managed),
            "reference" => Some(Self::Reference),
            _ => None,
        }
    }
}

impl ItemKind {
    pub const ALL: [ItemKind; 3] = [Self::Image, Self::Text, Self::Link];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Text => "text",
            Self::Link => "link",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        let lower = raw.trim().to_ascii_lowercase();
        Self::ALL.into_iter().find(|kind| kind.as_str() == lower)
    }

    /// 中文展示名（chips 用）。
    pub fn label_zh(self) -> &'static str {
        match self {
            Self::Image => "图片",
            Self::Text => "文本",
            Self::Link => "链接",
        }
    }
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

impl ItemStatus {
    pub const ALL: [ItemStatus; 4] = [Self::Parsing, Self::Ready, Self::Pending, Self::Failed];

    /// 与 `items.status` 的 SQL 值一致。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Parsing => "parsing",
            Self::Ready => "ready",
            Self::Pending => "pending",
            Self::Failed => "failed",
        }
    }

    /// 解析 SQL 值；未知值返回 `None`（IPC 层据此忽略非法输入，而不是整体报错）。
    pub fn parse(raw: &str) -> Option<Self> {
        let lower = raw.trim().to_ascii_lowercase();
        Self::ALL
            .into_iter()
            .find(|status| status.as_str() == lower)
    }

    /// 中文展示名（筛选标签与 chips 用）；与前端 `statusLabel` 保持一致。
    pub fn label_zh(self) -> &'static str {
        match self {
            Self::Parsing => "解析中",
            Self::Ready => "正常",
            Self::Pending => "待解析",
            Self::Failed => "失败",
        }
    }
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
    /// 用户可编辑标题；`None` 表示未改名，展示层回落到 `source_title`。
    ///
    /// 不放进 [`NewItem`]：捕获与导入永远以「未改名」入库，避免所有调用点都要多传一个字段。
    pub title: Option<String>,
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

impl Item {
    /// 展示与检索使用的标题：用户改名优先，否则回落到来源标题。
    ///
    /// 索引写入侧与检索解释侧都走这里，避免两处各写一遍「title or source_title」而漂移。
    pub fn display_title(&self) -> Option<&str> {
        self.title.as_deref().or(self.source_title.as_deref())
    }
}
