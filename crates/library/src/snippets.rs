//! 片段 HTML 的读写（迁移 0008）。
//!
//! 只服务「复制时还原富文本」：正文仍以 `texts/<id>.txt` 为准，HTML 是可选附件。
//! 没有记录 = 旧素材或纯文本来源，调用方回退纯文本复制即可。

use rusqlite::OptionalExtension;
use visrecall_domain::{now_millis, AppError};

use crate::db::{db_error, Library};

impl Library {
    /// 保存/覆盖片段的 HTML（同一素材重复导入只保留最新一份）。
    pub fn set_snippet_html(&self, item_id: &str, html: &str) -> Result<(), AppError> {
        if html.trim().is_empty() {
            return Err(AppError::InvalidInput("snippet html is empty".into()));
        }
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO snippet_html (item_id, html, created_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(item_id) DO UPDATE SET html = excluded.html, created_at = excluded.created_at",
            rusqlite::params![item_id, html, now_millis()],
        )
        .map_err(db_error)?;
        Ok(())
    }

    /// 读取片段的 HTML；没有记录时返回 `None`（调用方回退纯文本）。
    pub fn get_snippet_html(&self, item_id: &str) -> Result<Option<String>, AppError> {
        let conn = self.conn()?;
        conn.query_row(
            "SELECT html FROM snippet_html WHERE item_id = ?1",
            [item_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(db_error)
    }
}

#[cfg(test)]
mod tests {
    use visrecall_domain::item::{ItemKind, NewItem};
    use visrecall_domain::now_millis;

    use super::*;

    fn fixture() -> (tempfile::TempDir, Library) {
        let dir = tempfile::tempdir().unwrap();
        let library = Library::open(dir.path().join("library")).unwrap();
        (dir, library)
    }

    fn insert_text(library: &Library) -> String {
        library
            .insert_item(NewItem {
                kind: ItemKind::Text,
                captured_at: now_millis(),
                source_app: Some("chrome.exe".into()),
                source_title: Some("片段".into()),
                source_url: None,
                width: None,
                height: None,
                char_count: Some(4),
                original_path: "texts/t1.txt".into(),
                content_hash: "hash".into(),
            })
            .unwrap()
            .id
    }

    #[test]
    fn html_is_stored_and_overwritten() {
        let (_dir, library) = fixture();
        let id = insert_text(&library);

        assert_eq!(library.get_snippet_html(&id).unwrap(), None);
        library.set_snippet_html(&id, "<p>第一版</p>").unwrap();
        assert_eq!(
            library.get_snippet_html(&id).unwrap().as_deref(),
            Some("<p>第一版</p>")
        );

        library.set_snippet_html(&id, "<p>第二版</p>").unwrap();
        assert_eq!(
            library.get_snippet_html(&id).unwrap().as_deref(),
            Some("<p>第二版</p>")
        );
    }

    #[test]
    fn blank_html_is_rejected() {
        let (_dir, library) = fixture();
        let id = insert_text(&library);
        assert!(library.set_snippet_html(&id, "   ").is_err());
        assert_eq!(library.get_snippet_html(&id).unwrap(), None);
    }

    /// 删除素材必须连带清掉 HTML：不能让片段内容留在库里。
    #[test]
    fn html_follows_item_deletion() {
        let (_dir, library) = fixture();
        let id = insert_text(&library);
        library.set_snippet_html(&id, "<p>会随素材删除</p>").unwrap();

        library.delete_item(&id).unwrap();
        let conn = library.conn().unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM snippet_html", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }
}
