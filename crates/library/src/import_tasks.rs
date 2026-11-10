//! 主动导入批次与分页候选项的持久化。
//!
//! 这些表只在**任务运行期**存在：任务进终态后立即删行（结果随 `import_task_finished`
//! 事件推给前端），库里不留导入历史。唯一长期保留的是 `scanned` 状态的目录扫描任务，
//! 作为「关闭弹窗后继续确认」的载体，下次扫描或确认导入时作废。

use rusqlite::{OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use visrecall_domain::{new_item_id, now_millis, AppError};

use crate::db::{db_error, Library};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportTaskStatus {
    Pending,
    Running,
    Scanned,
    Completed,
    CompletedWithErrors,
    Cancelled,
    Interrupted,
    Failed,
}

impl ImportTaskStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Scanned => "scanned",
            Self::Completed => "completed",
            Self::CompletedWithErrors => "completed_with_errors",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
            Self::Failed => "failed",
        }
    }

    fn parse(raw: &str) -> Self {
        match raw {
            "pending" => Self::Pending,
            "running" => Self::Running,
            "scanned" => Self::Scanned,
            "completed" => Self::Completed,
            "completed_with_errors" => Self::CompletedWithErrors,
            "cancelled" => Self::Cancelled,
            "interrupted" => Self::Interrupted,
            _ => Self::Failed,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportTask {
    pub id: String,
    pub source_id: Option<String>,
    pub kind: String,
    pub status: ImportTaskStatus,
    pub total_count: u32,
    pub processed_count: u32,
    pub succeeded_count: u32,
    pub skipped_count: u32,
    pub failed_count: u32,
    pub original_bytes: u64,
    pub cancel_requested: bool,
    pub options_json: String,
    pub error_summary: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug)]
pub struct NewImportTask {
    pub source_id: Option<String>,
    pub kind: String,
    pub options_json: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportTaskEntry {
    pub rowid: i64,
    pub task_id: String,
    pub entry_key: String,
    pub relative_path: String,
    pub display_name: String,
    pub size: u64,
    pub status: String,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub item_id: Option<String>,
}

impl Library {
    pub fn create_import_task(&self, new: NewImportTask) -> Result<ImportTask, AppError> {
        let id = new_item_id();
        let now = now_millis();
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO import_tasks(id, source_id, kind, status, options_json, created_at, updated_at)
             VALUES (?1, ?2, ?3, 'pending', ?4, ?5, ?5)",
            rusqlite::params![id, new.source_id, new.kind, new.options_json, now],
        )
        .map_err(db_error)?;
        drop(conn);
        self.get_import_task(&id)?.ok_or(AppError::NotFound)
    }

    pub fn get_import_task(&self, id: &str) -> Result<Option<ImportTask>, AppError> {
        let conn = self.conn()?;
        conn.query_row(&format!("{TASK_SELECT} WHERE id = ?1"), [id], task_from_row)
            .optional()
            .map_err(db_error)
    }

    /// 未结束的任务（状态栏进度 + 弹窗恢复未完成的目录扫描）。
    ///
    /// 导入任务完成后即被删除，所以这里不会返回导入历史。
    pub fn list_import_tasks(&self, limit: u32) -> Result<Vec<ImportTask>, AppError> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(&format!("{TASK_SELECT} ORDER BY updated_at DESC LIMIT ?1"))
            .map_err(db_error)?;
        let rows = stmt
            .query_map([i64::from(limit.clamp(1, 100))], task_from_row)
            .map_err(db_error)?;
        rows.map(|row| row.map_err(db_error)).collect()
    }

    pub fn set_import_task_status(
        &self,
        id: &str,
        status: ImportTaskStatus,
    ) -> Result<(), AppError> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE import_tasks SET status = ?2, updated_at = ?3 WHERE id = ?1",
            rusqlite::params![id, status.as_str(), now_millis()],
        )
        .map_err(db_error)?;
        Ok(())
    }

    /// 扫描尚未生成最终分页清单时，先持久化聚合进度供前端轮询。
    pub fn update_import_task_scan_progress(
        &self,
        id: &str,
        total_count: u32,
        skipped_count: u32,
        original_bytes: u64,
    ) -> Result<(), AppError> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE import_tasks SET total_count = ?2, skipped_count = ?3,
               original_bytes = ?4, updated_at = ?5 WHERE id = ?1",
            rusqlite::params![
                id,
                i64::from(total_count),
                i64::from(skipped_count),
                original_bytes as i64,
                now_millis()
            ],
        )
        .map_err(db_error)?;
        Ok(())
    }

    pub fn replace_import_task_entries(
        &self,
        id: &str,
        entries: &[ImportTaskEntry],
        original_bytes: u64,
    ) -> Result<(), AppError> {
        let mut conn = self.conn()?;
        let tx = conn.transaction().map_err(db_error)?;
        tx.execute("DELETE FROM import_task_entries WHERE task_id = ?1", [id])
            .map_err(db_error)?;
        for entry in entries {
            tx.execute(
                "INSERT INTO import_task_entries
                   (task_id, entry_key, relative_path, display_name, size, status,
                    error_code, error_message, item_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                rusqlite::params![
                    id,
                    entry.entry_key,
                    entry.relative_path,
                    entry.display_name,
                    entry.size as i64,
                    entry.status,
                    entry.error_code,
                    entry.error_message,
                    entry.item_id,
                ],
            )
            .map_err(db_error)?;
        }
        tx.execute(
            "UPDATE import_tasks SET total_count = ?2, original_bytes = ?3,
               skipped_count = (SELECT COUNT(*) FROM import_task_entries
                                WHERE task_id = ?1 AND status = 'skipped'),
               updated_at = ?4 WHERE id = ?1",
            rusqlite::params![
                id,
                entries.len() as i64,
                original_bytes as i64,
                now_millis()
            ],
        )
        .map_err(db_error)?;
        tx.commit().map_err(db_error)?;
        Ok(())
    }

    pub fn list_import_task_entries(
        &self,
        task_id: &str,
        cursor: Option<i64>,
        limit: u32,
    ) -> Result<Vec<ImportTaskEntry>, AppError> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT rowid, task_id, entry_key, relative_path, display_name, size,
                        status, error_code, error_message, item_id
                 FROM import_task_entries
                 WHERE task_id = ?1 AND (?2 IS NULL OR rowid > ?2)
                 ORDER BY rowid LIMIT ?3",
            )
            .map_err(db_error)?;
        let rows = stmt
            .query_map(
                rusqlite::params![task_id, cursor, i64::from(limit.clamp(1, 200))],
                entry_from_row,
            )
            .map_err(db_error)?;
        rows.map(|row| row.map_err(db_error)).collect()
    }

    pub fn ready_import_task_entries(
        &self,
        task_id: &str,
    ) -> Result<Vec<ImportTaskEntry>, AppError> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT rowid, task_id, entry_key, relative_path, display_name, size,
                        status, error_code, error_message, item_id
                 FROM import_task_entries WHERE task_id = ?1 AND status = 'ready' ORDER BY rowid",
            )
            .map_err(db_error)?;
        let rows = stmt
            .query_map([task_id], entry_from_row)
            .map_err(db_error)?;
        rows.map(|row| row.map_err(db_error)).collect()
    }

    /// 失败条目（终态事件用）：任务行随即被删除，明细只能靠这次读取带出。
    pub fn failed_import_task_entries(
        &self,
        task_id: &str,
        limit: u32,
    ) -> Result<Vec<ImportTaskEntry>, AppError> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT rowid, task_id, entry_key, relative_path, display_name, size,
                        status, error_code, error_message, item_id
                 FROM import_task_entries WHERE task_id = ?1 AND status = 'failed'
                 ORDER BY rowid LIMIT ?2",
            )
            .map_err(db_error)?;
        let rows = stmt
            .query_map(
                rusqlite::params![task_id, i64::from(limit.clamp(1, 200))],
                entry_from_row,
            )
            .map_err(db_error)?;
        rows.map(|row| row.map_err(db_error)).collect()
    }

    pub fn append_import_task_entry(&self, entry: &ImportTaskEntry) -> Result<(), AppError> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO import_task_entries
               (task_id, entry_key, relative_path, display_name, size, status,
                error_code, error_message, item_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                entry.task_id,
                entry.entry_key,
                entry.relative_path,
                entry.display_name,
                entry.size as i64,
                entry.status,
                entry.error_code,
                entry.error_message,
                entry.item_id
            ],
        )
        .map_err(db_error)?;
        Ok(())
    }

    pub fn recount_import_task(&self, task_id: &str) -> Result<(), AppError> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE import_tasks SET
               total_count = (SELECT COUNT(*) FROM import_task_entries WHERE task_id = ?1),
               original_bytes = (SELECT COALESCE(SUM(size), 0) FROM import_task_entries WHERE task_id = ?1),
               processed_count = (SELECT COUNT(*) FROM import_task_entries
                                  WHERE task_id = ?1 AND status IN ('imported','skipped','failed')),
               succeeded_count = (SELECT COUNT(*) FROM import_task_entries WHERE task_id = ?1 AND status = 'imported'),
               skipped_count = (SELECT COUNT(*) FROM import_task_entries WHERE task_id = ?1 AND status = 'skipped'),
               failed_count = (SELECT COUNT(*) FROM import_task_entries WHERE task_id = ?1 AND status = 'failed'),
               updated_at = ?2 WHERE id = ?1",
            rusqlite::params![task_id, now_millis()],
        ).map_err(db_error)?;
        Ok(())
    }

    pub fn set_import_task_error(&self, task_id: &str, message: &str) -> Result<(), AppError> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE import_tasks SET status = 'failed', error_summary = ?2, updated_at = ?3 WHERE id = ?1",
            rusqlite::params![task_id, message, now_millis()],
        ).map_err(db_error)?;
        Ok(())
    }

    pub fn update_import_task_entry(
        &self,
        rowid: i64,
        status: &str,
        item_id: Option<&str>,
        error_code: Option<&str>,
        error_message: Option<&str>,
    ) -> Result<(), AppError> {
        let mut conn = self.conn()?;
        let tx = conn.transaction().map_err(db_error)?;
        let task_id: String = tx
            .query_row(
                "SELECT task_id FROM import_task_entries WHERE rowid = ?1",
                [rowid],
                |row| row.get(0),
            )
            .map_err(db_error)?;
        tx.execute(
            "UPDATE import_task_entries SET status = ?2, item_id = ?3,
               error_code = ?4, error_message = ?5 WHERE rowid = ?1",
            rusqlite::params![rowid, status, item_id, error_code, error_message],
        )
        .map_err(db_error)?;
        tx.execute(
            "UPDATE import_tasks SET
               processed_count = (SELECT COUNT(*) FROM import_task_entries
                                  WHERE task_id = ?1 AND status IN ('imported','skipped','failed')),
               succeeded_count = (SELECT COUNT(*) FROM import_task_entries
                                  WHERE task_id = ?1 AND status = 'imported'),
               skipped_count = (SELECT COUNT(*) FROM import_task_entries
                                WHERE task_id = ?1 AND status = 'skipped'),
               failed_count = (SELECT COUNT(*) FROM import_task_entries
                               WHERE task_id = ?1 AND status = 'failed'),
               updated_at = ?2 WHERE id = ?1",
            rusqlite::params![task_id, now_millis()],
        )
        .map_err(db_error)?;
        tx.commit().map_err(db_error)?;
        Ok(())
    }

    pub fn request_import_task_cancel(&self, id: &str) -> Result<(), AppError> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE import_tasks SET cancel_requested = 1, updated_at = ?2 WHERE id = ?1",
            rusqlite::params![id, now_millis()],
        )
        .map_err(db_error)?;
        Ok(())
    }

    pub fn import_task_cancel_requested(&self, id: &str) -> Result<bool, AppError> {
        let conn = self.conn()?;
        conn.query_row(
            "SELECT cancel_requested FROM import_tasks WHERE id = ?1",
            [id],
            |row| row.get(0),
        )
        .optional()
        .map_err(db_error)?
        .ok_or(AppError::NotFound)
    }

    /// 删除任务行；`import_task_entries` 由外键级联清理。
    ///
    /// 导入任务不保留历史：终态结果通过 `import_task_finished` 事件推给前端后即调用本方法。
    pub fn delete_import_task(&self, id: &str) -> Result<(), AppError> {
        let conn = self.conn()?;
        conn.execute("DELETE FROM import_tasks WHERE id = ?1", [id])
            .map_err(db_error)?;
        Ok(())
    }

    /// 一个任务下所有候选文件的相对路径（不区分 ready/skipped）。
    ///
    /// 刷新用它判断“扫描里还出现的文件”，已存在而跳过的条目同样算“仍然存在”。
    pub fn import_task_entry_paths(&self, task_id: &str) -> Result<Vec<String>, AppError> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare("SELECT relative_path FROM import_task_entries WHERE task_id = ?1")
            .map_err(db_error)?;
        let rows = stmt.query_map([task_id], |row| row.get(0)).map_err(db_error)?;
        rows.map(|row| row.map_err(db_error)).collect()
    }

    /// 删除某来源遗留的目录扫描任务（条目级联删除）。
    ///
    /// 目录扫描任务只在“扫描完待确认”期间保留，供弹窗重新打开后恢复；
    /// 重新扫描同一目录或完成导入后，旧扫描结果立即作废，不留在库里当历史。
    pub fn delete_directory_scan_tasks(&self, source_id: &str) -> Result<u64, AppError> {
        let conn = self.conn()?;
        let count = conn
            .execute(
                "DELETE FROM import_tasks WHERE source_id = ?1 AND kind = 'directory_scan'",
                [source_id],
            )
            .map_err(db_error)?;
        Ok(count as u64)
    }

    /// 应用重启后丢弃未完成的导入任务：不保留历史，也不自动恢复扫描或云端任务。
    pub fn discard_unfinished_import_tasks(&self) -> Result<u64, AppError> {
        let conn = self.conn()?;
        let count = conn
            .execute(
                "DELETE FROM import_tasks WHERE status IN ('pending','running')",
                [],
            )
            .map_err(db_error)?;
        Ok(count as u64)
    }
}

const TASK_SELECT: &str = "SELECT id, source_id, kind, status, total_count, processed_count,
  succeeded_count, skipped_count, failed_count, original_bytes, cancel_requested,
  options_json, error_summary, created_at, updated_at FROM import_tasks";

fn task_from_row(row: &Row<'_>) -> rusqlite::Result<ImportTask> {
    let status: String = row.get(3)?;
    Ok(ImportTask {
        id: row.get(0)?,
        source_id: row.get(1)?,
        kind: row.get(2)?,
        status: ImportTaskStatus::parse(&status),
        total_count: row.get::<_, i64>(4)? as u32,
        processed_count: row.get::<_, i64>(5)? as u32,
        succeeded_count: row.get::<_, i64>(6)? as u32,
        skipped_count: row.get::<_, i64>(7)? as u32,
        failed_count: row.get::<_, i64>(8)? as u32,
        original_bytes: row.get::<_, i64>(9)? as u64,
        cancel_requested: row.get(10)?,
        options_json: row.get(11)?,
        error_summary: row.get(12)?,
        created_at: row.get(13)?,
        updated_at: row.get(14)?,
    })
}

fn entry_from_row(row: &Row<'_>) -> rusqlite::Result<ImportTaskEntry> {
    Ok(ImportTaskEntry {
        rowid: row.get(0)?,
        task_id: row.get(1)?,
        entry_key: row.get(2)?,
        relative_path: row.get(3)?,
        display_name: row.get(4)?,
        size: row.get::<_, i64>(5)? as u64,
        status: row.get(6)?,
        error_code: row.get(7)?,
        error_message: row.get(8)?,
        item_id: row.get(9)?,
    })
}
