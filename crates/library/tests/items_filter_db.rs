//! `items.title` 与结构化筛选（`ItemFilters`）的集成测试（真实 SQLite + 临时库根目录）。

use visrecall_domain::item::{ItemKind, ItemStatus, NewItem};
use visrecall_domain::{AppError, ItemFilters};
use visrecall_library::Library;

fn new_item(n: i64, kind: ItemKind) -> NewItem {
    NewItem {
        kind,
        captured_at: 1_700_000_000_000 + n,
        source_app: Some("chrome.exe".into()),
        source_title: Some(format!("来源标题 {n}")),
        source_url: None,
        width: Some(100),
        height: Some(50),
        char_count: None,
        original_path: match kind {
            ItemKind::Image => format!("originals/images/2026/11/{n}.png"),
            _ => format!("texts/{n}.txt"),
        },
        content_hash: format!("hash-{n}"),
    }
}

fn library() -> (tempfile::TempDir, Library) {
    let dir = tempfile::tempdir().unwrap();
    let lib = Library::open(dir.path().to_path_buf()).unwrap();
    (dir, lib)
}

#[test]
fn title_defaults_to_source_title_and_can_be_cleared() {
    let (_dir, lib) = library();
    let item = lib.insert_item(new_item(1, ItemKind::Image)).unwrap();
    assert!(item.title.is_none());
    assert_eq!(item.display_title(), item.source_title.as_deref());

    lib.set_item_title(&item.id, Some("  季度营收  ")).unwrap();
    let renamed = lib.get_item(&item.id).unwrap().unwrap();
    assert_eq!(renamed.title.as_deref(), Some("季度营收"));
    assert_eq!(renamed.display_title(), Some("季度营收"));
    // 来源标题保持原值，详情里仍能看到捕获时的窗口标题。
    assert_eq!(
        renamed.source_title.as_deref(),
        Some("来源标题 1"),
        "source_title 不应被改名覆盖"
    );

    // 空白 = 恢复未改名，写 NULL 而不是空串。
    lib.set_item_title(&item.id, Some("   ")).unwrap();
    let cleared = lib.get_item(&item.id).unwrap().unwrap();
    assert!(cleared.title.is_none());
    assert_eq!(cleared.display_title(), Some("来源标题 1"));
}

#[test]
fn title_rejects_unknown_id_and_too_long_text() {
    let (_dir, lib) = library();
    let item = lib.insert_item(new_item(1, ItemKind::Image)).unwrap();

    assert!(matches!(
        lib.set_item_title("missing", Some("标题")),
        Err(AppError::NotFound)
    ));

    let too_long = "字".repeat(visrecall_library::items::MAX_TITLE_CHARS + 1);
    assert!(matches!(
        lib.set_item_title(&item.id, Some(&too_long)),
        Err(AppError::InvalidInput(_))
    ));

    let at_limit = "字".repeat(visrecall_library::items::MAX_TITLE_CHARS);
    lib.set_item_title(&item.id, Some(&at_limit)).unwrap();
    assert_eq!(
        lib.get_item(&item.id)
            .unwrap()
            .unwrap()
            .title
            .unwrap()
            .chars()
            .count(),
        visrecall_library::items::MAX_TITLE_CHARS
    );
}

#[test]
fn filter_by_kinds_uses_or_within_group() {
    let (_dir, lib) = library();
    let image = lib.insert_item(new_item(1, ItemKind::Image)).unwrap();
    let text = lib.insert_item(new_item(2, ItemKind::Text)).unwrap();
    let link = lib.insert_item(new_item(3, ItemKind::Link)).unwrap();

    let filters = ItemFilters {
        kinds: vec![ItemKind::Image, ItemKind::Text],
        ..ItemFilters::default()
    };
    let rows = lib.list_items_filtered(&filters, None, 50).unwrap();
    let ids: Vec<&str> = rows.iter().map(|row| row.id.as_str()).collect();
    assert_eq!(ids, vec![text.id.as_str(), image.id.as_str()]);
    assert!(!ids.contains(&link.id.as_str()));
}

#[test]
fn filter_by_statuses_uses_or_within_group() {
    let (_dir, lib) = library();
    let ready = lib.insert_item(new_item(1, ItemKind::Image)).unwrap();
    let failed = lib.insert_item(new_item(2, ItemKind::Image)).unwrap();
    let pending = lib.insert_item(new_item(3, ItemKind::Image)).unwrap();
    lib.update_status(&ready.id, ItemStatus::Ready, None).unwrap();
    lib.update_status(&failed.id, ItemStatus::Failed, None)
        .unwrap();

    let filters = ItemFilters {
        statuses: vec![ItemStatus::Ready, ItemStatus::Failed],
        ..ItemFilters::default()
    };
    let ids = lib.list_item_ids_filtered(&filters, 50).unwrap();
    assert_eq!(ids, vec![failed.id.clone(), ready.id.clone()]);
    assert!(!ids.contains(&pending.id));
}

#[test]
fn filter_by_tags_is_or_within_group_and_intersects_other_groups() {
    let (_dir, lib) = library();
    let work = lib.insert_item(new_item(1, ItemKind::Image)).unwrap();
    let finance = lib.insert_item(new_item(2, ItemKind::Image)).unwrap();
    let text = lib.insert_item(new_item(3, ItemKind::Text)).unwrap();
    lib.tag_add(&work.id, "工作").unwrap();
    lib.tag_add(&finance.id, "财务").unwrap();
    lib.tag_add(&text.id, "工作").unwrap();

    let tags_only = ItemFilters {
        tags: vec!["工作".into(), "财务".into()],
        ..ItemFilters::default()
    };
    let mut ids = lib.list_item_ids_filtered(&tags_only, 50).unwrap();
    ids.sort();
    let mut expected = vec![work.id.clone(), finance.id.clone(), text.id.clone()];
    expected.sort();
    assert_eq!(ids, expected);

    // 组间取交集：标签「工作」且类型为图片 → 只剩 work。
    let combined = ItemFilters {
        tags: vec!["工作".into()],
        kinds: vec![ItemKind::Image],
        ..ItemFilters::default()
    };
    assert_eq!(
        lib.list_item_ids_filtered(&combined, 50).unwrap(),
        vec![work.id.clone()]
    );
}

#[test]
fn filter_by_time_range_is_half_open() {
    let (_dir, lib) = library();
    let old = lib.insert_item(new_item(10, ItemKind::Image)).unwrap();
    let mid = lib.insert_item(new_item(20, ItemKind::Image)).unwrap();
    let new = lib.insert_item(new_item(30, ItemKind::Image)).unwrap();

    let filters = ItemFilters {
        time_from: Some(mid.captured_at),
        time_to: Some(new.captured_at),
        ..ItemFilters::default()
    };
    // 左闭右开：包含 mid，不含 new。
    assert_eq!(
        lib.list_item_ids_filtered(&filters, 50).unwrap(),
        vec![mid.id.clone()]
    );
    assert!(!filters.is_empty());
    assert!(ItemFilters::default().is_empty());
    let _ = old;
}

#[test]
fn filtered_listing_keeps_keyset_pagination() {
    let (_dir, lib) = library();
    for n in 0..5 {
        lib.insert_item(new_item(n, ItemKind::Image)).unwrap();
    }

    let filters = ItemFilters {
        kinds: vec![ItemKind::Image],
        ..ItemFilters::default()
    };
    let page1 = lib.list_items_filtered(&filters, None, 2).unwrap();
    assert_eq!(page1.len(), 2);
    let cursor = page1.last().unwrap().captured_at;
    let page2 = lib.list_items_filtered(&filters, Some(cursor), 2).unwrap();
    assert_eq!(page2.len(), 2);
    assert!(page2.iter().all(|item| item.captured_at < cursor));
}

#[test]
fn empty_filters_return_all_items() {
    let (_dir, lib) = library();
    lib.insert_item(new_item(1, ItemKind::Image)).unwrap();
    lib.insert_item(new_item(2, ItemKind::Text)).unwrap();

    let rows = lib
        .list_items_filtered(&ItemFilters::default(), None, 50)
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(
        lib.list_item_ids_filtered(&ItemFilters::default(), 50)
            .unwrap()
            .len(),
        2
    );
}
