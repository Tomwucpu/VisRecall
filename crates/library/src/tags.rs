//! 标签读写：标签云、按素材查询、关联/解除、重命名与删除（design §6.3）。
//!
//! 三条语义约束决定了这里的实现方式：
//! 1. 同名复用：`tags.name` 上有 `NOCASE` 唯一索引，查找用 `WHERE name = ?1 COLLATE NOCASE`；
//! 2. 关联来源与标签来源分离：`item_tags.origin` 记录「这一次」是自动还是手动；
//! 3. 抑制记录：用户手动移除**自动**标签时写 `tag_suppressions`，阶段 03 重新解析不得贴回。
//!
//! 库层不发事件（`item_tags_updated` / `tags_changed` 由 `app-core` 在事务提交后发），
//! 也不做文件操作。

use rusqlite::{params, ErrorCode, OptionalExtension, Row};
use visrecall_domain::tag::{normalize_tag_name, ItemTag, Tag, TagSource};
use visrecall_domain::{now_millis, AppError};

use crate::db::{db_error, Library};

const TAG_COLUMNS: &str = "t.id, t.name, t.source, t.created_at";

impl Library {
    /// 标签云：全部标签 + 各自的素材数量，按数量降序、同数量按名称升序。
    ///
    /// `ItemTag.origin` 在标签云语境下没有「这次关联」，取标签自身的创建来源。
    pub fn tags_list(&self) -> Result<Vec<ItemTag>, AppError> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {TAG_COLUMNS}, COUNT(it.item_id) AS item_count
                 FROM tags t LEFT JOIN item_tags it ON it.tag_id = t.id
                 GROUP BY t.id
                 ORDER BY item_count DESC, t.name ASC"
            ))
            .map_err(db_error)?;
        let rows = stmt
            .query_map([], |row| {
                let tag = row_to_tag(row)?;
                let count = row.get::<_, i64>(4)?;
                let created_from = tag.source;
                Ok(ItemTag::new(tag, created_from, count as u64))
            })
            .map_err(db_error)?;

        let mut tags = Vec::new();
        for row in rows {
            tags.push(row.map_err(db_error)?);
        }
        Ok(tags)
    }

    /// 某条素材的标签，按关联创建时间升序；`item_count` 填 0（标签云才需要数量）。
    pub fn tags_for_item(&self, item_id: &str) -> Result<Vec<ItemTag>, AppError> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {TAG_COLUMNS}, it.origin
                 FROM item_tags it JOIN tags t ON t.id = it.tag_id
                 WHERE it.item_id = ?1
                 ORDER BY it.created_at ASC, t.name ASC"
            ))
            .map_err(db_error)?;
        let rows = stmt
            .query_map([item_id], |row| {
                let tag = row_to_tag(row)?;
                let origin = TagSource::parse(&row.get::<_, String>(4)?)
                    .map_err(|err| to_sql_error(err, 4))?;
                Ok(ItemTag::new(tag, origin, 0))
            })
            .map_err(db_error)?;

        let mut tags = Vec::new();
        for row in rows {
            tags.push(row.map_err(db_error)?);
        }
        Ok(tags)
    }

    /// 给素材贴标签：同名（大小写不敏感）复用，已关联时保持原关联来源。
    ///
    /// 先校验素材存在：否则外键失败会变成 `DATABASE`，前端无法区分「素材不存在」。
    /// 手动贴回某个被抑制的名称时，一并解除抑制（用户明确表达了要它）。
    pub fn tag_add(&self, item_id: &str, raw_name: &str) -> Result<ItemTag, AppError> {
        if self.get_item(item_id)?.is_none() {
            return Err(AppError::NotFound);
        }
        let name = normalize_tag_name(raw_name)?;

        let mut conn = self.conn()?;
        let tx = conn.transaction().map_err(db_error)?;
        let now = now_millis();

        let existing: Option<(String, String, String, i64)> = tx
            .query_row(
                "SELECT id, name, source, created_at FROM tags WHERE name = ?1 COLLATE NOCASE",
                [&name],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(db_error)?;

        let (tag_id, tag_name, tag_source, created_at) = match existing {
            Some((id, stored_name, source, created_at)) => {
                (id, stored_name, TagSource::parse(&source)?, created_at)
            }
            None => {
                let id = uuid::Uuid::new_v4().to_string();
                tx.execute(
                    "INSERT INTO tags (id, name, source, created_at) VALUES (?1, ?2, 'manual', ?3)",
                    params![&id, &name, now],
                )
                .map_err(db_error)?;
                (id, name.clone(), TagSource::Manual, now)
            }
        };

        tx.execute(
            "INSERT OR IGNORE INTO item_tags (item_id, tag_id, origin, created_at)
             VALUES (?1, ?2, 'manual', ?3)",
            params![item_id, &tag_id, now],
        )
        .map_err(db_error)?;
        tx.execute(
            "DELETE FROM tag_suppressions WHERE item_id = ?1 AND tag_name = ?2",
            params![item_id, &tag_name],
        )
        .map_err(db_error)?;

        let origin: String = tx
            .query_row(
                "SELECT origin FROM item_tags WHERE item_id = ?1 AND tag_id = ?2",
                params![item_id, &tag_id],
                |row| row.get(0),
            )
            .map_err(db_error)?;
        let item_count: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM item_tags WHERE tag_id = ?1",
                [&tag_id],
                |row| row.get(0),
            )
            .map_err(db_error)?;
        tx.commit().map_err(db_error)?;

        let origin_source = TagSource::parse(&origin)?;
        Ok(ItemTag::new(
            Tag {
                id: tag_id,
                name: tag_name,
                // `Tag::source` 是标签创建来源；本次关联来源在 `origin`。
                source: tag_source,
                created_at,
            },
            origin_source,
            item_count as u64,
        ))
    }

    /// 解除素材与标签的关联。
    ///
    /// - 自动关联被移除时写抑制记录（阶段 03 不得贴回）；
    /// - `source='auto'` 且已无任何关联的标签一并删除（自动标签的寿命跟随关联）；
    ///   手动标签保留，便于复用与输入建议。
    pub fn tag_detach(&self, item_id: &str, tag_id: &str) -> Result<(), AppError> {
        let mut conn = self.conn()?;
        let tx = conn.transaction().map_err(db_error)?;

        let found: Option<(String, String)> = tx
            .query_row(
                "SELECT it.origin, t.name
                 FROM item_tags it JOIN tags t ON t.id = it.tag_id
                 WHERE it.item_id = ?1 AND it.tag_id = ?2",
                params![item_id, tag_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(db_error)?;
        let Some((origin, tag_name)) = found else {
            return Err(AppError::NotFound);
        };

        tx.execute(
            "DELETE FROM item_tags WHERE item_id = ?1 AND tag_id = ?2",
            params![item_id, tag_id],
        )
        .map_err(db_error)?;

        if TagSource::parse(&origin)? == TagSource::Auto {
            tx.execute(
                "INSERT OR IGNORE INTO tag_suppressions (item_id, tag_name, created_at)
                 VALUES (?1, ?2, ?3)",
                params![item_id, &tag_name, now_millis()],
            )
            .map_err(db_error)?;
        }

        tx.execute(
            "DELETE FROM tags
             WHERE id = ?1 AND source = 'auto'
               AND NOT EXISTS (SELECT 1 FROM item_tags WHERE tag_id = ?1)",
            [tag_id],
        )
        .map_err(db_error)?;
        tx.commit().map_err(db_error)?;
        Ok(())
    }

    /// 重命名标签；撞名（大小写不敏感）返回 `INVALID_INPUT`。
    pub fn tag_rename(&self, tag_id: &str, raw_name: &str) -> Result<(), AppError> {
        let name = normalize_tag_name(raw_name)?;
        let conn = self.conn()?;
        let updated = conn
            .execute(
                "UPDATE tags SET name = ?2 WHERE id = ?1",
                params![tag_id, &name],
            )
            .map_err(rename_error)?;
        if updated == 0 {
            return Err(AppError::NotFound);
        }
        Ok(())
    }

    /// 删除标签（关联靠外键级联），返回解除的关联条数。
    pub fn tag_delete(&self, tag_id: &str) -> Result<u64, AppError> {
        let mut conn = self.conn()?;
        let tx = conn.transaction().map_err(db_error)?;

        let links: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM item_tags WHERE tag_id = ?1",
                [tag_id],
                |row| row.get(0),
            )
            .map_err(db_error)?;
        let deleted = tx
            .execute("DELETE FROM tags WHERE id = ?1", [tag_id])
            .map_err(db_error)?;
        if deleted == 0 {
            return Err(AppError::NotFound);
        }
        tx.commit().map_err(db_error)?;
        Ok(links as u64)
    }

    /// 测试辅助：把一个 `source='auto'` 的标签贴到素材上（阶段 03 的自动标注会用同一条路径）。
    ///
    /// 生产代码本阶段没有自动标注，但「移除自动标签要写抑制记录」是硬需求，
    /// 因此保留一个可控入口供测试与阶段 03 复用。
    #[doc(hidden)]
    pub fn attach_auto_tag_for_test(
        &self,
        item_id: &str,
        raw_name: &str,
    ) -> Result<String, AppError> {
        let name = normalize_tag_name(raw_name)?;
        let mut conn = self.conn()?;
        let tx = conn.transaction().map_err(db_error)?;
        let now = now_millis();
        let existing: Option<String> = tx
            .query_row(
                "SELECT id FROM tags WHERE name = ?1 COLLATE NOCASE",
                [&name],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_error)?;
        let tag_id = match existing {
            Some(id) => id,
            None => {
                let id = uuid::Uuid::new_v4().to_string();
                tx.execute(
                    "INSERT INTO tags (id, name, source, created_at) VALUES (?1, ?2, 'auto', ?3)",
                    params![&id, &name, now],
                )
                .map_err(db_error)?;
                id
            }
        };
        tx.execute(
            "INSERT OR IGNORE INTO item_tags (item_id, tag_id, origin, created_at)
             VALUES (?1, ?2, 'auto', ?3)",
            params![item_id, &tag_id, now],
        )
        .map_err(db_error)?;
        tx.commit().map_err(db_error)?;
        Ok(tag_id)
    }

    /// 测试辅助：读取抑制记录里的标签名。
    #[doc(hidden)]
    pub fn suppressed_tag_names_for_test(&self, item_id: &str) -> Result<Vec<String>, AppError> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare("SELECT tag_name FROM tag_suppressions WHERE item_id = ?1 ORDER BY tag_name")
            .map_err(db_error)?;
        let rows = stmt
            .query_map([item_id], |row| row.get::<_, String>(0))
            .map_err(db_error)?;
        let mut names = Vec::new();
        for row in rows {
            names.push(row.map_err(db_error)?);
        }
        Ok(names)
    }
}

fn row_to_tag(row: &Row<'_>) -> rusqlite::Result<Tag> {
    Ok(Tag {
        id: row.get(0)?,
        name: row.get(1)?,
        source: TagSource::parse(&row.get::<_, String>(2)?).map_err(|err| to_sql_error(err, 2))?,
        created_at: row.get(3)?,
    })
}

/// 唯一索引冲突（改名撞名）转成可读的 `INVALID_INPUT`，其余保持 `DATABASE`。
fn rename_error(err: rusqlite::Error) -> AppError {
    let is_conflict = matches!(
        &err,
        rusqlite::Error::SqliteFailure(inner, _) if inner.code == ErrorCode::ConstraintViolation
    );
    if is_conflict {
        AppError::InvalidInput("tag name already exists".into())
    } else {
        db_error(err)
    }
}

fn to_sql_error(err: AppError, index: usize) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        index,
        rusqlite::types::Type::Text,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            err.to_string(),
        )),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use visrecall_domain::item::{ItemKind, NewItem};

    fn library() -> (tempfile::TempDir, Library) {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::open(dir.path().to_path_buf()).unwrap();
        (dir, lib)
    }

    fn image_item(n: i64) -> NewItem {
        NewItem {
            kind: ItemKind::Image,
            captured_at: 1_700_000_000_000 + n,
            source_app: None,
            source_title: None,
            source_url: None,
            width: None,
            height: None,
            char_count: None,
            original_path: format!("originals/images/2026/11/{n}.png"),
            content_hash: format!("hash-{n}"),
        }
    }

    #[test]
    fn deleting_item_row_cascades_item_tags() {
        let (_dir, lib) = library();
        let item = lib.insert_item(image_item(1)).unwrap();
        lib.tag_add(&item.id, "work").unwrap();

        let conn = lib.conn().unwrap();
        conn.execute("DELETE FROM items WHERE id = ?1", [&item.id])
            .unwrap();
        drop(conn);

        assert!(lib.tags_for_item(&item.id).unwrap().is_empty());
        // 关联被级联清空，标签本身（手动）保留但计数归零。
        let cloud = lib.tags_list().unwrap();
        assert_eq!(cloud.len(), 1);
        assert_eq!(cloud[0].item_count, 0);
    }
}
