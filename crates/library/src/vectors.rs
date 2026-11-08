//! 向量表读写与「蓝绿重建」指针（design §4.1、§6.4）。
//!
//! V0.1 的主路径是**暴力 int8 余弦**：表里只存量化后的字节，相似度比较在
//! `crates/library::vector_index` 里做（Rust 侧逐条点积）。本模块只管表结构与指针：
//!
//! - `vector.active_table` 指向当前活跃表，默认 `item_vectors_int8`；
//! - 蓝绿重建时建 `item_vectors_int8_v2` 等同构表，全量写入后原子切指针，旧表留作回滚；
//! - 检索全程读指针，切换是单事务，零中断。
//!
//! 表名会拼进 SQL（SQLite 不支持参数化表名），因此只接受白名单形态（见
//! [`is_valid_vector_table`]），避免注入。

use rusqlite::{params, OptionalExtension, Transaction};
use visrecall_domain::AppError;

use crate::db::{db_error, Library};

/// 默认向量表名。
pub const DEFAULT_VECTOR_TABLE: &str = "item_vectors_int8";
/// `settings` 键：当前活跃向量表。
pub const ACTIVE_TABLE_KEY: &str = "vector.active_table";
/// `settings` 键：嵌入模型 id。
pub const MODEL_ID_KEY: &str = "embedding.model_id";
/// `settings` 键：向量维度。
pub const DIM_KEY: &str = "embedding.dim";

/// 一行向量。
#[derive(Debug, Clone, PartialEq)]
pub struct VectorRow {
    pub rowid: i64,
    /// int8 量化后的向量字节（长度 == `dim`）。
    pub embedding: Vec<u8>,
    pub dim: u32,
    pub model_id: String,
}

/// 只允许 `item_vectors_int8` 或 `item_vectors_int8_v<N>`。
pub fn is_valid_vector_table(name: &str) -> bool {
    if name == DEFAULT_VECTOR_TABLE {
        return true;
    }
    let Some(suffix) = name.strip_prefix("item_vectors_int8_v") else {
        return false;
    };
    !suffix.is_empty() && suffix.len() <= 4 && suffix.chars().all(|c| c.is_ascii_digit())
}

pub fn vector_upsert_tx(
    tx: &Transaction<'_>,
    table: &str,
    rowid: i64,
    vec8: &[u8],
    dim: u32,
    model_id: &str,
) -> Result<(), AppError> {
    if !is_valid_vector_table(table) {
        return Err(AppError::InvalidInput(format!(
            "invalid vector table: {table}"
        )));
    }
    tx.execute(
        &format!("DELETE FROM {table} WHERE item_rowid = ?1"),
        [rowid],
    )
    .map_err(db_error)?;
    tx.execute(
        &format!(
            "INSERT INTO {table} (item_rowid, embedding, dim, model_id) VALUES (?1, ?2, ?3, ?4)"
        ),
        params![rowid, vec8, dim, model_id],
    )
    .map_err(db_error)?;
    Ok(())
}

pub fn vector_delete_tx(tx: &Transaction<'_>, table: &str, rowid: i64) -> Result<(), AppError> {
    if !is_valid_vector_table(table) {
        return Err(AppError::InvalidInput(format!(
            "invalid vector table: {table}"
        )));
    }
    tx.execute(
        &format!("DELETE FROM {table} WHERE item_rowid = ?1"),
        [rowid],
    )
    .map_err(db_error)?;
    Ok(())
}

impl Library {
    /// 当前活跃向量表（`settings` 缺失或非法时回落到默认表并记 `warn`）。
    pub fn active_vector_table(&self) -> String {
        match self.setting_get::<String>(ACTIVE_TABLE_KEY) {
            Some(name) if is_valid_vector_table(&name) => name,
            Some(name) => {
                tracing::warn!(table = %name, "invalid vector.active_table setting; using default");
                DEFAULT_VECTOR_TABLE.to_string()
            }
            None => DEFAULT_VECTOR_TABLE.to_string(),
        }
    }

    /// 原子切换活跃表（单事务改 settings）。调用方负责先确认新表行数正确。
    pub fn vector_switch_active(&self, table: &str) -> Result<(), AppError> {
        if !is_valid_vector_table(table) {
            return Err(AppError::InvalidInput(format!(
                "invalid vector table: {table}"
            )));
        }
        let mut conn = self.conn()?;
        let tx = conn.transaction().map_err(db_error)?;
        let json = serde_json::to_string(table)
            .map_err(|e| AppError::Internal(format!("serialize active table: {e}")))?;
        tx.execute(
            "INSERT OR REPLACE INTO settings (key, value_json) VALUES (?1, ?2)",
            params![ACTIVE_TABLE_KEY, json],
        )
        .map_err(db_error)?;
        tx.commit().map_err(db_error)
    }

    /// 建一张同构向量表（蓝绿重建用；已存在时先 DROP，避免残留旧数据）。
    pub fn create_vector_table(&self, table: &str) -> Result<(), AppError> {
        if !is_valid_vector_table(table) || table == DEFAULT_VECTOR_TABLE {
            return Err(AppError::InvalidInput(format!(
                "invalid vector table for rebuild: {table}"
            )));
        }
        let conn = self.conn()?;
        conn.execute_batch(&format!(
            "DROP TABLE IF EXISTS {table};
             CREATE TABLE {table} (
               item_rowid INTEGER PRIMARY KEY REFERENCES items(rowid) ON DELETE CASCADE,
               embedding  BLOB NOT NULL,
               dim        INTEGER NOT NULL,
               model_id   TEXT NOT NULL
             );"
        ))
        .map_err(db_error)?;
        Ok(())
    }

    /// 删除向量表（重建失败清理用；拒绝删默认表）。
    pub fn drop_vector_table(&self, table: &str) -> Result<(), AppError> {
        if !is_valid_vector_table(table) || table == DEFAULT_VECTOR_TABLE {
            return Err(AppError::InvalidInput(format!(
                "refusing to drop vector table: {table}"
            )));
        }
        let conn = self.conn()?;
        conn.execute_batch(&format!("DROP TABLE IF EXISTS {table};"))
            .map_err(db_error)?;
        Ok(())
    }

    /// 写入（覆盖）一条向量，自带事务。
    pub fn vector_upsert(
        &self,
        rowid: i64,
        vec8: &[u8],
        dim: u32,
        model_id: &str,
    ) -> Result<(), AppError> {
        let table = self.active_vector_table();
        let mut conn = self.conn()?;
        let tx = conn.transaction().map_err(db_error)?;
        vector_upsert_tx(&tx, &table, rowid, vec8, dim, model_id)?;
        tx.commit().map_err(db_error)
    }

    /// 删除一条向量（活跃表）。
    pub fn vector_delete(&self, rowid: i64) -> Result<(), AppError> {
        let table = self.active_vector_table();
        let mut conn = self.conn()?;
        let tx = conn.transaction().map_err(db_error)?;
        vector_delete_tx(&tx, &table, rowid)?;
        tx.commit().map_err(db_error)
    }

    /// 活跃表行数（启动自检用）。
    pub fn vector_count(&self) -> Result<u64, AppError> {
        let table = self.active_vector_table();
        self.vector_count_for(&table)
    }

    /// 指定表行数。
    pub fn vector_count_for(&self, table: &str) -> Result<u64, AppError> {
        if !is_valid_vector_table(table) {
            return Err(AppError::InvalidInput(format!(
                "invalid vector table: {table}"
            )));
        }
        let conn = self.conn()?;
        let count: i64 = conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .map_err(db_error)?;
        Ok(count as u64)
    }

    /// 读出活跃表的全部向量（暴力余弦用；20 条规模够用，规模上来换 ANN）。
    pub fn vector_rows(&self, candidates: Option<&[i64]>) -> Result<Vec<VectorRow>, AppError> {
        let table = self.active_vector_table();
        let conn = self.conn()?;
        let (sql, values): (String, Vec<rusqlite::types::Value>) = match candidates {
            Some(ids) if !ids.is_empty() => {
                let placeholders = vec!["?"; ids.len()].join(", ");
                (
                    format!(
                        "SELECT item_rowid, embedding, dim, model_id FROM {table} WHERE item_rowid IN ({placeholders})"
                    ),
                    ids.iter()
                        .map(|id| rusqlite::types::Value::Integer(*id))
                        .collect(),
                )
            }
            _ => (
                format!("SELECT item_rowid, embedding, dim, model_id FROM {table}"),
                Vec::new(),
            ),
        };
        let mut stmt = conn.prepare(&sql).map_err(db_error)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(values), |row| {
                Ok(VectorRow {
                    rowid: row.get(0)?,
                    embedding: row.get(1)?,
                    dim: row.get::<_, i64>(2)? as u32,
                    model_id: row.get(3)?,
                })
            })
            .map_err(db_error)?;

        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(db_error)?);
        }
        Ok(out)
    }

    /// 当前库固定的嵌入模型空间 `(model_id, dim)`；未写入过返回 `None`。
    pub fn embedding_space(&self) -> Option<(String, u32)> {
        let model_id = self.setting_get::<String>(MODEL_ID_KEY)?;
        let dim = self.setting_get::<u32>(DIM_KEY)?;
        Some((model_id, dim))
    }

    /// 首次写入嵌入模型空间；已写入且不一致时返回错误（必须走蓝绿重建，P3-R12）。
    pub fn init_embedding_space(&self, model_id: &str, dim: u32) -> Result<(), AppError> {
        match self.embedding_space() {
            None => {
                self.setting_set(MODEL_ID_KEY, &model_id.to_string())?;
                self.setting_set(DIM_KEY, &dim)?;
                tracing::info!(model_id, dim, "embedding space pinned for this library");
                Ok(())
            }
            Some((existing_id, existing_dim)) if existing_id == model_id && existing_dim == dim => {
                Ok(())
            }
            Some((existing_id, existing_dim)) => Err(AppError::InvalidInput(format!(
                "embedding space mismatch: library uses {existing_id}/{existing_dim}, \
                 provider is {model_id}/{dim}; run blue-green rebuild"
            ))),
        }
    }

    /// 蓝绿重建完成后强制切换嵌入模型空间（调用方已在同一决策里完成全量重嵌入）。
    pub fn force_embedding_space(&self, model_id: &str, dim: u32) -> Result<(), AppError> {
        self.setting_set(MODEL_ID_KEY, &model_id.to_string())?;
        self.setting_set(DIM_KEY, &dim)?;
        Ok(())
    }

    /// 该 `content_hash` 是否已有可复用的解析结果（insight 复用，P3-R12）。
    ///
    /// 只复用「非 pending」的结果：断网时生成的 OCR 占位摘要不该被当成完整解析。
    pub fn insight_by_content_hash(
        &self,
        content_hash: &str,
        exclude_item_id: &str,
    ) -> Result<Option<visrecall_domain::Insight>, AppError> {
        // 注意：连接是单连接 `Mutex`，必须先把 guard 释放出去，再调用会再次取锁的
        // `insight_get`，否则同一线程自锁。
        let item_id: Option<String> = {
            let conn = self.conn()?;
            conn.query_row(
                "SELECT i.id FROM items i
                 JOIN insights ins ON ins.item_id = i.id
                 WHERE i.content_hash = ?1 AND i.id <> ?2 AND i.status <> 'pending'
                 ORDER BY i.rowid ASC LIMIT 1",
                params![content_hash, exclude_item_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_error)?
        };
        match item_id {
            Some(id) => self.insight_get(&id),
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use visrecall_domain::{Insight, ItemKind, NewItem};

    fn temp_library() -> (tempfile::TempDir, Library) {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::open(dir.path().to_path_buf()).unwrap();
        (dir, lib)
    }

    fn insert_item(lib: &Library, hash: &str) -> (String, i64) {
        let item = lib
            .insert_item(NewItem {
                kind: ItemKind::Image,
                captured_at: 1,
                source_app: None,
                source_title: None,
                source_url: None,
                width: None,
                height: None,
                char_count: None,
                original_path: "originals/images/a.png".into(),
                content_hash: hash.into(),
            })
            .unwrap();
        let rowid = lib.item_rowid(&item.id).unwrap().unwrap();
        (item.id, rowid)
    }

    #[test]
    fn table_name_whitelist() {
        assert!(is_valid_vector_table("item_vectors_int8"));
        assert!(is_valid_vector_table("item_vectors_int8_v2"));
        assert!(!is_valid_vector_table("item_vectors_int8_v"));
        assert!(!is_valid_vector_table("items; DROP TABLE items"));
        assert!(!is_valid_vector_table("item_vectors_int8_v12345"));
    }

    #[test]
    fn default_active_table_when_setting_missing() {
        let (_dir, lib) = temp_library();
        assert_eq!(lib.active_vector_table(), DEFAULT_VECTOR_TABLE);
    }

    #[test]
    fn invalid_active_table_setting_falls_back() {
        let (_dir, lib) = temp_library();
        lib.setting_set(ACTIVE_TABLE_KEY, &"evil; DROP TABLE items".to_string())
            .unwrap();
        assert_eq!(lib.active_vector_table(), DEFAULT_VECTOR_TABLE);
    }

    #[test]
    fn blue_green_switch_keeps_old_table() {
        let (_dir, lib) = temp_library();
        let (_, rowid) = insert_item(&lib, "hash");
        let vec8: Vec<u8> = vec![1, 2, 3];
        lib.vector_upsert(rowid, &vec8, 3, "model-a").unwrap();
        assert_eq!(lib.vector_count().unwrap(), 1);

        lib.create_vector_table("item_vectors_int8_v2").unwrap();
        {
            let mut conn = lib.conn().unwrap();
            let tx = conn.transaction().unwrap();
            vector_upsert_tx(&tx, "item_vectors_int8_v2", rowid, &vec8, 3, "model-a").unwrap();
            tx.commit().unwrap();
        }
        assert_eq!(lib.vector_count_for("item_vectors_int8_v2").unwrap(), 1);

        lib.vector_switch_active("item_vectors_int8_v2").unwrap();
        assert_eq!(lib.active_vector_table(), "item_vectors_int8_v2");
        // 旧表数据仍在，可回滚
        assert_eq!(lib.vector_count_for(DEFAULT_VECTOR_TABLE).unwrap(), 1);
    }

    #[test]
    fn deleting_item_cascades_vector_rows() {
        let (_dir, lib) = temp_library();
        let (id, rowid) = insert_item(&lib, "hash");
        lib.vector_upsert(rowid, &[9, 9], 2, "model-a").unwrap();
        lib.delete_item(&id).unwrap();
        assert_eq!(lib.vector_count().unwrap(), 0);
    }

    #[test]
    fn embedding_space_is_pinned_on_first_write() {
        let (_dir, lib) = temp_library();
        assert!(lib.embedding_space().is_none());
        lib.init_embedding_space("bge-small-zh-v1.5-int8", 512)
            .unwrap();
        assert_eq!(
            lib.embedding_space(),
            Some(("bge-small-zh-v1.5-int8".to_string(), 512))
        );
        // 同空间重复调用幂等
        lib.init_embedding_space("bge-small-zh-v1.5-int8", 512)
            .unwrap();
        // 换空间必须报错（走蓝绿重建）
        assert!(lib
            .init_embedding_space("bge-base-zh-v1.5-int8", 768)
            .is_err());

        lib.force_embedding_space("bge-base-zh-v1.5-int8", 768)
            .unwrap();
        assert_eq!(
            lib.embedding_space(),
            Some(("bge-base-zh-v1.5-int8".to_string(), 768))
        );
    }

    #[test]
    fn insight_reuse_skips_pending_items() {
        let (_dir, lib) = temp_library();
        let (id_a, _) = insert_item(&lib, "same-hash");
        let mut insight = Insight::empty(&id_a, 1);
        insight.summary = Some("摘要".into());
        lib.insight_upsert(&insight).unwrap();

        let (id_b, _) = insert_item(&lib, "same-hash");
        let reused = lib.insight_by_content_hash("same-hash", &id_b).unwrap();
        assert_eq!(reused.map(|i| i.item_id), Some(id_a.clone()));

        // 把来源素材置为 pending 后不再复用
        lib.update_status(&id_a, visrecall_domain::ItemStatus::Pending, None)
            .unwrap();
        assert!(lib
            .insight_by_content_hash("same-hash", &id_b)
            .unwrap()
            .is_none());
    }
}
