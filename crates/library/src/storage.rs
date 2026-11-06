//! 用量统计、容量与保留策略、孤儿统计、WAL checkpoint（design §6.7、§6.8）。
//!
//! 三处硬约束：
//! 1. **策略存库内 `settings` 表**（D2-05）：容量与保留天数属于「这个库」，
//!    `config.json` 跟安装走，换库后会张冠李戴；
//! 2. **孤儿扫描范围是白名单**：只认 `originals/`、`texts/`，绝不遍历库根目录，
//!    也不触碰 `visrecall.db` / `-wal` / `-shm`、`library.json`、`backups/`、`tmp/`；
//! 3. **checkpoint 必须读 `busy` 计数**：`execute_batch` 拿不到返回值，会把
//!    「部分 checkpoint」当成成功，那正是「复制出一个缺帧的库」的入口。

use std::fs;
use std::path::PathBuf;

use rusqlite::OptionalExtension;
use visrecall_domain::storage::{
    OrphanStats, RetentionPolicy, StorageUsage, DEFAULT_CAPACITY_GB, MAX_CAPACITY_GB,
    MAX_RETENTION_DAYS, MIN_CAPACITY_GB, MIN_RETENTION_DAYS,
};
use visrecall_domain::AppError;

use crate::cleanup::{normalize_db_path, relative_slash_path, walk_files, ORPHAN_SCAN_DIRS};
use crate::db::{db_error, Library, DB_FILE};

/// `settings` 表的键名。
pub const KEY_CAPACITY_GB: &str = "storage.capacity_gb";
pub const KEY_RETENTION_DAYS: &str = "storage.retention_days";

const GIB: u64 = 1024 * 1024 * 1024;

impl Library {
    /// 用量快照：三个内容目录 + 主库 + `-wal` 的实际字节数，外加素材条数。
    pub fn usage(&self) -> Result<StorageUsage, AppError> {
        let root = self.root();
        let mut used: u64 = 0;
        for dir in ["originals", "texts", "thumbs"] {
            used += dir_size(&walk_files(&root.join(dir)));
        }
        used += file_size(&root.join(DB_FILE));
        used += file_size(&root.join(format!("{DB_FILE}-wal")));

        Ok(StorageUsage {
            used_bytes: used,
            capacity_bytes: self.capacity_bytes(),
            item_count: self.count_items()?,
        })
    }

    /// 容量（字节）。配置缺失/损坏/越界时按默认 50 GiB 处理。
    pub fn capacity_bytes(&self) -> u64 {
        match self.setting_get::<u32>(KEY_CAPACITY_GB) {
            Some(gb) if (MIN_CAPACITY_GB..=MAX_CAPACITY_GB).contains(&gb) => u64::from(gb) * GIB,
            Some(gb) => {
                tracing::warn!(gb, "capacity out of range; falling back to default");
                u64::from(DEFAULT_CAPACITY_GB) * GIB
            }
            None => u64::from(DEFAULT_CAPACITY_GB) * GIB,
        }
    }

    /// 设置容量（GiB）：先校验 1..=4096，再写库；越界不落盘。
    pub fn set_capacity_gb(&self, gb: u32) -> Result<(), AppError> {
        if !(MIN_CAPACITY_GB..=MAX_CAPACITY_GB).contains(&gb) {
            return Err(AppError::InvalidInput(format!(
                "capacity must be {MIN_CAPACITY_GB}..={MAX_CAPACITY_GB} GB"
            )));
        }
        self.setting_set(KEY_CAPACITY_GB, &gb)
    }

    /// 保留策略；缺失、损坏或越界时回落到「永久保留」。
    pub fn retention(&self) -> RetentionPolicy {
        match self.setting_get::<Option<u32>>(KEY_RETENTION_DAYS) {
            Some(days) => match days {
                None => RetentionPolicy::forever(),
                Some(days) if (MIN_RETENTION_DAYS..=MAX_RETENTION_DAYS).contains(&days) => {
                    RetentionPolicy { days: Some(days) }
                }
                Some(days) => {
                    tracing::warn!(days, "retention out of range; treating as forever");
                    RetentionPolicy::forever()
                }
            },
            None => RetentionPolicy::forever(),
        }
    }

    /// 保存保留策略：`None` = 永久；`Some` 必须在 1..=3650，越界不落盘。
    pub fn set_retention(&self, days: Option<u32>) -> Result<(), AppError> {
        if let Some(days) = days {
            if !(MIN_RETENTION_DAYS..=MAX_RETENTION_DAYS).contains(&days) {
                return Err(AppError::InvalidInput(format!(
                    "retention must be {MIN_RETENTION_DAYS}..={MAX_RETENTION_DAYS} days"
                )));
            }
        }
        self.setting_set(KEY_RETENTION_DAYS, &days)
    }

    /// 孤儿统计：`originals/`、`texts/` 下未被引用的文件；`thumbs/` 只统计缺失。
    pub fn orphans(&self) -> Result<OrphanStats, AppError> {
        let (files, missing_thumbs) = self.orphan_files()?;
        Ok(OrphanStats {
            files: files.len() as u64,
            bytes: files.iter().map(|(_, size)| *size).sum(),
            missing_thumbs,
        })
    }

    /// 删除孤儿文件，返回实际删除数与字节数；`missing_thumbs` 为清理后的现状。
    ///
    /// 逐条删除，失败只记 `warn` 并继续——一个删不掉的文件不该阻断其余清理。
    pub fn cleanup_orphans(&self) -> Result<OrphanStats, AppError> {
        let (files, missing_thumbs) = self.orphan_files()?;
        let mut removed = 0;
        let mut bytes = 0;

        for (path, size) in files {
            match fs::remove_file(&path) {
                Ok(()) => {
                    removed += 1;
                    bytes += size;
                }
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    // 已被外部删掉，按成功计。
                    removed += 1;
                    bytes += size;
                }
                Err(err) => {
                    tracing::warn!(path = %path.display(), %err, "cleanup orphan failed");
                }
            }
        }

        Ok(OrphanStats {
            files: removed,
            bytes,
            missing_thumbs,
        })
    }

    /// `wal_checkpoint(TRUNCATE)`，并把 `busy != 0` 视为失败。
    ///
    /// 调用方必须保证此刻没有其他连接持有读快照（迁移编排在调用前已停止捕获运行时）。
    pub fn checkpoint(&self) -> Result<(), AppError> {
        let conn = self.conn()?;
        let busy: i64 = conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))
            .map_err(db_error)?;
        if busy != 0 {
            return Err(AppError::Busy(format!(
                "checkpoint incomplete ({busy} busy)"
            )));
        }
        Ok(())
    }

    /// 孤儿文件（绝对路径 + 字节数）与缺失缩略图数量。
    ///
    /// 扫描根只来自 [`ORPHAN_SCAN_DIRS`] 常量：新增目录必须改常量，
    /// 避免以后被调用方误扩展到数据库文件所在层级。
    fn orphan_files(&self) -> Result<(Vec<(PathBuf, u64)>, u64), AppError> {
        let root = self.root();
        let referenced = self.referenced_original_paths()?;

        let mut files = Vec::new();
        for dir in ORPHAN_SCAN_DIRS {
            for path in walk_files(&root.join(dir)) {
                let Some(relative) = relative_slash_path(root, &path) else {
                    continue;
                };
                if !referenced.contains(&relative) {
                    files.push((path.clone(), file_size(&path)));
                }
            }
        }

        let missing_thumbs = self.count_missing_thumbs()?;
        Ok((files, missing_thumbs))
    }

    /// `thumb_path` 指向不存在文件的素材数（可重建，不提供删除入口）。
    fn count_missing_thumbs(&self) -> Result<u64, AppError> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare("SELECT thumb_path FROM items WHERE thumb_path IS NOT NULL")
            .map_err(db_error)?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(db_error)?;

        let mut missing = 0;
        for row in rows {
            let rel = row.map_err(db_error)?;
            let path = self.root().join(rel.replace('\\', "/"));
            if !path.is_file() {
                missing += 1;
            }
        }
        Ok(missing)
    }

    /// `items.original_path` 的集合，统一成 `/` 分隔的相对路径。
    pub(crate) fn referenced_original_paths(
        &self,
    ) -> Result<std::collections::HashSet<String>, AppError> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare("SELECT original_path FROM items WHERE original_path IS NOT NULL")
            .map_err(db_error)?;
        let rows = stmt
            .query_map([], |row| row.get::<_, Option<String>>(0))
            .map_err(db_error)?;

        let mut paths = std::collections::HashSet::new();
        for row in rows {
            if let Some(path) = row.map_err(db_error)? {
                paths.insert(normalize_db_path(&path));
            }
        }
        Ok(paths)
    }

    /// 测试辅助：读一条 `settings` 原始值（用于断言「越界不落盘」）。
    #[doc(hidden)]
    pub fn raw_setting_for_test(&self, key: &str) -> Result<Option<String>, AppError> {
        let conn = self.conn()?;
        conn.query_row(
            "SELECT value_json FROM settings WHERE key = ?1",
            [key],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(db_error)
    }
}

/// 目录下所有文件的字节数之和；单个文件读不到按 0 计。
pub(crate) fn dir_size(files: &[PathBuf]) -> u64 {
    files.iter().map(|path| file_size(path)).sum()
}

/// 单文件字节数；不存在或不可读按 0 计。
pub(crate) fn file_size(path: &std::path::Path) -> u64 {
    fs::metadata(path).map(|meta| meta.len()).unwrap_or(0)
}
