//! 目录来源与外部资产映射的持久化。

use rusqlite::{OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use visrecall_domain::item::{Item, NewItem};
use visrecall_domain::{new_item_id, now_millis, AppError};

use crate::db::{db_error, Library};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceStatus {
    #[default]
    Available,
    Offline,
    Removed,
}

impl SourceStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Offline => "offline",
            Self::Removed => "removed",
        }
    }

    fn parse(raw: &str) -> Self {
        match raw {
            "offline" => Self::Offline,
            "removed" => Self::Removed,
            _ => Self::Available,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportSource {
    pub id: String,
    pub root_path: String,
    pub path_key: String,
    pub include_subdirectories: bool,
    pub include_images: bool,
    pub include_text: bool,
    pub ai_allowed: bool,
    pub status: SourceStatus,
    pub last_scanned_at: Option<i64>,
    pub item_count: u64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug)]
pub struct NewImportSource {
    pub root_path: String,
    pub include_subdirectories: bool,
    pub include_images: bool,
    pub include_text: bool,
    pub ai_allowed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalAsset {
    pub item_id: String,
    pub source_id: String,
    pub relative_path: String,
    pub size: u64,
    pub modified_at: i64,
    pub indexed_hash: Option<String>,
    pub indexed_at: Option<i64>,
    pub source_status: String,
}

/// 统一 Windows 路径身份比较；其他平台保留大小写。
pub fn path_identity(raw: &str) -> String {
    let normalized = raw.replace('/', "\\");
    #[cfg(windows)]
    {
        normalized.to_lowercase()
    }
    #[cfg(not(windows))]
    {
        normalized
    }
}

impl Library {
    pub fn create_import_source(&self, new: NewImportSource) -> Result<ImportSource, AppError> {
        let id = new_item_id();
        let now = now_millis();
        let key = path_identity(&new.root_path);
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO import_sources
               (id, root_path, path_key, include_subdirectories, include_images,
                include_text, ai_allowed, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)
             ON CONFLICT(path_key) DO UPDATE SET
               root_path = excluded.root_path,
               include_subdirectories = excluded.include_subdirectories,
               include_images = excluded.include_images,
               include_text = excluded.include_text,
               ai_allowed = excluded.ai_allowed,
               status = 'available', updated_at = excluded.updated_at",
            rusqlite::params![
                id,
                new.root_path,
                key,
                new.include_subdirectories,
                new.include_images,
                new.include_text,
                new.ai_allowed,
                now,
            ],
        )
        .map_err(db_error)?;
        drop(conn);
        self.find_import_source_by_path(&new.root_path)?
            .ok_or(AppError::NotFound)
    }

    pub fn get_import_source(&self, id: &str) -> Result<Option<ImportSource>, AppError> {
        let conn = self.conn()?;
        source_query(&conn, "WHERE s.id = ?1", rusqlite::params![id])
    }

    pub fn find_import_source_by_path(&self, root: &str) -> Result<Option<ImportSource>, AppError> {
        let conn = self.conn()?;
        source_query(
            &conn,
            "WHERE s.path_key = ?1 AND s.status != 'removed'",
            rusqlite::params![path_identity(root)],
        )
    }

    pub fn list_import_sources(&self) -> Result<Vec<ImportSource>, AppError> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(&format!(
                "{SOURCE_SELECT} WHERE s.status != 'removed' ORDER BY s.created_at DESC"
            ))
            .map_err(db_error)?;
        let rows = stmt.query_map([], source_from_row).map_err(db_error)?;
        rows.map(|row| row.map_err(db_error)).collect()
    }

    pub fn update_import_source_root(&self, id: &str, root: &str) -> Result<(), AppError> {
        let conn = self.conn()?;
        let changed = conn
            .execute(
                "UPDATE import_sources SET root_path = ?2, path_key = ?3,
                   status = 'available', updated_at = ?4 WHERE id = ?1",
                rusqlite::params![id, root, path_identity(root), now_millis()],
            )
            .map_err(db_error)?;
        if changed == 0 {
            return Err(AppError::NotFound);
        }
        Ok(())
    }

    pub fn set_import_source_status(&self, id: &str, status: SourceStatus) -> Result<(), AppError> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE import_sources SET status = ?2, updated_at = ?3 WHERE id = ?1",
            rusqlite::params![id, status.as_str(), now_millis()],
        )
        .map_err(db_error)?;
        Ok(())
    }

    pub fn mark_import_source_scanned(&self, id: &str) -> Result<(), AppError> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE import_sources SET last_scanned_at = ?2, status = 'available',
               updated_at = ?2 WHERE id = ?1",
            rusqlite::params![id, now_millis()],
        )
        .map_err(db_error)?;
        Ok(())
    }

    pub fn external_asset_for_item(
        &self,
        item_id: &str,
    ) -> Result<Option<ExternalAsset>, AppError> {
        let conn = self.conn()?;
        conn.query_row(
            "SELECT item_id, source_id, relative_path, size, modified_at,
                    indexed_hash, indexed_at, source_status
             FROM external_assets WHERE item_id = ?1",
            [item_id],
            |row| {
                Ok(ExternalAsset {
                    item_id: row.get(0)?,
                    source_id: row.get(1)?,
                    relative_path: row.get(2)?,
                    size: row.get::<_, i64>(3)? as u64,
                    modified_at: row.get(4)?,
                    indexed_hash: row.get(5)?,
                    indexed_at: row.get(6)?,
                    source_status: row.get(7)?,
                })
            },
        )
        .optional()
        .map_err(db_error)
    }

    pub fn list_external_assets(&self, source_id: &str) -> Result<Vec<ExternalAsset>, AppError> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT item_id, source_id, relative_path, size, modified_at,
                    indexed_hash, indexed_at, source_status
             FROM external_assets WHERE source_id = ?1 ORDER BY path_key",
            )
            .map_err(db_error)?;
        let rows = stmt
            .query_map([source_id], external_asset_from_row)
            .map_err(db_error)?;
        rows.map(|row| row.map_err(db_error)).collect()
    }

    pub fn external_asset_at_path(
        &self,
        source_id: &str,
        relative_path: &str,
    ) -> Result<Option<ExternalAsset>, AppError> {
        let conn = self.conn()?;
        conn.query_row(
            "SELECT item_id, source_id, relative_path, size, modified_at,
                    indexed_hash, indexed_at, source_status
             FROM external_assets WHERE source_id = ?1 AND path_key = ?2",
            rusqlite::params![source_id, path_identity(relative_path)],
            external_asset_from_row,
        )
        .optional()
        .map_err(db_error)
    }

    pub fn source_path_excluded(
        &self,
        source_id: &str,
        relative_path: &str,
    ) -> Result<bool, AppError> {
        let conn = self.conn()?;
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM source_exclusions WHERE source_id = ?1 AND path_key = ?2)",
            rusqlite::params![source_id, path_identity(relative_path)],
            |row| row.get(0),
        )
        .map_err(db_error)
    }

    /// 引用内容变化时保留素材 ID、备注和标签，只替换内容版本及派生数据输入。
    pub fn update_reference_item(
        &self,
        item_id: &str,
        size: u64,
        modified_at: i64,
        new: &NewItem,
    ) -> Result<Item, AppError> {
        let now = now_millis();
        let mut conn = self.conn()?;
        let tx = conn.transaction().map_err(db_error)?;
        let changed = tx
            .execute(
                "UPDATE items SET status = 'parsing', source_title = ?2, width = ?3,
               height = ?4, char_count = ?5, content_hash = ?6,
               revision = revision + 1, updated_at = ?7 WHERE id = ?1",
                rusqlite::params![
                    item_id,
                    new.source_title,
                    new.width.map(i64::from),
                    new.height.map(i64::from),
                    new.char_count.map(i64::from),
                    new.content_hash,
                    now
                ],
            )
            .map_err(db_error)?;
        if changed == 0 {
            return Err(AppError::NotFound);
        }
        tx.execute(
            "UPDATE external_assets SET size = ?2, modified_at = ?3, indexed_hash = ?4,
               indexed_at = ?5,
               source_status = 'available' WHERE item_id = ?1",
            rusqlite::params![
                item_id,
                size as i64,
                modified_at,
                new.content_hash,
                now_millis()
            ],
        )
        .map_err(db_error)?;
        tx.commit().map_err(db_error)?;
        drop(conn);
        self.get_item(item_id)?.ok_or(AppError::NotFound)
    }

    pub fn set_external_asset_status(&self, item_id: &str, status: &str) -> Result<(), AppError> {
        if !matches!(
            status,
            "available" | "missing" | "permission_denied" | "changed"
        ) {
            return Err(AppError::InvalidInput("非法外部资产状态".into()));
        }
        let conn = self.conn()?;
        conn.execute(
            "UPDATE external_assets SET source_status = ?2 WHERE item_id = ?1",
            rusqlite::params![item_id, status],
        )
        .map_err(db_error)?;
        Ok(())
    }

    /// 引用来源必须单独授权云端 AI；托管素材沿用现有模型设置。
    pub fn cloud_ai_allowed(&self, item_id: &str) -> Result<bool, AppError> {
        let Some(asset) = self.external_asset_for_item(item_id)? else {
            return Ok(true);
        };
        Ok(self
            .get_import_source(&asset.source_id)?
            .is_some_and(|source| source.ai_allowed && source.status == SourceStatus::Available))
    }

    /// 同事务创建引用素材与路径映射，避免出现无法定位的半成品。
    pub fn insert_reference_item(
        &self,
        source_id: &str,
        relative_path: &str,
        size: u64,
        modified_at: i64,
        new: NewItem,
    ) -> Result<Item, AppError> {
        let id = new_item_id();
        let now = now_millis();
        let mut conn = self.conn()?;
        let tx = conn.transaction().map_err(db_error)?;
        tx.execute(
            "INSERT INTO items
               (id, kind, status, captured_at, source_app, source_title, source_url,
                width, height, char_count, original_path, note, content_hash,
                storage_mode, created_at, updated_at)
             VALUES (?1, ?2, 'parsing', ?3, 'VisRecall', ?4, NULL, ?5, ?6, ?7,
                     '', '', ?8, 'reference', ?9, ?9)",
            rusqlite::params![
                id,
                new.kind.as_str(),
                new.captured_at,
                new.source_title,
                new.width.map(i64::from),
                new.height.map(i64::from),
                new.char_count.map(i64::from),
                new.content_hash,
                now,
            ],
        )
        .map_err(db_error)?;
        tx.execute(
            "INSERT INTO external_assets
               (item_id, source_id, relative_path, path_key, size, modified_at, indexed_hash, indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                id,
                source_id,
                relative_path,
                path_identity(relative_path),
                size as i64,
                modified_at,
                new.content_hash,
                now,
            ],
        )
        .map_err(db_error)?;
        tx.commit().map_err(db_error)?;
        drop(conn);
        self.get_item(&id)?.ok_or(AppError::NotFound)
    }

    pub fn exclude_source_path(
        &self,
        source_id: &str,
        relative_path: &str,
    ) -> Result<(), AppError> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT OR REPLACE INTO source_exclusions(source_id, relative_path, path_key, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![source_id, relative_path, path_identity(relative_path), now_millis()],
        )
        .map_err(db_error)?;
        Ok(())
    }

    /// 移除来源时可保留条目但停用定位，或仅删除数据库中的关联素材。
    pub fn remove_import_source(&self, id: &str, remove_items: bool) -> Result<u64, AppError> {
        let mut conn = self.conn()?;
        let tx = conn.transaction().map_err(db_error)?;
        let mut removed = 0_u64;
        if remove_items {
            removed = tx
                .execute(
                    "DELETE FROM items WHERE id IN
                       (SELECT item_id FROM external_assets WHERE source_id = ?1)",
                    [id],
                )
                .map_err(db_error)? as u64;
            tx.execute("DELETE FROM source_exclusions WHERE source_id = ?1", [id])
                .map_err(db_error)?;
        }
        tx.execute(
            "UPDATE import_sources SET status = 'removed', updated_at = ?2 WHERE id = ?1",
            rusqlite::params![id, now_millis()],
        )
        .map_err(db_error)?;
        tx.commit().map_err(db_error)?;
        Ok(removed)
    }
}

fn external_asset_from_row(row: &Row<'_>) -> rusqlite::Result<ExternalAsset> {
    Ok(ExternalAsset {
        item_id: row.get(0)?,
        source_id: row.get(1)?,
        relative_path: row.get(2)?,
        size: row.get::<_, i64>(3)? as u64,
        modified_at: row.get(4)?,
        indexed_hash: row.get(5)?,
        indexed_at: row.get(6)?,
        source_status: row.get(7)?,
    })
}

const SOURCE_SELECT: &str = "SELECT s.id, s.root_path, s.path_key, s.include_subdirectories,
  s.include_images, s.include_text, s.ai_allowed, s.status, s.last_scanned_at,
  (SELECT COUNT(*) FROM external_assets e WHERE e.source_id = s.id),
  s.created_at, s.updated_at FROM import_sources s";

fn source_query(
    conn: &rusqlite::Connection,
    predicate: &str,
    params: impl rusqlite::Params,
) -> Result<Option<ImportSource>, AppError> {
    conn.query_row(
        &format!("{SOURCE_SELECT} {predicate}"),
        params,
        source_from_row,
    )
    .optional()
    .map_err(db_error)
}

fn source_from_row(row: &Row<'_>) -> rusqlite::Result<ImportSource> {
    let status: String = row.get(7)?;
    Ok(ImportSource {
        id: row.get(0)?,
        root_path: row.get(1)?,
        path_key: row.get(2)?,
        include_subdirectories: row.get(3)?,
        include_images: row.get(4)?,
        include_text: row.get(5)?,
        ai_allowed: row.get(6)?,
        status: SourceStatus::parse(&status),
        last_scanned_at: row.get(8)?,
        item_count: row.get::<_, i64>(9)? as u64,
        created_at: row.get(10)?,
        updated_at: row.get(11)?,
    })
}
