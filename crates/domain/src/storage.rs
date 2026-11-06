//! 存储相关的领域类型：用量、保留策略、孤儿统计与迁移报告（design §4.4）。
//!
//! 这里只放纯数据；字节统计、策略校验与文件操作由 `visrecall-library` 实现。

use serde::{Deserialize, Serialize};

/// 容量默认值（GiB）。
pub const DEFAULT_CAPACITY_GB: u32 = 50;

/// 容量允许范围（GiB，闭区间）。
pub const MIN_CAPACITY_GB: u32 = 1;
pub const MAX_CAPACITY_GB: u32 = 4096;

/// 保留天数允许范围（闭区间）。
pub const MIN_RETENTION_DAYS: u32 = 1;
pub const MAX_RETENTION_DAYS: u32 = 3650;

/// 库用量快照。
///
/// `used_bytes` = `originals/` + `texts/` + `thumbs/` + `visrecall.db` + `visrecall.db-wal`。
/// 容量按 GiB 存储但这里统一换成字节，避免前后端各写一遍换算。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageUsage {
    pub used_bytes: u64,
    pub capacity_bytes: u64,
    pub item_count: u64,
}

impl StorageUsage {
    /// 是否已达软配额（`>=` 即拒绝新捕获；不自动删除任何素材）。
    pub fn is_full(&self) -> bool {
        self.used_bytes >= self.capacity_bytes
    }

    /// 已用占容量的比例（0.0–1.0）；容量为 0 时按 1.0 处理，避免除零。
    pub fn fill_ratio(&self) -> f64 {
        if self.capacity_bytes == 0 {
            return 1.0;
        }
        (self.used_bytes as f64 / self.capacity_bytes as f64).min(1.0)
    }
}

/// 保留策略；`days = None` 表示永久保留。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetentionPolicy {
    pub days: Option<u32>,
}

impl RetentionPolicy {
    /// 永久保留。
    pub fn forever() -> RetentionPolicy {
        RetentionPolicy { days: None }
    }

    /// 是否为永久保留。
    pub fn is_forever(&self) -> bool {
        self.days.is_none()
    }
}

/// 清理预估：会删多少条、释放多少字节。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetentionPreview {
    pub item_count: u64,
    pub bytes: u64,
}

/// 清理结果汇总：成功 N / 失败 M。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PruneReport {
    pub deleted: u64,
    pub failed: u64,
}

/// 库迁移结果。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelocateReport {
    pub new_root: String,
    pub files_copied: u64,
    pub bytes_copied: u64,
}

/// 孤儿文件与缺失缩略图统计。
///
/// - `files` / `bytes`：`originals/`、`texts/` 下未被任何素材引用的文件；
/// - `missing_thumbs`：`thumb_path` 指向不存在文件的素材数（可重建，不提供删除）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrphanStats {
    pub files: u64,
    pub bytes: u64,
    pub missing_thumbs: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_full_boundary_is_inclusive() {
        let full = StorageUsage {
            used_bytes: 100,
            capacity_bytes: 100,
            item_count: 1,
        };
        assert!(full.is_full());
        assert_eq!(full.fill_ratio(), 1.0);

        let not_full = StorageUsage {
            used_bytes: 99,
            capacity_bytes: 100,
            item_count: 1,
        };
        assert!(!not_full.is_full());
        assert!((not_full.fill_ratio() - 0.99).abs() < f64::EPSILON);
    }

    #[test]
    fn fill_ratio_handles_zero_capacity() {
        let usage = StorageUsage {
            used_bytes: 10,
            capacity_bytes: 0,
            item_count: 0,
        };
        assert_eq!(usage.fill_ratio(), 1.0);
    }

    #[test]
    fn retention_defaults_to_forever() {
        assert!(RetentionPolicy::default().is_forever());
        assert!(!RetentionPolicy { days: Some(90) }.is_forever());
    }

    #[test]
    fn orphan_stats_default_is_zero() {
        assert_eq!(
            OrphanStats::default(),
            OrphanStats {
                files: 0,
                bytes: 0,
                missing_thumbs: 0
            }
        );
    }
}
