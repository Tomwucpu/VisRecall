//! 迁移清单与应用逻辑。
//!
//! DDL 以 SQL 文件内联进二进制（`include_str!`），按 `version` 升序应用；
//! 每条迁移在独立事务中执行，成功后写入 `schema_migrations`。
//! PRAGMA（journal_mode/foreign_keys/busy_timeout）在打开连接时设置，不进入迁移事务。

use rusqlite::{params, Connection};
use visrecall_domain::{now_millis, AppError};

use crate::db::db_error;

/// 已发布的迁移，按版本升序排列。新增迁移时追加条目。
pub const MIGRATIONS: &[(i64, &str)] = &[(1, include_str!("../migrations/0001_core.sql"))];

const MIGRATIONS_TABLE: &str = "CREATE TABLE IF NOT EXISTS schema_migrations (\n  version INTEGER PRIMARY KEY,\n  applied_at INTEGER NOT NULL\n)";

/// 确保 `schema_migrations` 存在，并应用所有未执行过的迁移。
pub fn apply(conn: &mut Connection) -> Result<(), AppError> {
    conn.execute_batch(MIGRATIONS_TABLE).map_err(db_error)?;

    let applied = applied_versions(conn)?;
    for (version, sql) in MIGRATIONS {
        if applied.contains(version) {
            continue;
        }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_are_ordered_and_non_empty() {
        assert!(!MIGRATIONS.is_empty());
        assert!(MIGRATIONS.iter().all(|(_, sql)| !sql.trim().is_empty()));
        assert!(MIGRATIONS.windows(2).all(|pair| pair[0].0 < pair[1].0));
        assert_eq!(MIGRATIONS[0].0, 1);
    }

    #[test]
    fn applying_twice_is_idempotent() {
        let mut conn = Connection::open_in_memory().unwrap();
        apply(&mut conn).unwrap();
        apply(&mut conn).unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count as usize, MIGRATIONS.len());
    }
}
