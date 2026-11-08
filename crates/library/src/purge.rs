//! 单条彻底删除与全库清空（design §6.5、§6.6）。
//!
//! 两条不可恢复操作的共同纪律是**先删库、后删文件**：库是真相来源，反序会在
//! 「文件删了、事务失败」时留下一条永远打不开的素材。文件删除失败不回滚数据库，
//! 记 `warn` 并由孤儿扫描（`library::storage`）计数后交给用户手动清理。

use std::fs;

use visrecall_domain::AppError;

use crate::cleanup::{is_content_dir, walk_files};
use crate::db::Library;

/// 单条删除影响的文件路径（供上层记日志、发事件前使用）。
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct DeletedItem {
    /// 原图/原文的库内相对路径（可能为空字符串，历史数据允许 `NULL`）。
    pub original_path: String,
    /// 缩略图的库内相对路径。
    pub thumb_path: Option<String>,
}

/// 全库清空的结果。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PurgeReport {
    /// 删除的素材条数。
    pub items: u64,
    /// 删除的标签条数。
    pub tags: u64,
    /// 删除的内容文件数（`originals/`、`texts/`、`thumbs/`）。
    pub files_removed: u64,
    /// 删除失败的文件数（不回滚，如实汇报）。
    pub files_failed: u64,
}

impl Library {
    /// 彻底删除一条素材：先在一个事务里删库记录（含 `jobs`，标签关联靠外键级联），
    /// 提交并释放连接后再删文件。
    ///
    /// 文件不存在视为成功（用户可能已经手工删过）；其他失败只记 `warn`，不向上报错——
    /// 库里已经没有这条素材，让用户看到「删除失败」反而会导致重试也无法消除的困惑。
    pub fn delete_item(&self, id: &str) -> Result<DeletedItem, AppError> {
        let (original_path, thumb_path) = self.delete_item_row(id)?.ok_or(AppError::NotFound)?;

        let mut relative_paths = vec![original_path.clone()];
        relative_paths.extend(thumb_path.clone());
        for rel in relative_paths.iter().filter(|rel| !rel.is_empty()) {
            let abs = match self.resolve_existing_or_join(rel) {
                Ok(abs) => abs,
                Err(err) => {
                    tracing::warn!(path = %rel, %err, "skip deleting file outside library");
                    continue;
                }
            };
            match fs::remove_file(&abs) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => {
                    tracing::warn!(path = %abs.display(), %err, "delete file failed");
                }
            }
        }

        Ok(DeletedItem {
            original_path,
            thumb_path,
        })
    }

    /// 清空整个素材库：派生数据与素材行全部删除，内容目录下的文件随后清掉。
    ///
    /// 保留 `schema_migrations`、`settings`（容量/保留策略是用户偏好）、`library.json`
    /// 与数据库文件本身。
    pub fn purge_library(&self) -> Result<PurgeReport, AppError> {
        let (items, tags) = {
            let mut conn = self.conn()?;
            let tx = conn.transaction().map_err(crate::db::db_error)?;

            let items: i64 = tx
                .query_row("SELECT COUNT(*) FROM items", [], |row| row.get(0))
                .map_err(crate::db::db_error)?;
            let tags: i64 = tx
                .query_row("SELECT COUNT(*) FROM tags", [], |row| row.get(0))
                .map_err(crate::db::db_error)?;

            // 先子后父，避免外键顺序问题（jobs/items_fts/queries 没有外键，要显式删）。
            // insights 与向量表靠 `items` 的 ON DELETE CASCADE 清空。
            for sql in [
                "DELETE FROM jobs",
                "DELETE FROM queries",
                "DELETE FROM items_fts",
                "DELETE FROM item_tags",
                "DELETE FROM tag_suppressions",
                "DELETE FROM tags",
                "DELETE FROM items",
            ] {
                tx.execute(sql, []).map_err(crate::db::db_error)?;
            }
            tx.commit().map_err(crate::db::db_error)?;
            (items as u64, tags as u64)
        };

        // 连接 guard 已在此释放：文件 IO 不持有库锁。
        let (files_removed, files_failed) = self.clear_content_dirs()?;

        Ok(PurgeReport {
            items,
            tags,
            files_removed,
            files_failed,
        })
    }

    /// 清空 `originals/`、`texts/`、`thumbs/` 下的普通文件，保留目录本身。
    ///
    /// 返回 `(删除成功数, 删除失败数)`；文件已在外部被删掉按成功计。
    pub fn clear_content_dirs(&self) -> Result<(u64, u64), AppError> {
        let mut removed = 0;
        let mut failed = 0;

        for dir in ["originals", "texts", "thumbs"] {
            debug_assert!(is_content_dir(dir));
            for path in walk_files(&self.root().join(dir)) {
                match fs::remove_file(&path) {
                    Ok(()) => removed += 1,
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => removed += 1,
                    Err(err) => {
                        failed += 1;
                        tracing::warn!(path = %path.display(), %err, "purge: delete file failed");
                    }
                }
            }
        }

        Ok((removed, failed))
    }
}
