//! SQLite 连接与库句柄。
//!
//! 一个 `Library` 对应库根目录下的 `visrecall.db`。写连接放在 [`Mutex`] 中，
//! 阶段 01 的写入量极小，单连接串行化足以避免 `SQLITE_BUSY`。
//! 读操作同样走该连接（WAL 下同一个连接可读写）。

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use rusqlite::{Connection, OptionalExtension};
use visrecall_domain::{now_millis, AppError};

use crate::migrations;
use crate::paths::ensure_library_root;

/// 库文件名（位于库根目录）。
pub const DB_FILE: &str = "visrecall.db";

/// 打开连接时执行的 PRAGMA；不放在迁移事务里。
const OPEN_PRAGMAS: &str =
    "PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;";

/// 本地库句柄：库根目录 + 写连接 + 元数据变更钩子。
pub struct Library {
    root: PathBuf,
    write: Mutex<Connection>,
    /// 阶段 03：备注/标签变更后由上层（app-core）注册的「重算索引」回调。
    pub(crate) metadata_hook: Mutex<Option<crate::hooks::MetadataHook>>,
}

impl Library {
    /// 打开（必要时初始化）库：建目录与 `library.json` → 打开 `visrecall.db`
    /// → 设置 PRAGMA → 应用未执行的迁移 → 启动清理（design §3.3 第 4 步）。
    ///
    /// 启动清理失败只记录 warn，不影响打开库。
    pub fn open(root: PathBuf) -> Result<Library, AppError> {
        ensure_library_root(&root)?;

        let mut conn = Connection::open(root.join(DB_FILE)).map_err(db_error)?;
        conn.execute_batch(OPEN_PRAGMAS).map_err(db_error)?;
        // 传入根目录：迁移前要把 visrecall.db 备份到 backups/。
        migrations::apply(&mut conn, &root)?;

        let library = Library {
            root,
            write: Mutex::new(conn),
            metadata_hook: Mutex::new(None),
        };

        library.self_check_index();

        match library.cleanup_startup() {
            Ok(cleanup) => tracing::info!(
                tmp_removed = cleanup.tmp_removed,
                orphans = cleanup.orphans,
                // 用户报障「列表灰块/文件不见了」时，这一项能直接对账（P2-R06）。
                missing_thumbs = cleanup.missing_thumbs,
                "startup cleanup finished"
            ),
            Err(err) => tracing::warn!(%err, "startup cleanup failed"),
        }

        Ok(library)
    }

    /// 库根目录（调用方传入的原始路径，未 canonicalize）。
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 把库内相对路径解析为绝对路径：拒绝绝对路径与 `..`，
    /// 拼接后 `canonicalize` 并校验仍位于库根目录内。
    pub fn relative_to_root(&self, rel: &str) -> Result<PathBuf, AppError> {
        let relative = Path::new(rel);
        if !is_safe_relative(relative) {
            return Err(AppError::InvalidInput(format!(
                "path escapes library root: {rel}"
            )));
        }

        let root = fs::canonicalize(&self.root)
            .map_err(|err| AppError::Io(format!("canonicalize {}: {err}", self.root.display())))?;
        let joined = root.join(relative);
        let canonical = fs::canonicalize(&joined)
            .map_err(|err| AppError::Io(format!("canonicalize {}: {err}", joined.display())))?;

        if !canonical.starts_with(&root) {
            return Err(AppError::InvalidInput(format!(
                "path escapes library root: {rel}"
            )));
        }
        Ok(canonical)
    }

    /// 把库内相对路径解析为绝对路径，**不要求文件存在**。
    ///
    /// [`Library::relative_to_root`] 依赖 `canonicalize`，只能解析已存在的文件；
    /// 删除流程需要「文件可能已经被用户手工删掉」的路径解析，因此单独提供：
    /// 相对路径本身先做组件校验（拒绝绝对路径、盘符、`..`），文件存在时再
    /// canonicalize 并复核仍位于库根目录内（防符号链接逃逸）。
    pub fn resolve_existing_or_join(&self, rel: &str) -> Result<PathBuf, AppError> {
        let relative = Path::new(rel);
        if !is_safe_relative(relative) {
            return Err(AppError::InvalidInput(format!(
                "path escapes library root: {rel}"
            )));
        }

        let root = fs::canonicalize(&self.root)
            .map_err(|err| AppError::Io(format!("canonicalize {}: {err}", self.root.display())))?;
        let joined = root.join(relative);
        if !joined.exists() {
            return Ok(joined);
        }

        let canonical = fs::canonicalize(&joined)
            .map_err(|err| AppError::Io(format!("canonicalize {}: {err}", joined.display())))?;
        if !canonical.starts_with(&root) {
            return Err(AppError::InvalidInput(format!(
                "path escapes library root: {rel}"
            )));
        }
        Ok(canonical)
    }

    /// 启动恢复：把遗留的 `running` 任务改回 `queued`，返回恢复条数。
    pub fn recover_jobs(&self) -> Result<u64, AppError> {
        let conn = self.conn()?;
        let updated = conn
            .execute(
                "UPDATE jobs SET state='queued', updated_at=?1 WHERE state='running'",
                [now_millis()],
            )
            .map_err(db_error)?;
        Ok(updated as u64)
    }

    /// 入队任务（阶段 01 只用于缩略图与恢复测试）。同一 `(item_id, stage, revision)` 幂等覆盖。
    pub fn enqueue_job(&self, item_id: &str, stage: &str, revision: i64) -> Result<(), AppError> {
        let now = now_millis();
        let id = uuid::Uuid::new_v4().to_string();
        let conn = self.conn()?;
        conn.execute(
            "INSERT OR REPLACE INTO jobs
               (id, item_id, stage, revision, state, priority, attempts, next_run_at, last_error, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, 'queued', 1, 0, NULL, NULL, ?5, ?5)",
            rusqlite::params![id, item_id, stage, revision, now],
        )
        .map_err(db_error)?;
        Ok(())
    }

    /// 取 `(item_id, stage)` 最新 revision 的 state；不存在返回 [`AppError::NotFound`]。
    pub fn job_state(&self, item_id: &str, stage: &str) -> Result<String, AppError> {
        let conn = self.conn()?;
        let state = conn
            .query_row(
                "SELECT state FROM jobs WHERE item_id=?1 AND stage=?2 ORDER BY revision DESC LIMIT 1",
                rusqlite::params![item_id, stage],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(db_error)?;
        state.ok_or(AppError::NotFound)
    }

    /// 测试辅助：直接把该 `(item_id, stage)` 的任务置为 `running`。
    ///
    /// 生产路径通过任务状态机修改状态；这里绕开它以模拟进程被杀时的遗留状态。
    #[doc(hidden)]
    pub fn mark_job_running_for_test(&self, item_id: &str, stage: &str) -> Result<(), AppError> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE jobs SET state='running', updated_at=?1 WHERE item_id=?2 AND stage=?3",
            rusqlite::params![now_millis(), item_id, stage],
        )
        .map_err(db_error)?;
        Ok(())
    }

    /// 三写同事务入口（design §6.1 的「一致性模型」）：
    /// `insights` + `items_fts` + 向量表必须在同一事务里写完，任一 `Err` 整体回滚。
    ///
    /// **禁止**把这三处拆成三个独立事务——那样会允许「FTS 搜得到但向量搜不到」的半更新。
    pub fn write_index_tx(
        &self,
        f: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<(), AppError>,
    ) -> Result<(), AppError> {
        let mut conn = self.conn()?;
        let tx = conn.transaction().map_err(db_error)?;
        f(&tx)?;
        tx.commit().map_err(db_error)
    }

    /// 索引行数自检（`items` / `items_fts` / 活跃向量表）。不一致只记 `warn`；
    /// 修复入队由 pipeline 的启动检查完成（Task 9）。
    pub fn index_counts(&self) -> Result<IndexCounts, AppError> {
        let conn = self.conn()?;
        let items: i64 = conn
            .query_row("SELECT COUNT(*) FROM items", [], |row| row.get(0))
            .map_err(db_error)?;
        let fts: i64 = conn
            .query_row("SELECT COUNT(*) FROM items_fts", [], |row| row.get(0))
            .map_err(db_error)?;
        drop(conn);
        let vectors = self.vector_count()?;
        Ok(IndexCounts {
            items: items as u64,
            fts: fts as u64,
            vectors,
        })
    }

    /// 执行「候选集 SQL」（`search::filters::to_sql` 的产物）并返回 `items.rowid`。
    ///
    /// SQL 由上游用编号占位符拼好、参数逐一绑定——本方法不接受字符串插值，
    /// 也不做任何改写（唯一职责是执行与收集）。
    pub fn candidate_rowids(
        &self,
        sql: &str,
        params: &[rusqlite::types::Value],
    ) -> Result<Vec<i64>, AppError> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(sql).map_err(db_error)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(params.iter()), |row| {
                row.get::<_, i64>(0)
            })
            .map_err(db_error)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(db_error)?);
        }
        Ok(out)
    }

    /// 缺失派生数据的素材 `(rowid, item_id)`：没有 FTS 行、或没有活跃向量行。
    ///
    /// 启动自检据此入队修复（P3-R12「缺失派生数据自动入队修复」）。
    pub fn missing_index_rowids(&self, limit: u32) -> Result<Vec<(i64, String)>, AppError> {
        let table = self.active_vector_table();
        if !crate::vectors::is_valid_vector_table(&table) {
            return Err(AppError::InvalidInput(format!(
                "invalid vector table: {table}"
            )));
        }
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT i.rowid, i.id FROM items i
                 LEFT JOIN items_fts f ON f.rowid = i.rowid
                 LEFT JOIN {table} v ON v.item_rowid = i.rowid
                 WHERE f.rowid IS NULL OR v.item_rowid IS NULL
                 ORDER BY i.rowid ASC LIMIT ?1"
            ))
            .map_err(db_error)?;
        let rows = stmt
            .query_map([i64::from(limit.clamp(1, 1000))], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .map_err(db_error)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(db_error)?);
        }
        Ok(out)
    }

    /// 启动自检：派生数据行数不一致时 `warn`（不阻塞开库）。
    pub(crate) fn self_check_index(&self) {
        match self.index_counts() {
            Ok(counts) => {
                if counts.items != counts.fts || counts.items != counts.vectors {
                    tracing::warn!(
                        items = counts.items,
                        fts = counts.fts,
                        vectors = counts.vectors,
                        "derived index rows do not match items; repair will be enqueued on startup"
                    );
                }
            }
            Err(err) => tracing::warn!(%err, "index self check failed"),
        }

        // 分词器版本：首次开库写入，之后不一致只告警（全量重建留给真正需要时，design §1.2）。
        let current = visrecall_domain::search::TOKENIZER_VERSION;
        match self.setting_get::<String>(visrecall_domain::search::TOKENIZER_VERSION_KEY) {
            None => {
                if let Err(err) = self.setting_set(
                    visrecall_domain::search::TOKENIZER_VERSION_KEY,
                    &current.to_string(),
                ) {
                    tracing::warn!(%err, "failed to record tokenizer version");
                }
            }
            Some(stored) if stored != current => tracing::warn!(
                stored = %stored,
                current = %current,
                "fts.tokenizer_version mismatch; index text may not match query tokenizer"
            ),
            Some(_) => {}
        }
    }

    /// 测试辅助：某张表是否存在（不暴露连接）。
    #[doc(hidden)]
    pub fn table_exists_for_test(&self, name: &str) -> Result<bool, AppError> {
        let conn = self.conn()?;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name = ?1",
                [name],
                |row| row.get(0),
            )
            .map_err(db_error)?;
        Ok(count > 0)
    }

    /// 测试辅助：已应用的最高迁移版本。
    #[doc(hidden)]
    pub fn max_schema_version_for_test(&self) -> Result<i64, AppError> {
        let conn = self.conn()?;
        conn.query_row("SELECT MAX(version) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .map_err(db_error)
    }

    pub(crate) fn conn(&self) -> Result<MutexGuard<'_, Connection>, AppError> {
        self.write
            .lock()
            .map_err(|_| AppError::Internal("library connection poisoned".into()))
    }
}

/// 索引行数快照（启动自检用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexCounts {
    pub items: u64,
    pub fts: u64,
    pub vectors: u64,
}

impl IndexCounts {
    /// 三处是否一致。
    pub fn is_consistent(&self) -> bool {
        self.items == self.fts && self.items == self.vectors
    }
}

/// 相对路径只允许普通组件与 `.`，拒绝绝对路径、盘符与 `..`。
fn is_safe_relative(path: &Path) -> bool {
    let mut has_normal = false;
    for component in path.components() {
        match component {
            Component::Normal(_) => has_normal = true,
            Component::CurDir => {}
            Component::Prefix(_) | Component::RootDir | Component::ParentDir => return false,
        }
    }
    has_normal
}

pub(crate) fn db_error(err: rusqlite::Error) -> AppError {
    AppError::Database(err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_paths_outside_library() {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::open(dir.path().to_path_buf()).unwrap();

        assert!(matches!(
            lib.relative_to_root("../escape.png"),
            Err(AppError::InvalidInput(_))
        ));
        assert!(matches!(
            lib.relative_to_root("thumbs/../../escape.png"),
            Err(AppError::InvalidInput(_))
        ));
        assert!(matches!(
            lib.relative_to_root(""),
            Err(AppError::InvalidInput(_))
        ));
        let absolute = dir.path().join("thumbs").join("a.png");
        assert!(matches!(
            lib.relative_to_root(&absolute.to_string_lossy()),
            Err(AppError::InvalidInput(_))
        ));
    }

    #[test]
    fn resolves_existing_file_inside_library() {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::open(dir.path().to_path_buf()).unwrap();
        let rel = Path::new("thumbs").join("a.png");
        fs::write(dir.path().join(&rel), b"png").unwrap();

        let resolved = lib.relative_to_root(&rel.to_string_lossy()).unwrap();
        assert_eq!(resolved, fs::canonicalize(dir.path().join(&rel)).unwrap());
    }
}
