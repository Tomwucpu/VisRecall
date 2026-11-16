//! `insights` 表读写：AI 解析结果的唯一落库点。
//!
//! 写入遵循「读旧值 → 合并 → 写回」：`ocr` handler 只动 `ocr_*` 列，`vlm` handler 只动
//! 摘要/属性列，`index` handler 只补占位摘要——三条路径都基于 [`Insight`] 整行读取后
//! 合并再 upsert，避免把彼此的字段覆盖成 NULL（design §6.1）。
//!
//! `entities_json` / `ocr_boxes_json` 是 JSON 文本列：反序列化失败记 `warn` 并当作空，
//! 一条坏解析结果不该让整条素材读不出来。

use rusqlite::{params, OptionalExtension, Row, Transaction};
use visrecall_domain::insight::{OcrBox, SummarySource};
use visrecall_domain::{now_millis, AppError, Insight, ItemKind};

use crate::db::{db_error, Library};

/// AI 描述长度上限（按字符计）：用户手工编辑时校验，超限拒绝而不是截断。
pub const MAX_SUMMARY_CHARS: usize = 2_000;

/// OCR 文本长度上限（按字符计）：与详情展示的 20000 字符口径一致。
pub const MAX_OCR_CHARS: usize = 20_000;

const COLUMNS: &str = "item_id, summary, summary_source, content_type, primary_color, chart_type, \
                       ui_type, entities_json, ocr_text, ocr_boxes_json, ocr_engine, ocr_confidence, \
                       latex, has_formula, model_provider, model_name, confidence, \
                       summary_user_edited, tags_user_edited, updated_at, ocr_user_edited";

/// 整行 upsert（`item_id` 冲突时覆盖所有列）。
pub fn insight_upsert_tx(tx: &Transaction<'_>, insight: &Insight) -> Result<(), AppError> {
    let entities_json = serde_json::to_string(&insight.entities)
        .map_err(|e| AppError::Internal(format!("serialize entities: {e}")))?;
    let boxes_json = serde_json::to_string(&insight.ocr_boxes)
        .map_err(|e| AppError::Internal(format!("serialize ocr boxes: {e}")))?;

    tx.execute(
        "INSERT INTO insights
           (item_id, summary, summary_source, content_type, primary_color, chart_type, ui_type,
            entities_json, ocr_text, ocr_boxes_json, ocr_engine, ocr_confidence, latex, has_formula,
            model_provider, model_name, confidence, summary_user_edited, tags_user_edited, updated_at,
            ocr_user_edited)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)
         ON CONFLICT(item_id) DO UPDATE SET
           summary = excluded.summary,
           summary_source = excluded.summary_source,
           content_type = excluded.content_type,
           primary_color = excluded.primary_color,
           chart_type = excluded.chart_type,
           ui_type = excluded.ui_type,
           entities_json = excluded.entities_json,
           ocr_text = excluded.ocr_text,
           ocr_boxes_json = excluded.ocr_boxes_json,
           ocr_engine = excluded.ocr_engine,
           ocr_confidence = excluded.ocr_confidence,
           latex = excluded.latex,
           has_formula = excluded.has_formula,
           model_provider = excluded.model_provider,
           model_name = excluded.model_name,
           confidence = excluded.confidence,
           summary_user_edited = excluded.summary_user_edited,
           tags_user_edited = excluded.tags_user_edited,
           updated_at = excluded.updated_at,
           ocr_user_edited = excluded.ocr_user_edited",
        params![
            insight.item_id,
            insight.summary,
            insight.summary_source.as_str(),
            insight.content_type,
            insight.primary_color,
            insight.chart_type,
            insight.ui_type,
            entities_json,
            insight.ocr_text,
            boxes_json,
            insight.ocr_engine,
            insight.ocr_confidence,
            insight.latex,
            insight.has_formula,
            insight.model_provider,
            insight.model_name,
            insight.confidence,
            insight.summary_user_edited,
            insight.tags_user_edited,
            insight.updated_at,
            insight.ocr_user_edited,
        ],
    )
    .map_err(db_error)?;
    Ok(())
}

impl Library {
    /// 保存用户编辑的 AI 描述。
    ///
    /// `summary_source` 保持原值（不改成 `user`），只置 `summary_user_edited` 阻止后续自动解析覆盖；
    /// 空白文本写 `NULL`（面板回到「待解析」占位）。成功后触发元数据钩子重新索引。
    pub fn set_summary(&self, id: &str, text: &str) -> Result<(), AppError> {
        self.get_item(id)?.ok_or(AppError::NotFound)?;
        if text.chars().count() > MAX_SUMMARY_CHARS {
            return Err(AppError::InvalidInput(format!(
                "summary longer than {MAX_SUMMARY_CHARS} chars"
            )));
        }

        // 列级写入：只碰 summary / 保护位 / 时间，不回写整行，
        // 避免把并发的解析结果（OCR、颜色、标签等列）覆盖成旧值。
        let trimmed = text.trim();
        let summary = (!trimmed.is_empty()).then(|| trimmed.to_string());
        {
            let conn = self.conn()?;
            conn.execute(
                "INSERT INTO insights (item_id, summary, summary_user_edited, updated_at)
                 VALUES (?1, ?2, 1, ?3)
                 ON CONFLICT(item_id) DO UPDATE SET
                   summary = excluded.summary,
                   summary_user_edited = 1,
                   updated_at = excluded.updated_at",
                params![id, summary, now_millis()],
            )
            .map_err(db_error)?;
        }
        self.notify_metadata_changed(&[id.to_string()]);
        Ok(())
    }

    /// 保存用户编辑的 OCR 文本。
    ///
    /// 只改 `ocr_text` 与保护位 `ocr_user_edited`，保留 `ocr_boxes` 与识别元数据
    /// （没有坐标来源，不重算文字框）。空文本视为清空。成功后触发元数据钩子重新索引。
    pub fn set_ocr_text(&self, id: &str, text: &str) -> Result<(), AppError> {
        let item = self.get_item(id)?.ok_or(AppError::NotFound)?;
        if item.kind != ItemKind::Image {
            return Err(AppError::InvalidInput(
                "only image items have ocr text".to_string(),
            ));
        }
        if text.chars().count() > MAX_OCR_CHARS {
            return Err(AppError::InvalidInput(format!(
                "ocr text longer than {MAX_OCR_CHARS} chars"
            )));
        }

        // 列级写入：只碰 ocr_text / 保护位 / 时间，文字框与识别元数据原样保留。
        {
            let conn = self.conn()?;
            conn.execute(
                "INSERT INTO insights (item_id, ocr_text, ocr_user_edited, updated_at)
                 VALUES (?1, ?2, 1, ?3)
                 ON CONFLICT(item_id) DO UPDATE SET
                   ocr_text = excluded.ocr_text,
                   ocr_user_edited = 1,
                   updated_at = excluded.updated_at",
                params![id, text, now_millis()],
            )
            .map_err(db_error)?;
        }
        self.notify_metadata_changed(&[id.to_string()]);
        Ok(())
    }

    /// 写入（覆盖）一条解析结果。
    pub fn insight_upsert(&self, insight: &Insight) -> Result<(), AppError> {
        let mut conn = self.conn()?;
        let tx = conn.transaction().map_err(db_error)?;
        insight_upsert_tx(&tx, insight)?;
        tx.commit().map_err(db_error)
    }

    /// 读取一条解析结果；不存在返回 `Ok(None)`。
    pub fn insight_get(&self, item_id: &str) -> Result<Option<Insight>, AppError> {
        let conn = self.conn()?;
        conn.query_row(
            &format!("SELECT {COLUMNS} FROM insights WHERE item_id = ?1"),
            [item_id],
            row_to_insight,
        )
        .optional()
        .map_err(db_error)
    }

    /// 批量读取解析结果（检索装配用）：`item_id → Insight`。
    pub fn insights_for(
        &self,
        item_ids: &[String],
    ) -> Result<std::collections::HashMap<String, Insight>, AppError> {
        let mut out = std::collections::HashMap::new();
        if item_ids.is_empty() {
            return Ok(out);
        }
        let conn = self.conn()?;
        let placeholders = vec!["?"; item_ids.len()].join(", ");
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {COLUMNS} FROM insights WHERE item_id IN ({placeholders})"
            ))
            .map_err(db_error)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(item_ids.iter()), row_to_insight)
            .map_err(db_error)?;
        for row in rows {
            let insight = row.map_err(db_error)?;
            out.insert(insight.item_id.clone(), insight);
        }
        Ok(out)
    }

    /// 缺失 `insights` 行的素材 id（启动自检/修复用）。
    pub fn items_missing_insight(&self, limit: u32) -> Result<Vec<String>, AppError> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT i.id FROM items i
                 LEFT JOIN insights ins ON ins.item_id = i.id
                 WHERE ins.item_id IS NULL
                 ORDER BY i.rowid ASC LIMIT ?1",
            )
            .map_err(db_error)?;
        let rows = stmt
            .query_map([i64::from(limit.clamp(1, 1000))], |row| row.get(0))
            .map_err(db_error)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(db_error)?);
        }
        Ok(out)
    }
}

fn row_to_insight(row: &Row<'_>) -> rusqlite::Result<Insight> {
    let summary_source: String = row.get(2)?;
    let entities_json: Option<String> = row.get(7)?;
    let boxes_json: Option<String> = row.get(9)?;

    Ok(Insight {
        item_id: row.get(0)?,
        summary: row.get(1)?,
        summary_source: SummarySource::parse(&summary_source).unwrap_or(SummarySource::None),
        content_type: row.get(3)?,
        primary_color: row.get(4)?,
        chart_type: row.get(5)?,
        ui_type: row.get(6)?,
        entities: parse_entities(entities_json.as_deref()),
        ocr_text: row.get(8)?,
        ocr_boxes: parse_boxes(boxes_json.as_deref()),
        ocr_engine: row.get(10)?,
        ocr_confidence: row.get(11)?,
        latex: row.get(12)?,
        has_formula: row.get::<_, i64>(13)? != 0,
        model_provider: row.get(14)?,
        model_name: row.get(15)?,
        confidence: row.get(16)?,
        summary_user_edited: row.get::<_, i64>(17)? != 0,
        tags_user_edited: row.get::<_, i64>(18)? != 0,
        updated_at: row.get(19)?,
        ocr_user_edited: row.get::<_, i64>(20)? != 0,
    })
}

fn parse_entities(raw: Option<&str>) -> Vec<String> {
    let Some(raw) = raw.filter(|s| !s.is_empty()) else {
        return Vec::new();
    };
    match serde_json::from_str::<Vec<String>>(raw) {
        Ok(values) => values,
        Err(err) => {
            tracing::warn!(%err, "insights.entities_json is not valid json");
            Vec::new()
        }
    }
}

fn parse_boxes(raw: Option<&str>) -> Vec<OcrBox> {
    let Some(raw) = raw.filter(|s| !s.is_empty()) else {
        return Vec::new();
    };
    match serde_json::from_str::<Vec<OcrBox>>(raw) {
        Ok(boxes) => boxes,
        Err(err) => {
            tracing::warn!(%err, "insights.ocr_boxes_json is not valid json");
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Library;
    use visrecall_domain::insight::OcrBox;

    fn temp_library() -> (tempfile::TempDir, Library) {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::open(dir.path().to_path_buf()).unwrap();
        (dir, lib)
    }

    fn insert_item(lib: &Library) -> String {
        lib.insert_item(visrecall_domain::NewItem {
            kind: visrecall_domain::ItemKind::Image,
            captured_at: 1,
            source_app: None,
            source_title: None,
            source_url: None,
            width: None,
            height: None,
            char_count: None,
            original_path: "originals/images/a.png".into(),
            content_hash: "hash".into(),
        })
        .unwrap()
        .id
    }

    #[test]
    fn upsert_then_get_roundtrips_all_fields() {
        let (_dir, lib) = temp_library();
        let id = insert_item(&lib);
        let mut insight = Insight::empty(&id, 42);
        insight.summary = Some("季度营收漏斗图".into());
        insight.summary_source = SummarySource::Vlm;
        insight.primary_color = Some("orange".into());
        insight.chart_type = Some("funnel".into());
        insight.entities = vec!["VisRecall".into()];
        insight.ocr_text = Some("转化率 42%".into());
        insight.ocr_boxes = vec![OcrBox::new("转化率", 0.1, 0.2, 0.3, 0.04)];
        insight.ocr_engine = Some("ppocr-mobile".into());
        insight.ocr_confidence = Some(0.92);
        insight.has_formula = true;
        insight.latex = Some("E=mc^2".into());
        insight.confidence = Some(0.85);

        lib.insight_upsert(&insight).unwrap();
        let loaded = lib.insight_get(&id).unwrap().unwrap();
        assert_eq!(loaded, insight);
    }

    #[test]
    fn upsert_is_idempotent_per_item() {
        let (_dir, lib) = temp_library();
        let id = insert_item(&lib);
        lib.insight_upsert(&Insight::empty(&id, 1)).unwrap();
        let mut second = Insight::empty(&id, 2);
        second.summary = Some("新摘要".into());
        lib.insight_upsert(&second).unwrap();

        let conn = lib.conn().unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM insights WHERE item_id = ?1",
                [&id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn deleting_item_cascades_insight() {
        let (_dir, lib) = temp_library();
        let id = insert_item(&lib);
        lib.insight_upsert(&Insight::empty(&id, 1)).unwrap();
        lib.delete_item(&id).unwrap();
        assert!(lib.insight_get(&id).unwrap().is_none());
    }

    #[test]
    fn broken_json_columns_degrade_to_empty() {
        let (_dir, lib) = temp_library();
        let id = insert_item(&lib);
        {
            let conn = lib.conn().unwrap();
            conn.execute(
                "INSERT INTO insights (item_id, entities_json, ocr_boxes_json, updated_at)
                 VALUES (?1, 'not json', '[{', 0)",
                [&id],
            )
            .unwrap();
        }
        let loaded = lib.insight_get(&id).unwrap().unwrap();
        assert!(loaded.entities.is_empty());
        assert!(loaded.ocr_boxes.is_empty());
    }

    #[test]
    fn items_missing_insight_lists_unparsed_items() {
        let (_dir, lib) = temp_library();
        let id = insert_item(&lib);
        assert_eq!(lib.items_missing_insight(10).unwrap(), vec![id.clone()]);
        lib.insight_upsert(&Insight::empty(&id, 1)).unwrap();
        assert!(lib.items_missing_insight(10).unwrap().is_empty());
    }
}
