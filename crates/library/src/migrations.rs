//! 迁移清单与应用逻辑。
//!
//! DDL 以 SQL 文件内联进二进制（`include_str!`），按 `version` 升序应用；
//! 每条迁移在独立事务中执行，成功后写入 `schema_migrations`。
//! PRAGMA（journal_mode/foreign_keys/busy_timeout）在打开连接时设置，不进入迁移事务。
//!
//! 检测到待应用迁移时，先把 `visrecall.db` 复制到 `backups/visrecall.db.bak-<version>`
//! （保留最近 2 份）。备份是**最佳努力**：失败只记 `warn`，不阻断迁移——迁移本身在
//! 事务里，失败会整条回滚，备份只是多一层保险。

use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection};
use visrecall_domain::{now_millis, AppError};

use crate::db::{db_error, DB_FILE};

/// 已发布的迁移，按版本升序排列。新增迁移时追加条目。
pub const MIGRATIONS: &[(i64, &str)] = &[
    (1, include_str!("../migrations/0001_core.sql")),
    (2, include_str!("../migrations/0002_library_detail.sql")),
];

const MIGRATIONS_TABLE: &str = "CREATE TABLE IF NOT EXISTS schema_migrations (\n  version INTEGER PRIMARY KEY,\n  applied_at INTEGER NOT NULL\n)";

/// 迁移前备份目录名（库根目录下）。
const BACKUP_DIR: &str = "backups";

/// 迁移前备份的文件名前缀；完整名称为 `<前缀><目标版本>`。
const BACKUP_PREFIX: &str = "visrecall.db.bak-";

/// 保留的迁移前备份份数。
const BACKUP_KEEP: usize = 2;

/// 确保 `schema_migrations` 存在，并应用所有未执行过的迁移。
///
/// `root` 是库根目录，仅用于迁移前备份（`backups/visrecall.db.bak-<version>`）。
pub fn apply(conn: &mut Connection, root: &Path) -> Result<(), AppError> {
    conn.execute_batch(MIGRATIONS_TABLE).map_err(db_error)?;

    let applied = applied_versions(conn)?;
    let pending: Vec<&(i64, &str)> = MIGRATIONS
        .iter()
        .filter(|(version, _)| !applied.contains(version))
        .collect();

    if let Some((version, _)) = pending.last() {
        // 全新库（`schema_migrations` 为空）没有旧结构可保护，不必留一份空备份。
        if !applied.is_empty() {
            backup_before_migration(conn, root, *version);
        }
    }

    for (version, sql) in pending {
        let tx = conn.transaction().map_err(db_error)?;
        tx.execute_batch(sql).map_err(db_error)?;
        tx.execute(
            "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
            params![version, now_millis()],
        )
        .map_err(db_error)?;
        tx.commit().map_err(db_error)?;

        tracing::info!(version, "applied database migration");
    }

    Ok(())
}

fn applied_versions(conn: &Connection) -> Result<Vec<i64>, AppError> {
    let mut stmt = conn
        .prepare("SELECT version FROM schema_migrations ORDER BY version")
        .map_err(db_error)?;
    let rows = stmt
        .query_map([], |row| row.get::<_, i64>(0))
        .map_err(db_error)?;

    let mut versions = Vec::new();
    for row in rows {
        versions.push(row.map_err(db_error)?);
    }
    Ok(versions)
}

/// 把 `visrecall.db` 复制到 `backups/visrecall.db.bak-<version>`，返回备份路径。
///
/// 这里的 `wal_checkpoint(TRUNCATE)` 是**最佳努力**：迁移发生在开库流程中，此时还没有
/// 其他连接持有快照，备份失败也不会阻断迁移，所以不判定 `busy`。严格判定（`busy != 0`
/// 即失败）只用于存储位置迁移（见 `Library::checkpoint`）。
///
/// 库文件不存在（新库首次迁移）时直接返回 `None`。
fn backup_before_migration(conn: &Connection, root: &Path, version: i64) -> Option<PathBuf> {
    let source = root.join(DB_FILE);
    if !source.exists() {
        return None;
    }

    if let Err(err) = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);") {
        tracing::warn!(%err, "checkpoint before migration backup failed");
    }

    let backups = root.join(BACKUP_DIR);
    if let Err(err) = fs::create_dir_all(&backups) {
        tracing::warn!(path = %backups.display(), %err, "create backup dir failed");
        return None;
    }

    let target = backups.join(format!("{BACKUP_PREFIX}{version}"));
    if let Err(err) = fs::copy(&source, &target) {
        tracing::warn!(
            from = %source.display(),
            to = %target.display(),
            %err,
            "migration backup failed"
        );
        return None;
    }

    prune_backups(&backups, BACKUP_KEEP);
    tracing::info!(
        version,
        path = %target.display(),
        "backed up library before migration"
    );
    Some(target)
}

/// 只保留版本号最大的 `keep` 份备份，其余删除（失败只记 warn）。
fn prune_backups(dir: &Path, keep: usize) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) => {
            tracing::warn!(path = %dir.display(), %err, "read backup dir failed");
            return;
        }
    };

    let mut backups: Vec<(i64, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(version) = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_prefix(BACKUP_PREFIX))
            .and_then(|suffix| suffix.parse::<i64>().ok())
        else {
            continue;
        };
        backups.push((version, path));
    }

    backups.sort_by(|a, b| b.0.cmp(&a.0));
    for (version, path) in backups.into_iter().skip(keep) {
        match fs::remove_file(&path) {
            Ok(()) => {
                tracing::info!(version, path = %path.display(), "pruned old migration backup")
            }
            Err(err) => {
                tracing::warn!(path = %path.display(), %err, "prune backup failed");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_file_db(dir: &Path) -> Connection {
        let conn = Connection::open(dir.join(DB_FILE)).unwrap();
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
            .unwrap();
        conn
    }

    /// 造一个只应用过 0001 的库，用于验证 0002 的增量迁移与迁移前备份。
    fn open_v1_db(dir: &Path) -> Connection {
        let conn = open_file_db(dir);
        conn.execute_batch(MIGRATIONS_TABLE).unwrap();
        conn.execute_batch(MIGRATIONS[0].1).unwrap();
        conn.execute(
            "INSERT INTO schema_migrations (version, applied_at) VALUES (1, ?1)",
            [now_millis()],
        )
        .unwrap();
        conn
    }

    fn insert_item(conn: &Connection, id: &str) {
        conn.execute(
            "INSERT INTO items (id, kind, status, captured_at, original_path, content_hash, created_at, updated_at)
             VALUES (?1, 'image', 'ready', 0, 'originals/images/a.png', 'hash', 0, 0)",
            [id],
        )
        .unwrap();
    }

    #[test]
    fn migrations_are_ordered_and_non_empty() {
        assert!(!MIGRATIONS.is_empty());
        assert!(MIGRATIONS.iter().all(|(_, sql)| !sql.trim().is_empty()));
        assert!(MIGRATIONS.windows(2).all(|pair| pair[0].0 < pair[1].0));
        assert_eq!(MIGRATIONS[0].0, 1);
    }

    #[test]
    fn applying_twice_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let mut conn = open_file_db(dir.path());
        apply(&mut conn, dir.path()).unwrap();
        apply(&mut conn, dir.path()).unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count as usize, MIGRATIONS.len());
        assert_eq!(count, 2);
    }

    #[test]
    fn migration_0002_is_writable_and_cascades_from_items() {
        let dir = tempfile::tempdir().unwrap();
        let mut conn = open_v1_db(dir.path());
        apply(&mut conn, dir.path()).unwrap();

        insert_item(&conn, "item-1");
        conn.execute(
            "INSERT INTO tags (id, name, source, created_at) VALUES ('tag-1', 'work', 'manual', 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO item_tags (item_id, tag_id, origin, created_at) VALUES ('item-1', 'tag-1', 'manual', 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tag_suppressions (item_id, tag_name, created_at) VALUES ('item-1', 'auto', 0)",
            [],
        )
        .unwrap();

        // 外键已由 open_file_db 打开：删除 items 行会级联清空 item_tags 与 tag_suppressions。
        conn.execute("DELETE FROM items WHERE id='item-1'", [])
            .unwrap();
        let links: i64 = conn
            .query_row("SELECT COUNT(*) FROM item_tags", [], |row| row.get(0))
            .unwrap();
        let suppressions: i64 = conn
            .query_row("SELECT COUNT(*) FROM tag_suppressions", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(links, 0);
        assert_eq!(suppressions, 0);
    }

    #[test]
    fn ncase_index_rejects_duplicate_tag_names() {
        let dir = tempfile::tempdir().unwrap();
        let mut conn = open_file_db(dir.path());
        apply(&mut conn, dir.path()).unwrap();

        conn.execute(
            "INSERT INTO tags (id, name, source, created_at) VALUES ('a', 'Work', 'manual', 0)",
            [],
        )
        .unwrap();
        assert!(conn
            .execute(
                "INSERT INTO tags (id, name, source, created_at) VALUES ('b', 'work', 'manual', 0)",
                [],
            )
            .is_err());
    }

    #[test]
    fn migration_creates_one_backup_for_pending_version() {
        let dir = tempfile::tempdir().unwrap();
        let mut conn = open_v1_db(dir.path());
        apply(&mut conn, dir.path()).unwrap();

        let backups = dir.path().join(BACKUP_DIR);
        let names: Vec<String> = fs::read_dir(&backups)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["visrecall.db.bak-2".to_string()]);
        assert!(backups.join("visrecall.db.bak-2").metadata().unwrap().len() > 0);
    }

    #[test]
    fn fresh_library_has_no_backup() {
        let dir = tempfile::tempdir().unwrap();
        let mut conn = open_file_db(dir.path());
        apply(&mut conn, dir.path()).unwrap();

        // 全新库没有旧结构，不产生备份（`backups/` 也不会被创建）。
        let backups = dir.path().join(BACKUP_DIR);
        assert!(!backups.exists());
    }

    #[test]
    fn prune_keeps_newest_backups_only() {
        let dir = tempfile::tempdir().unwrap();
        for version in 1..=4 {
            fs::write(dir.path().join(format!("{BACKUP_PREFIX}{version}")), b"bak").unwrap();
        }
        prune_backups(dir.path(), 2);

        let mut remaining: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        remaining.sort();
        assert_eq!(
            remaining,
            vec![
                "visrecall.db.bak-3".to_string(),
                "visrecall.db.bak-4".to_string()
            ]
        );
    }
}
