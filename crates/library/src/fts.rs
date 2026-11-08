//! `items_fts` 的写入、删除与检索（contentful FTS5 表，design §4.1 / §1.2 澄清）。
//!
//! 分工：分词在 `crates/search`（jieba，写入侧与查询侧必须同源），本模块只负责
//! 「已分词的列」→ SQL。这样 `library` 不依赖 jieba，索引进程与检索进程共用同一张表。
//!
//! `contentful`（默认）表保留原始列内容，因此 `snippet()`/`highlight()` 可用；代价是
//! 删除必须显式 `DELETE FROM items_fts WHERE rowid=?`（FTS5 的 `'delete'` 指令只适用于
//! contentless/external-content 表）。

use rusqlite::{params, Transaction};
use visrecall_domain::AppError;

use crate::db::{db_error, Library};

/// 一行 FTS 内容（**必须已分词**：token 之间以空格分隔）。
#[derive(Debug, Clone, Copy, Default)]
pub struct FtsColumns<'a> {
    pub title: &'a str,
    pub summary: &'a str,
    pub ocr_text: &'a str,
    pub note: &'a str,
    pub tags: &'a str,
    pub entities: &'a str,
    pub original_text: &'a str,
}

/// 一条 FTS 命中。
#[derive(Debug, Clone, PartialEq)]
pub struct FtsHit {
    /// `items.rowid`。
    pub rowid: i64,
    /// `snippet()` 生成的命中摘要（含 `«»` 标记）。
    pub snippet: String,
    /// BM25 相关度，**已转成正数**（越大越相关；SQLite 原生返回负值）。
    pub score: f32,
}

/// 候选集下推阈值：候选超过该数量时不在 SQL 里拼 `IN (...)`，由调用方在融合后过滤。
pub const CANDIDATE_INLINE_LIMIT: usize = 500;

/// 写入（覆盖）一行：FTS5 不支持 UPSERT，先删后插，同一事务内原子。
pub fn fts_upsert_tx(
    tx: &Transaction<'_>,
    rowid: i64,
    cols: &FtsColumns<'_>,
) -> Result<(), AppError> {
    tx.execute("DELETE FROM items_fts WHERE rowid = ?1", [rowid])
        .map_err(db_error)?;
    tx.execute(
        "INSERT INTO items_fts
           (rowid, title, summary, ocr_text, note, tags, entities, original_text)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            rowid,
            cols.title,
            cols.summary,
            cols.ocr_text,
            cols.note,
            cols.tags,
            cols.entities,
            cols.original_text
        ],
    )
    .map_err(db_error)?;
    Ok(())
}

/// 删除一行（contentful 表用普通 DELETE）。
pub fn fts_delete_tx(tx: &Transaction<'_>, rowid: i64) -> Result<(), AppError> {
    tx.execute("DELETE FROM items_fts WHERE rowid = ?1", [rowid])
        .map_err(db_error)?;
    Ok(())
}

impl Library {
    /// 写入（覆盖）一行 FTS 内容，自带事务。
    pub fn fts_upsert(&self, rowid: i64, cols: &FtsColumns<'_>) -> Result<(), AppError> {
        let mut conn = self.conn()?;
        let tx = conn.transaction().map_err(db_error)?;
        fts_upsert_tx(&tx, rowid, cols)?;
        tx.commit().map_err(db_error)
    }

    /// 删除一行 FTS 内容。
    pub fn fts_delete(&self, rowid: i64) -> Result<(), AppError> {
        let mut conn = self.conn()?;
        let tx = conn.transaction().map_err(db_error)?;
        fts_delete_tx(&tx, rowid)?;
        tx.commit().map_err(db_error)
    }

    /// `items_fts` 行数（启动自检用）。
    pub fn fts_count(&self) -> Result<u64, AppError> {
        let conn = self.conn()?;
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM items_fts", [], |row| row.get(0))
            .map_err(db_error)?;
        Ok(count as u64)
    }

    /// BM25 检索。`expr` 为空返回空结果（调用方据此跳过字面通道）。
    ///
    /// `candidates` 为 `Some` 时下推 `rowid IN (...)`，但仅在候选数 ≤
    /// [`CANDIDATE_INLINE_LIMIT`] 时生效；更大的候选集由调用方在融合后过滤
    /// （20 条规模恒走 `IN` 分支，登记为实现简化）。
    pub fn fts_search(
        &self,
        expr: &str,
        candidates: Option<&[i64]>,
        limit: u32,
    ) -> Result<Vec<FtsHit>, AppError> {
        if expr.trim().is_empty() {
            return Ok(Vec::new());
        }
        let limit = limit.clamp(1, 1000);
        let conn = self.conn()?;

        let mut sql = String::from(
            "SELECT rowid, snippet(items_fts, -1, '«', '»', '…', 24), bm25(items_fts)
             FROM items_fts WHERE items_fts MATCH ?1",
        );
        let inline: Option<&[i64]> = candidates.filter(|c| c.len() <= CANDIDATE_INLINE_LIMIT);
        if let Some(ids) = inline {
            if ids.is_empty() {
                return Ok(Vec::new());
            }
            let placeholders = vec!["?"; ids.len()].join(", ");
            sql.push_str(&format!(" AND rowid IN ({placeholders})"));
        }
        sql.push_str(" ORDER BY rank LIMIT ");
        sql.push_str(&limit.to_string());

        let mut stmt = conn.prepare(&sql).map_err(db_error)?;
        let mut values: Vec<rusqlite::types::Value> =
            vec![rusqlite::types::Value::Text(expr.to_string())];
        if let Some(ids) = inline {
            values.extend(ids.iter().map(|id| rusqlite::types::Value::Integer(*id)));
        }

        let rows = stmt
            .query_map(rusqlite::params_from_iter(values), |row| {
                let rowid: i64 = row.get(0)?;
                let snippet: String = row.get(1)?;
                let bm25: f64 = row.get(2)?;
                Ok(FtsHit {
                    rowid,
                    snippet,
                    #[allow(clippy::cast_possible_truncation)]
                    score: (-bm25) as f32,
                })
            })
            .map_err(db_error)?;

        let mut hits = Vec::new();
        for row in rows {
            hits.push(row.map_err(db_error)?);
        }
        Ok(hits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Library;

    fn temp_library() -> (tempfile::TempDir, Library) {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::open(dir.path().to_path_buf()).unwrap();
        (dir, lib)
    }

    fn insert_item(lib: &Library, title: &str) -> i64 {
        let item = lib
            .insert_item(visrecall_domain::NewItem {
                kind: visrecall_domain::ItemKind::Image,
                captured_at: 1,
                source_app: None,
                source_title: Some(title.to_string()),
                source_url: None,
                width: None,
                height: None,
                char_count: None,
                original_path: "originals/images/a.png".into(),
                content_hash: format!("hash-{title}"),
            })
            .unwrap();
        lib.item_rowid(&item.id).unwrap().unwrap()
    }

    #[test]
    fn empty_expr_returns_no_hits() {
        let (_dir, lib) = temp_library();
        assert!(lib.fts_search("", None, 10).unwrap().is_empty());
        assert!(lib.fts_search("   ", None, 10).unwrap().is_empty());
    }

    #[test]
    fn upsert_overwrites_without_duplicating_rows() {
        let (_dir, lib) = temp_library();
        let rowid = insert_item(&lib, "标题");
        let cols = FtsColumns {
            title: "发票 报销",
            ..Default::default()
        };
        lib.fts_upsert(rowid, &cols).unwrap();
        assert_eq!(lib.fts_count().unwrap(), 1);

        let cols2 = FtsColumns {
            title: "发票 明细",
            ..Default::default()
        };
        lib.fts_upsert(rowid, &cols2).unwrap();
        assert_eq!(lib.fts_count().unwrap(), 1);
        let hits = lib.fts_search("\"明细\"", None, 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].rowid, rowid);
    }

    #[test]
    fn empty_candidate_list_short_circuits() {
        let (_dir, lib) = temp_library();
        let rowid = insert_item(&lib, "标题");
        lib.fts_upsert(
            rowid,
            &FtsColumns {
                title: "发票",
                ..Default::default()
            },
        )
        .unwrap();
        assert!(lib
            .fts_search("\"发票\"", Some(&[]), 10)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn candidate_filter_limits_results() {
        let (_dir, lib) = temp_library();
        let a = insert_item(&lib, "a");
        let b = insert_item(&lib, "b");
        for rowid in [a, b] {
            lib.fts_upsert(
                rowid,
                &FtsColumns {
                    title: "发票",
                    ..Default::default()
                },
            )
            .unwrap();
        }
        let hits = lib.fts_search("\"发票\"", Some(&[b]), 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].rowid, b);
    }

    #[test]
    fn bm25_score_is_positive_and_snippet_marked() {
        let (_dir, lib) = temp_library();
        let rowid = insert_item(&lib, "报告");
        lib.fts_upsert(
            rowid,
            &FtsColumns {
                title: "季度 营收 漏斗图",
                ..Default::default()
            },
        )
        .unwrap();
        let hits = lib.fts_search("\"漏斗图\"", None, 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].score > 0.0, "score={}", hits[0].score);
        assert!(hits[0].snippet.contains('«'));
    }
}
