//! `items` 表的 CRUD：插入、按 `captured_at` 键集分页读取、取单条、更新状态。

use rusqlite::{types::Type, OptionalExtension, Row};
use visrecall_domain::item::{Item, ItemKind, ItemStatus, NewItem};
use visrecall_domain::{new_item_id, now_millis, AppError};

use crate::db::{db_error, Library};

/// 读取列（表别名 `i`）：`primary_color` 来自 `insights` 的 LEFT JOIN（P3-R10）。
const COLUMNS: &str =
    "i.id, i.kind, i.status, i.captured_at, i.source_app, i.source_title, i.source_url, \
                       i.width, i.height, i.char_count, i.original_path, i.thumb_path, i.note, \
                       i.content_hash, i.revision, ins.primary_color";

/// 读取来源：素材 + 解析结果（可能没有 insights 行）。
const FROM: &str = "items i LEFT JOIN insights ins ON ins.item_id = i.id";

/// 备注长度上限（按字符计，不是字节）。
pub const MAX_NOTE_CHARS: usize = 10_000;

impl Library {
    /// 插入新素材：生成 id 与时间戳，初始 `status = parsing`，`note` 为空。
    pub fn insert_item(&self, new: NewItem) -> Result<Item, AppError> {
        self.insert_item_with_id(new_item_id(), new)
    }

    /// 同 [`Library::insert_item`]，但 id 由调用方提供。
    ///
    /// 捕获落盘需要先用 id 命名文件（`originals/images/YYYY/MM/<id>.png`），再按同一
    /// id 入库，避免文件名与素材 id 不一致。
    pub fn insert_item_with_id(&self, id: String, new: NewItem) -> Result<Item, AppError> {
        let now = now_millis();
        let item = Item {
            id,
            kind: new.kind,
            status: ItemStatus::Parsing,
            captured_at: new.captured_at,
            source_app: new.source_app,
            source_title: new.source_title,
            source_url: new.source_url,
            width: new.width,
            height: new.height,
            char_count: new.char_count,
            original_path: new.original_path,
            thumb_path: None,
            note: String::new(),
            content_hash: new.content_hash,
            revision: 0,
            primary_color: None,
        };

        let mut conn = self.conn()?;
        let tx = conn.transaction().map_err(db_error)?;
        tx.execute(
            "INSERT INTO items
               (id, kind, status, captured_at, source_app, source_title, source_url,
                width, height, char_count, original_path, thumb_path, note, content_hash,
                created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?15)",
            rusqlite::params![
                &item.id,
                kind_str(item.kind),
                status_str(item.status),
                item.captured_at,
                &item.source_app,
                &item.source_title,
                &item.source_url,
                item.width.map(i64::from),
                item.height.map(i64::from),
                item.char_count.map(i64::from),
                &item.original_path,
                &item.thumb_path,
                &item.note,
                &item.content_hash,
                now,
            ],
        )
        .map_err(db_error)?;
        tx.commit().map_err(db_error)?;

        Ok(item)
    }

    /// 按 `captured_at` 倒序（同毫秒用 rowid 兜底）取一页；`cursor` 为上一页最后一项的
    /// `captured_at`，严格小于该值。
    pub fn list_items(&self, cursor: Option<i64>, limit: u32) -> Result<Vec<Item>, AppError> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {COLUMNS} FROM {FROM}
                 WHERE (?1 IS NULL OR i.captured_at < ?1)
                 ORDER BY i.captured_at DESC, i.rowid DESC
                 LIMIT ?2"
            ))
            .map_err(db_error)?;
        let rows = stmt
            .query_map(rusqlite::params![cursor, i64::from(limit)], row_to_item)
            .map_err(db_error)?;

        let mut items = Vec::new();
        for row in rows {
            items.push(row.map_err(db_error)?);
        }
        Ok(items)
    }

    /// 按 id 取单条；不存在返回 `Ok(None)`。
    pub fn get_item(&self, id: &str) -> Result<Option<Item>, AppError> {
        let conn = self.conn()?;
        conn.query_row(
            &format!("SELECT {COLUMNS} FROM {FROM} WHERE i.id = ?1"),
            [id],
            row_to_item,
        )
        .optional()
        .map_err(db_error)
    }

    /// 更新状态；`thumb` 为 `Some` 时同时写入缩略图路径，`None` 保留原值。
    pub fn update_status(
        &self,
        id: &str,
        status: ItemStatus,
        thumb: Option<&str>,
    ) -> Result<(), AppError> {
        let conn = self.conn()?;
        let updated = conn
            .execute(
                "UPDATE items
                 SET status = ?2, thumb_path = COALESCE(?3, thumb_path), updated_at = ?4
                 WHERE id = ?1",
                rusqlite::params![id, status_str(status), thumb, now_millis()],
            )
            .map_err(db_error)?;
        if updated == 0 {
            return Err(AppError::NotFound);
        }
        Ok(())
    }
    /// 更新备注（混合拖放的图片 + 文字片段，design §9.2）并刷新 `updated_at`。
    pub fn set_item_note(&self, id: &str, note: &str) -> Result<(), AppError> {
        let conn = self.conn()?;
        let updated = conn
            .execute(
                "UPDATE items SET note = ?2, updated_at = ?3 WHERE id = ?1",
                rusqlite::params![id, note, now_millis()],
            )
            .map_err(db_error)?;
        if updated == 0 {
            return Err(AppError::NotFound);
        }
        Ok(())
    }

    /// 保存备注：先按**字符**校验长度，超限直接拒绝，不静默截断已存内容。
    ///
    /// 前端负责防抖，这里只做「校验 + 单条 UPDATE + 由调用方发事件」。
    /// 并发语义：单写连接串行执行，后写覆盖先写，不引入版本号。
    pub fn set_note(&self, id: &str, text: &str) -> Result<(), AppError> {
        if text.chars().count() > MAX_NOTE_CHARS {
            return Err(AppError::InvalidInput(format!(
                "note longer than {MAX_NOTE_CHARS} chars"
            )));
        }
        self.set_item_note(id, text)
    }

    /// 素材总数。
    pub fn count_items(&self) -> Result<u64, AppError> {
        let conn = self.conn()?;
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM items", [], |row| row.get(0))
            .map_err(db_error)?;
        Ok(count as u64)
    }

    /// `captured_at` 早于 `cutoff` 的素材数（保留策略预估用）。
    pub fn count_items_before(&self, cutoff: i64) -> Result<u64, AppError> {
        let conn = self.conn()?;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM items WHERE captured_at < ?1",
                [cutoff],
                |row| row.get(0),
            )
            .map_err(db_error)?;
        Ok(count as u64)
    }

    /// 过期的素材，按 `captured_at ASC, rowid ASC` 排序；`limit` 夹到 1..=500。
    pub fn list_expired(&self, cutoff: i64, limit: u32) -> Result<Vec<Item>, AppError> {
        let limit = limit.clamp(1, 500);
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {COLUMNS} FROM {FROM}
                 WHERE i.captured_at < ?1
                 ORDER BY i.captured_at ASC, i.rowid ASC
                 LIMIT ?2"
            ))
            .map_err(db_error)?;
        let rows = stmt
            .query_map(rusqlite::params![cutoff, i64::from(limit)], row_to_item)
            .map_err(db_error)?;

        let mut items = Vec::new();
        for row in rows {
            items.push(row.map_err(db_error)?);
        }
        Ok(items)
    }

    /// 删除一条：`jobs` + `items`（标签关联靠外键级联）。调用方负责文件删除与事件。
    ///
    /// 返回 `(original_path, thumb_path)` 供上层删文件；`None` 表示记录不存在。
    /// 两条 DELETE 必须在同一个事务里提交：`jobs.item_id` 在 0001 中没有外键，
    /// 漏删会留下指向不存在素材的任务（阶段 03 会真的跑这些任务）。
    pub(crate) fn delete_item_row(
        &self,
        id: &str,
    ) -> Result<Option<(String, Option<String>)>, AppError> {
        let mut conn = self.conn()?;
        let tx = conn.transaction().map_err(db_error)?;
        let row: Option<(Option<String>, Option<String>, i64)> = tx
            .query_row(
                "SELECT original_path, thumb_path, rowid FROM items WHERE id = ?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(db_error)?;
        let Some((original_path, thumb_path, rowid)) = row else {
            return Ok(None);
        };

        tx.execute("DELETE FROM jobs WHERE item_id = ?1", [id])
            .map_err(db_error)?;
        // FTS 是 contentful 虚表，没有外键：必须在同一事务里显式删（design §1.2 澄清）。
        // insights 与向量行由 `items` 的外键级联清理。
        tx.execute("DELETE FROM items_fts WHERE rowid = ?1", [rowid])
            .map_err(db_error)?;
        tx.execute("DELETE FROM items WHERE id = ?1", [id])
            .map_err(db_error)?;
        tx.commit().map_err(db_error)?;

        Ok(Some((original_path.unwrap_or_default(), thumb_path)))
    }

    /// 素材的 `rowid`（FTS 与向量表都以它为键）；不存在返回 `Ok(None)`。
    pub fn item_rowid(&self, id: &str) -> Result<Option<i64>, AppError> {
        let conn = self.conn()?;
        conn.query_row("SELECT rowid FROM items WHERE id = ?1", [id], |row| {
            row.get(0)
        })
        .optional()
        .map_err(db_error)
    }

    /// 递增 `revision` 并返回新值（`jobs.revision` 的真相来源，D3-03）。
    ///
    /// 必须与 [`Library::enqueue_job`] 在同一事务语义内调用（pipeline 侧用
    /// `bump_revision` + `enqueue` 的顺序保证），否则旧任务可能带着新 revision 逃逸。
    pub fn bump_revision(&self, id: &str) -> Result<i64, AppError> {
        let conn = self.conn()?;
        conn.query_row(
            "UPDATE items SET revision = revision + 1, updated_at = ?2 WHERE id = ?1
             RETURNING revision",
            rusqlite::params![id, now_millis()],
            |row| row.get(0),
        )
        .optional()
        .map_err(db_error)?
        .ok_or(AppError::NotFound)
    }

    /// 事务内的 [`Library::bump_revision`]。
    pub fn bump_revision_tx(tx: &rusqlite::Transaction<'_>, id: &str) -> Result<i64, AppError> {
        tx.query_row(
            "UPDATE items SET revision = revision + 1, updated_at = ?2 WHERE id = ?1
             RETURNING revision",
            rusqlite::params![id, now_millis()],
            |row| row.get(0),
        )
        .optional()
        .map_err(db_error)?
        .ok_or(AppError::NotFound)
    }

    /// 测试用：读取一行的 `(created_at, updated_at)`（`Item` 不暴露这两个字段）。
    #[doc(hidden)]
    pub fn item_timestamps_for_test(&self, id: &str) -> Result<(i64, i64), AppError> {
        let conn = self.conn()?;
        conn.query_row(
            "SELECT created_at, updated_at FROM items WHERE id = ?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(db_error)?
        .ok_or(AppError::NotFound)
    }
}

fn row_to_item(row: &Row<'_>) -> rusqlite::Result<Item> {
    Ok(Item {
        id: row.get(0)?,
        kind: parse_kind(row.get::<_, String>(1)?, 1)?,
        status: parse_status(row.get::<_, String>(2)?, 2)?,
        captured_at: row.get(3)?,
        source_app: row.get(4)?,
        source_title: row.get(5)?,
        source_url: row.get(6)?,
        width: row.get::<_, Option<i64>>(7)?.map(|value| value as u32),
        height: row.get::<_, Option<i64>>(8)?.map(|value| value as u32),
        char_count: row.get::<_, Option<i64>>(9)?.map(|value| value as u32),
        original_path: row.get(10)?,
        thumb_path: row.get(11)?,
        note: row.get(12)?,
        content_hash: row.get(13)?,
        revision: row.get(14)?,
        primary_color: row.get(15)?,
    })
}

fn kind_str(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::Image => "image",
        ItemKind::Text => "text",
        ItemKind::Link => "link",
    }
}

fn status_str(status: ItemStatus) -> &'static str {
    match status {
        ItemStatus::Parsing => "parsing",
        ItemStatus::Ready => "ready",
        ItemStatus::Pending => "pending",
        ItemStatus::Failed => "failed",
    }
}

fn parse_kind(text: String, index: usize) -> rusqlite::Result<ItemKind> {
    match text.as_str() {
        "image" => Ok(ItemKind::Image),
        "text" => Ok(ItemKind::Text),
        "link" => Ok(ItemKind::Link),
        _ => Err(conversion_error(index, Type::Text, &text)),
    }
}

fn parse_status(text: String, index: usize) -> rusqlite::Result<ItemStatus> {
    match text.as_str() {
        "parsing" => Ok(ItemStatus::Parsing),
        "ready" => Ok(ItemStatus::Ready),
        "pending" => Ok(ItemStatus::Pending),
        "failed" => Ok(ItemStatus::Failed),
        _ => Err(conversion_error(index, Type::Text, &text)),
    }
}

fn conversion_error(index: usize, kind: Type, value: &str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        index,
        kind,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("unexpected enum value {value:?}"),
        )),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enum_strings_roundtrip() {
        for kind in [ItemKind::Image, ItemKind::Text, ItemKind::Link] {
            assert_eq!(parse_kind(kind_str(kind).to_string(), 0).unwrap(), kind);
        }
        for status in [
            ItemStatus::Parsing,
            ItemStatus::Ready,
            ItemStatus::Pending,
            ItemStatus::Failed,
        ] {
            assert_eq!(
                parse_status(status_str(status).to_string(), 0).unwrap(),
                status
            );
        }
        assert!(parse_kind("video".into(), 0).is_err());
        assert!(parse_status("done".into(), 0).is_err());
    }

    fn test_library() -> (tempfile::TempDir, Library) {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::open(dir.path().to_path_buf()).unwrap();
        (dir, lib)
    }

    fn image_item() -> NewItem {
        NewItem {
            kind: ItemKind::Image,
            captured_at: 1_700_000_000_000,
            source_app: None,
            source_title: None,
            source_url: None,
            width: None,
            height: None,
            char_count: None,
            original_path: "originals/images/2026/11/a.png".into(),
            content_hash: "hash".into(),
        }
    }

    #[test]
    fn delete_item_row_removes_jobs_and_returns_paths() {
        let (_dir, lib) = test_library();
        let item = lib.insert_item(image_item()).unwrap();
        lib.update_status(&item.id, ItemStatus::Ready, Some("thumbs/a.png"))
            .unwrap();
        lib.enqueue_job(&item.id, "thumbnail", 1).unwrap();
        lib.tag_add(&item.id, "work").unwrap();

        let paths = lib.delete_item_row(&item.id).unwrap().unwrap();
        assert_eq!(paths.0, "originals/images/2026/11/a.png");
        assert_eq!(paths.1.as_deref(), Some("thumbs/a.png"));

        assert!(lib.get_item(&item.id).unwrap().is_none());
        let conn = lib.conn().unwrap();
        let jobs: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM jobs WHERE item_id = ?1",
                [&item.id],
                |row| row.get(0),
            )
            .unwrap();
        let links: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM item_tags WHERE item_id = ?1",
                [&item.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(jobs, 0);
        assert_eq!(links, 0);
    }

    #[test]
    fn delete_item_row_returns_none_for_missing_item() {
        let (_dir, lib) = test_library();
        assert!(lib.delete_item_row("no-such-item").unwrap().is_none());
    }

    #[test]
    fn bump_revision_increments_per_item() {
        let (_dir, lib) = test_library();
        let item = lib.insert_item(image_item()).unwrap();
        assert_eq!(item.revision, 0);

        assert_eq!(lib.bump_revision(&item.id).unwrap(), 1);
        assert_eq!(lib.bump_revision(&item.id).unwrap(), 2);
        assert_eq!(lib.get_item(&item.id).unwrap().unwrap().revision, 2);

        assert!(matches!(
            lib.bump_revision("no-such-item"),
            Err(AppError::NotFound)
        ));
    }

    #[test]
    fn bump_revision_works_inside_write_index_tx() {
        let (_dir, lib) = test_library();
        let item = lib.insert_item(image_item()).unwrap();

        lib.write_index_tx(|tx| {
            Library::bump_revision_tx(tx, &item.id)?;
            Ok(())
        })
        .unwrap();
        assert_eq!(lib.get_item(&item.id).unwrap().unwrap().revision, 1);

        // 闭包返回 Err → 整体回滚，revision 不变
        let result = lib.write_index_tx(|tx| {
            Library::bump_revision_tx(tx, &item.id)?;
            Err(AppError::Internal("boom".into()))
        });
        assert!(result.is_err());
        assert_eq!(lib.get_item(&item.id).unwrap().unwrap().revision, 1);
    }

    #[test]
    fn list_items_joins_primary_color() {
        let (_dir, lib) = test_library();
        let item = lib.insert_item(image_item()).unwrap();
        assert!(lib.list_items(None, 10).unwrap()[0].primary_color.is_none());

        let mut insight = visrecall_domain::Insight::empty(&item.id, 1);
        insight.primary_color = Some("orange".into());
        lib.insight_upsert(&insight).unwrap();

        assert_eq!(
            lib.list_items(None, 10).unwrap()[0]
                .primary_color
                .as_deref(),
            Some("orange")
        );
        assert_eq!(
            lib.get_item(&item.id)
                .unwrap()
                .unwrap()
                .primary_color
                .as_deref(),
            Some("orange")
        );
    }
}
