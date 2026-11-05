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

/// 本地库句柄：库根目录 + 写连接。
pub struct Library {
    root: PathBuf,
    write: Mutex<Connection>,
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
        migrations::apply(&mut conn)?;

        let library = Library {
            root,
            write: Mutex::new(conn),
        };

        match library.cleanup_startup() {
            Ok(cleanup) => tracing::info!(
                tmp_removed = cleanup.tmp_removed,
                orphans = cleanup.orphans,
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

    pub(crate) fn conn(&self) -> Result<MutexGuard<'_, Connection>, AppError> {
        self.write
            .lock()
            .map_err(|_| AppError::Internal("library connection poisoned".into()))
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
