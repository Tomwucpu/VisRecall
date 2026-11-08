//! 迁移 0003 与索引存储原语的集成测试（tempdir + 真实 SQLite）。
//!
//! 覆盖 tasks.md Task 2 Step 7 的清单：迁移幂等与升级、`write_index_tx` 三写回滚、
//! FTS 覆盖写/删除、蓝绿指针切换、`bump_revision`、删除后无残留、自检计数差异。

use visrecall_domain::{Insight, ItemKind, ItemStatus, NewItem};
use visrecall_library::{
    fts_upsert_tx, insight_upsert_tx, vector_upsert_tx, FtsColumns, Library, QueryLog,
    DEFAULT_VECTOR_TABLE,
};

fn temp_library() -> (tempfile::TempDir, Library) {
    let dir = tempfile::tempdir().unwrap();
    let lib = Library::open(dir.path().to_path_buf()).unwrap();
    (dir, lib)
}

fn new_item(hash: &str, captured_at: i64) -> NewItem {
    NewItem {
        kind: ItemKind::Image,
        captured_at,
        source_app: Some("chrome".into()),
        source_title: Some("季度营收漏斗图".into()),
        source_url: None,
        width: Some(2560),
        height: Some(1440),
        char_count: None,
        original_path: format!("originals/images/2026/11/{hash}.png"),
        content_hash: hash.into(),
    }
}

fn insert_item(lib: &Library, hash: &str) -> (String, i64) {
    let item = lib.insert_item(new_item(hash, 1_700_000_000_000)).unwrap();
    let rowid = lib.item_rowid(&item.id).unwrap().unwrap();
    (item.id, rowid)
}

fn fts_cols() -> FtsColumns<'static> {
    FtsColumns {
        title: "季度 营收 漏斗图",
        summary: "转化率 提升",
        ocr_text: "订单 转化率 42%",
        note: "周会 材料",
        tags: "工作 数据",
        entities: "VisRecall",
        original_text: "",
    }
}

#[test]
fn migration_0003_is_idempotent_and_creates_tables() {
    let dir = tempfile::tempdir().unwrap();
    // 打开两次 = 两次 apply（第二次无待应用迁移）
    drop(Library::open(dir.path().to_path_buf()).unwrap());
    let lib = Library::open(dir.path().to_path_buf()).unwrap();

    for table in [
        "insights",
        "items_fts",
        "item_vectors_int8",
        "queries",
        "schema_migrations",
    ] {
        assert!(
            lib.table_exists_for_test(table).unwrap(),
            "表 {table} 应存在"
        );
    }
    assert_eq!(lib.max_schema_version_for_test().unwrap(), 3);
}

#[test]
fn upgrading_from_0002_keeps_data_and_defaults_revision_to_zero() {
    // 用真实升级路径：先建 0002 库（通过 Library::open 会自动到 3，所以这里手工造旧库再 open）
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("visrecall.db");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
            .unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_migrations (
               version INTEGER PRIMARY KEY, applied_at INTEGER NOT NULL);",
        )
        .unwrap();
        // 只应用 0001+0002（从已发布的迁移文件里取，避免手抄 DDL）
        let m1 = include_str!("../migrations/0001_core.sql");
        let m2 = include_str!("../migrations/0002_library_detail.sql");
        conn.execute_batch(m1).unwrap();
        conn.execute_batch(m2).unwrap();
        conn.execute_batch(
            "INSERT INTO schema_migrations (version, applied_at) VALUES (1, 0), (2, 0);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO items (id, kind, status, captured_at, source_app, source_title, source_url,
                                width, height, char_count, original_path, thumb_path, note, content_hash,
                                created_at, updated_at)
             VALUES ('old-1', 'image', 'ready', 123, 'chrome', '旧素材', NULL, 100, 100, NULL,
                     'originals/images/old.png', NULL, '旧备注', 'old-hash', 1, 1)",
            [],
        )
        .unwrap();
    }

    let lib = Library::open(dir.path().to_path_buf()).unwrap();
    let item = lib.get_item("old-1").unwrap().expect("旧数据必须还在");
    assert_eq!(item.note, "旧备注");
    assert_eq!(item.source_title.as_deref(), Some("旧素材"));
    assert_eq!(item.revision, 0, "旧数据升级后 revision 默认 0");
    assert!(item.primary_color.is_none());

    // 迁移前备份存在（bak-3）
    let backups = dir.path().join("backups");
    assert!(
        backups.join("visrecall.db.bak-3").is_file(),
        "0002 → 0003 迁移前应留备份"
    );
}

#[test]
fn write_index_tx_rolls_back_all_three_writes_on_error() {
    let (_dir, lib) = temp_library();
    let (id, rowid) = insert_item(&lib, "hash-a");
    let insight = Insight::empty(&id, 1);

    let result = lib.write_index_tx(|tx| {
        insight_upsert_tx(tx, &insight)?;
        fts_upsert_tx(tx, rowid, &fts_cols())?;
        Err(visrecall_domain::AppError::Internal(
            "模拟向量写入失败".into(),
        ))
    });
    assert!(result.is_err());

    assert!(lib.insight_get(&id).unwrap().is_none(), "insights 应回滚");
    assert_eq!(lib.fts_count().unwrap(), 0, "FTS 应回滚");
    assert_eq!(lib.vector_count().unwrap(), 0, "向量应回滚");
}

#[test]
fn write_index_tx_commits_all_three_writes() {
    let (_dir, lib) = temp_library();
    let (id, rowid) = insert_item(&lib, "hash-b");
    let mut insight = Insight::empty(&id, 1);
    insight.primary_color = Some("orange".into());

    lib.write_index_tx(|tx| {
        insight_upsert_tx(tx, &insight)?;
        fts_upsert_tx(tx, rowid, &fts_cols())?;
        vector_upsert_tx(tx, DEFAULT_VECTOR_TABLE, rowid, &[1, 2, 3], 3, "model-a")?;
        Ok(())
    })
    .unwrap();

    assert_eq!(
        lib.insight_get(&id)
            .unwrap()
            .unwrap()
            .primary_color
            .as_deref(),
        Some("orange")
    );
    assert_eq!(lib.fts_count().unwrap(), 1);
    assert_eq!(lib.vector_count().unwrap(), 1);
    assert!(lib.index_counts().unwrap().is_consistent());
}

#[test]
fn fts_upsert_overwrites_and_delete_shrinks_count() {
    let (_dir, lib) = temp_library();
    let (_, rowid) = insert_item(&lib, "hash-c");

    lib.fts_upsert(rowid, &fts_cols()).unwrap();
    lib.fts_upsert(rowid, &fts_cols()).unwrap();
    assert_eq!(lib.fts_count().unwrap(), 1, "同 rowid 二次写不应产生重复行");

    lib.fts_delete(rowid).unwrap();
    assert_eq!(lib.fts_count().unwrap(), 0);
}

#[test]
fn blue_green_switch_moves_active_table_and_keeps_old_rows() {
    let (_dir, lib) = temp_library();
    let (_, rowid) = insert_item(&lib, "hash-d");
    lib.vector_upsert(rowid, &[7, 8, 9], 3, "model-a").unwrap();

    lib.create_vector_table("item_vectors_int8_v2").unwrap();
    lib.write_index_tx(|tx| {
        vector_upsert_tx(tx, "item_vectors_int8_v2", rowid, &[1, 1, 1], 3, "model-a")
    })
    .unwrap();

    // 切换前：检索读到旧表
    assert_eq!(lib.active_vector_table(), DEFAULT_VECTOR_TABLE);
    assert_eq!(lib.vector_rows(None).unwrap()[0].embedding, vec![7, 8, 9]);

    lib.vector_switch_active("item_vectors_int8_v2").unwrap();
    assert_eq!(lib.active_vector_table(), "item_vectors_int8_v2");
    assert_eq!(lib.vector_rows(None).unwrap()[0].embedding, vec![1, 1, 1]);

    // 旧表数据仍在，可回滚
    assert_eq!(lib.vector_count_for(DEFAULT_VECTOR_TABLE).unwrap(), 1);
}

#[test]
fn bump_revision_is_monotonic_and_item_scoped() {
    let (_dir, lib) = temp_library();
    let (a, _) = insert_item(&lib, "hash-e");
    let (b, _) = insert_item(&lib, "hash-f");

    assert_eq!(lib.bump_revision(&a).unwrap(), 1);
    assert_eq!(lib.bump_revision(&a).unwrap(), 2);
    assert_eq!(lib.bump_revision(&b).unwrap(), 1);
    assert_eq!(lib.get_item(&a).unwrap().unwrap().revision, 2);
    assert_eq!(lib.get_item(&b).unwrap().unwrap().revision, 1);
}

#[test]
fn deleting_item_removes_fts_insight_and_vector_rows() {
    let (_dir, lib) = temp_library();
    let (id, rowid) = insert_item(&lib, "hash-g");
    lib.write_index_tx(|tx| {
        insight_upsert_tx(tx, &Insight::empty(&id, 1))?;
        fts_upsert_tx(tx, rowid, &fts_cols())?;
        vector_upsert_tx(tx, DEFAULT_VECTOR_TABLE, rowid, &[1], 1, "model-a")?;
        Ok(())
    })
    .unwrap();
    lib.query_log_insert(&QueryLog {
        id: "q-1".into(),
        raw: "漏斗图".into(),
        parsed_json: None,
        result_ids_json: Some(format!(r#"["{id}"]"#)),
        feedback: None,
        created_at: 1,
    })
    .unwrap();

    lib.delete_item(&id).unwrap();
    assert_eq!(lib.fts_count().unwrap(), 0, "FTS 行必须显式删除");
    assert_eq!(lib.vector_count().unwrap(), 0, "向量行应级联删除");
    assert!(
        lib.insight_get(&id).unwrap().is_none(),
        "insights 应级联删除"
    );
}

#[test]
fn purge_library_clears_index_and_query_log() {
    let (_dir, lib) = temp_library();
    let (id, rowid) = insert_item(&lib, "hash-h");
    lib.write_index_tx(|tx| {
        insight_upsert_tx(tx, &Insight::empty(&id, 1))?;
        fts_upsert_tx(tx, rowid, &fts_cols())?;
        vector_upsert_tx(tx, DEFAULT_VECTOR_TABLE, rowid, &[1], 1, "model-a")?;
        Ok(())
    })
    .unwrap();
    lib.query_log_insert(&QueryLog {
        id: "q-2".into(),
        raw: "发票".into(),
        parsed_json: None,
        result_ids_json: None,
        feedback: None,
        created_at: 1,
    })
    .unwrap();

    lib.purge_library().unwrap();
    assert_eq!(lib.count_items().unwrap(), 0);
    assert_eq!(lib.fts_count().unwrap(), 0);
    assert_eq!(lib.vector_count().unwrap(), 0);
    assert_eq!(lib.query_log_count().unwrap(), 0);
}

#[test]
fn self_check_detects_missing_fts_row() {
    let (_dir, lib) = temp_library();
    for i in 0..3 {
        let (_, rowid) = insert_item(&lib, &format!("hash-{i}"));
        lib.fts_upsert(rowid, &fts_cols()).unwrap();
        lib.vector_upsert(rowid, &[1], 1, "model-a").unwrap();
    }
    let counts = lib.index_counts().unwrap();
    assert!(counts.is_consistent());
    assert_eq!(counts.items, 3);

    // 手工删一条 FTS 行（模拟外部改动）
    let rows = lib.vector_rows(None).unwrap();
    lib.fts_delete(rows[0].rowid).unwrap();
    let counts = lib.index_counts().unwrap();
    assert_eq!(counts.items, 3);
    assert_eq!(counts.fts, 2);
    assert!(!counts.is_consistent());
}

#[test]
fn failed_items_are_still_indexable() {
    let (_dir, lib) = temp_library();
    let (id, rowid) = insert_item(&lib, "hash-i");
    lib.update_status(&id, ItemStatus::Failed, None).unwrap();
    lib.fts_upsert(rowid, &fts_cols()).unwrap();
    // failed 素材仍可被 OCR 命中（P3-R07）
    let hits = lib.fts_search("\"漏斗图\"", None, 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].rowid, rowid);
}
