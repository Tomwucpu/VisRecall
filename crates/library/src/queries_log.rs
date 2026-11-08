//! `queries` 表：每次检索一行（raw / parsed / 结果 id / 反馈）。
//!
//! 用途仅限评测与「不是这个」反馈闭环（P3-R07、P3-R14）；V0.1 **不提供搜索历史 UI**。

use rusqlite::{params, OptionalExtension, Row};
use visrecall_domain::AppError;

use crate::db::{db_error, Library};

const COLUMNS: &str = "id, raw, parsed_json, result_ids_json, feedback, created_at";

/// 一条检索记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryLog {
    pub id: String,
    pub raw: String,
    /// `ParsedQuery` 的 JSON（chips 回显与调参复盘用）。
    pub parsed_json: Option<String>,
    /// 结果 id 列表的 JSON 数组。
    pub result_ids_json: Option<String>,
    /// 反馈信号，目前只有 `"not-this"`。
    pub feedback: Option<String>,
    pub created_at: i64,
}

impl Library {
    /// 写入一条检索记录（评测/反馈数据）。
    pub fn query_log_insert(&self, log: &QueryLog) -> Result<(), AppError> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT OR REPLACE INTO queries
               (id, raw, parsed_json, result_ids_json, feedback, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                log.id,
                log.raw,
                log.parsed_json,
                log.result_ids_json,
                log.feedback,
                log.created_at
            ],
        )
        .map_err(db_error)?;
        Ok(())
    }

    /// 设置反馈；queryId 不存在返回 [`AppError::NotFound`]。
    pub fn query_log_set_feedback(&self, id: &str, feedback: &str) -> Result<(), AppError> {
        let conn = self.conn()?;
        let updated = conn
            .execute(
                "UPDATE queries SET feedback = ?2 WHERE id = ?1",
                params![id, feedback],
            )
            .map_err(db_error)?;
        if updated == 0 {
            return Err(AppError::NotFound);
        }
        Ok(())
    }

    /// 读取一条记录（测试与评测复盘用）。
    pub fn query_log_get(&self, id: &str) -> Result<Option<QueryLog>, AppError> {
        let conn = self.conn()?;
        conn.query_row(
            &format!("SELECT {COLUMNS} FROM queries WHERE id = ?1"),
            [id],
            row_to_query_log,
        )
        .optional()
        .map_err(db_error)
    }

    /// 记录总数（purge 后应为 0）。
    pub fn query_log_count(&self) -> Result<u64, AppError> {
        let conn = self.conn()?;
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM queries", [], |row| row.get(0))
            .map_err(db_error)?;
        Ok(count as u64)
    }
}

fn row_to_query_log(row: &Row<'_>) -> rusqlite::Result<QueryLog> {
    Ok(QueryLog {
        id: row.get(0)?,
        raw: row.get(1)?,
        parsed_json: row.get(2)?,
        result_ids_json: row.get(3)?,
        feedback: row.get(4)?,
        created_at: row.get(5)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_library() -> (tempfile::TempDir, Library) {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::open(dir.path().to_path_buf()).unwrap();
        (dir, lib)
    }

    fn sample() -> QueryLog {
        QueryLog {
            id: "q-1".into(),
            raw: "上个月的橙色漏斗图".into(),
            parsed_json: Some(r#"{"color":["orange"]}"#.into()),
            result_ids_json: Some(r#"["a","b"]"#.into()),
            feedback: None,
            created_at: 1,
        }
    }

    #[test]
    fn insert_and_read_back() {
        let (_dir, lib) = temp_library();
        lib.query_log_insert(&sample()).unwrap();
        let loaded = lib.query_log_get("q-1").unwrap().unwrap();
        assert_eq!(loaded, sample());
        assert_eq!(lib.query_log_count().unwrap(), 1);
    }

    #[test]
    fn feedback_update_requires_existing_query() {
        let (_dir, lib) = temp_library();
        lib.query_log_insert(&sample()).unwrap();
        lib.query_log_set_feedback("q-1", "not-this").unwrap();
        assert_eq!(
            lib.query_log_get("q-1")
                .unwrap()
                .unwrap()
                .feedback
                .as_deref(),
            Some("not-this")
        );

        assert!(matches!(
            lib.query_log_set_feedback("missing", "not-this"),
            Err(AppError::NotFound)
        ));
    }
}
