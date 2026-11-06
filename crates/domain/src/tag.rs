//! 标签领域类型、名称规范化与色令牌散列（design §4.4、§4.5）。
//!
//! 标签的语义规则放在这里而不是库层，方便库层、编排层与 IPC 层共用同一套判断：
//! - 名称先 `trim` 并折叠内部空白，再按**字符**（不是字节）限制长度；
//! - 同一名称大小写不敏感地复用同一条标签（`NOCASE` 索引在库层表达）；
//! - 颜色由名称散列到固定的 7 个色相令牌，后端定型，前端只做令牌 → CSS 变量映射。

use serde::{Deserialize, Serialize};

use crate::error::AppError;

/// `tokens.css` 已定义的 7 个色相令牌；`ash` 是中性档。
pub const TAG_HUES: [&str; 7] = ["ochre", "slate", "jade", "ash", "indigo", "plum", "paper"];

/// 标签名长度上限（按字符计）。
pub const MAX_TAG_CHARS: usize = 32;

/// 标签来源。
///
/// - `tags.source` 记录标签**首次创建**的来源；
/// - `item_tags.origin` 记录**这一次关联**的来源。
///
/// 两者必须分开：同一个标签可能被自动贴到 A、被用户手动贴到 B，
/// chip 上的「自动」标记要按关联判定，不能按标签判定。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TagSource {
    Auto,
    Manual,
}

impl TagSource {
    /// 数据库中的字面值（`'auto'` / `'manual'`）。
    pub fn as_str(self) -> &'static str {
        match self {
            TagSource::Auto => "auto",
            TagSource::Manual => "manual",
        }
    }

    /// 解析数据库中的字面值；未知值视为数据损坏。
    pub fn parse(raw: &str) -> Result<TagSource, AppError> {
        match raw {
            "auto" => Ok(TagSource::Auto),
            "manual" => Ok(TagSource::Manual),
            other => Err(AppError::Database(format!("unknown tag source: {other}"))),
        }
    }
}

/// 标签本体。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tag {
    pub id: String,
    pub name: String,
    /// 标签首次创建的来源。
    pub source: TagSource,
    pub created_at: i64,
}

/// 标签 + 当前素材的关联信息；IPC 的 `TagChip` 由此映射。
///
/// `hue` 是令牌名（见 [`TAG_HUES`]），由 [`hue_for`] 从名称派生，不入库——
/// 这样改名后颜色随之改变，且任何机器上结果一致。
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ItemTag {
    pub tag: Tag,
    /// 本次关联的来源。
    pub origin: TagSource,
    /// 标签云用的素材数量；按素材查询时为 0。
    pub item_count: u64,
    /// 色令牌名。
    pub hue: &'static str,
}

impl ItemTag {
    /// 用固定字段拼装；`hue` 始终由名称派生，避免调用方各写一遍散列。
    pub fn new(tag: Tag, origin: TagSource, item_count: u64) -> ItemTag {
        let hue = hue_for(&tag.name);
        ItemTag {
            tag,
            origin,
            item_count,
            hue,
        }
    }

    /// 本次关联是否为自动标注。
    pub fn is_auto(&self) -> bool {
        self.origin == TagSource::Auto
    }
}

/// 规范化标签名：`trim` + 折叠连续空白 + 长度/控制字符校验。
///
/// 返回的名字是**唯一**可用于入库的形式；`"  a   b  "` 与 `"a b"` 归一到同一值。
pub fn normalize_tag_name(raw: &str) -> Result<String, AppError> {
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return Err(AppError::InvalidInput("tag name is empty".into()));
    }
    if collapsed.chars().count() > MAX_TAG_CHARS {
        return Err(AppError::InvalidInput(format!(
            "tag name longer than {MAX_TAG_CHARS} chars"
        )));
    }
    if collapsed.chars().any(|c| c.is_control()) {
        return Err(AppError::InvalidInput("tag name has control chars".into()));
    }
    Ok(collapsed)
}

/// FNV-1a 32 位，对小写名称散列后映射到 [`TAG_HUES`]。
///
/// 同一名称在任何机器上颜色一致；大小写不同视为同一颜色（与标签复用规则一致）。
pub fn hue_for(name: &str) -> &'static str {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in name.to_lowercase().as_bytes() {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    TAG_HUES[(hash as usize) % TAG_HUES.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn normalize_collapses_inner_whitespace() {
        assert_eq!(normalize_tag_name("  a   b  ").unwrap(), "a b");
        assert_eq!(normalize_tag_name("\t参考\n").unwrap(), "参考");
    }

    #[test]
    fn normalize_rejects_empty_and_control_chars() {
        for raw in ["", "   ", "\t\n"] {
            assert!(matches!(
                normalize_tag_name(raw),
                Err(AppError::InvalidInput(_))
            ));
        }
        assert!(matches!(
            normalize_tag_name("a\u{7}b"),
            Err(AppError::InvalidInput(_))
        ));
    }

    #[test]
    fn normalize_limits_by_chars_not_bytes() {
        // 32 个汉字（96 字节）合法。
        let ok = "字".repeat(MAX_TAG_CHARS);
        assert_eq!(normalize_tag_name(&ok).unwrap().chars().count(), 32);
        // 33 个字符被拒。
        let too_long = "字".repeat(MAX_TAG_CHARS + 1);
        assert!(matches!(
            normalize_tag_name(&too_long),
            Err(AppError::InvalidInput(_))
        ));
    }

    #[test]
    fn hue_is_stable_and_case_insensitive() {
        assert_eq!(hue_for("work"), hue_for("work"));
        assert_eq!(hue_for("Work"), hue_for("work"));
        assert!(TAG_HUES.contains(&hue_for("any-other-name")));
    }

    #[test]
    fn hue_spreads_over_tokens() {
        let hues: HashSet<&'static str> = (0..1000).map(|i| hue_for(&format!("tag-{i}"))).collect();
        assert!(
            hues.len() >= 5,
            "expected at least 5 distinct hues, got {}",
            hues.len()
        );
    }

    #[test]
    fn item_tag_derives_hue_from_name() {
        let tag = Tag {
            id: "t1".into(),
            name: "参考".into(),
            source: TagSource::Manual,
            created_at: 0,
        };
        let item_tag = ItemTag::new(tag, TagSource::Auto, 3);
        assert_eq!(item_tag.hue, hue_for("参考"));
        assert!(item_tag.is_auto());
        assert_eq!(item_tag.item_count, 3);
    }

    #[test]
    fn tag_source_round_trips() {
        for source in [TagSource::Auto, TagSource::Manual] {
            assert_eq!(TagSource::parse(source.as_str()).unwrap(), source);
        }
        assert!(TagSource::parse("robot").is_err());
    }
}
