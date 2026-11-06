//! 标签 CRUD 与抑制记录的集成测试（真实 SQLite 文件 + 临时库根目录）。

use visrecall_domain::item::{ItemKind, NewItem};
use visrecall_domain::tag::TagSource;
use visrecall_domain::AppError;
use visrecall_library::Library;

fn new_item(n: i64) -> NewItem {
    NewItem {
        kind: ItemKind::Image,
        captured_at: 1_700_000_000_000 + n,
        source_app: Some("chrome.exe".into()),
        source_title: Some("test".into()),
        source_url: None,
        width: Some(100),
        height: Some(50),
        char_count: None,
        original_path: format!("originals/images/2026/11/{n}.png"),
        content_hash: format!("hash-{n}"),
    }
}

fn library() -> (tempfile::TempDir, Library) {
    let dir = tempfile::tempdir().unwrap();
    let lib = Library::open(dir.path().to_path_buf()).unwrap();
    (dir, lib)
}

fn count_tags(lib: &Library) -> i64 {
    // 通过公共 API 间接读取：标签云里每个标签一行。
    lib.tags_list().unwrap().len() as i64
}

#[test]
fn adding_same_tag_to_two_items_reuses_one_row() {
    let (_dir, lib) = library();
    let a = lib.insert_item(new_item(1)).unwrap();
    let b = lib.insert_item(new_item(2)).unwrap();

    let first = lib.tag_add(&a.id, "work").unwrap();
    let second = lib.tag_add(&b.id, "work").unwrap();

    assert_eq!(first.tag.id, second.tag.id);
    assert_eq!(count_tags(&lib), 1);
    assert_eq!(second.item_count, 2);
    assert_eq!(lib.tags_for_item(&a.id).unwrap().len(), 1);
    assert_eq!(lib.tags_for_item(&b.id).unwrap().len(), 1);
}

#[test]
fn tag_names_are_case_insensitive() {
    let (_dir, lib) = library();
    let item = lib.insert_item(new_item(1)).unwrap();

    let first = lib.tag_add(&item.id, "Work").unwrap();
    let second = lib.tag_add(&item.id, "work").unwrap();

    assert_eq!(first.tag.id, second.tag.id);
    assert_eq!(count_tags(&lib), 1);
    // 已关联时重复添加不产生第二条关联。
    assert_eq!(lib.tags_for_item(&item.id).unwrap().len(), 1);
}

#[test]
fn tag_add_normalizes_and_rejects_bad_names() {
    let (_dir, lib) = library();
    let item = lib.insert_item(new_item(1)).unwrap();

    let chip = lib.tag_add(&item.id, "  参考   资料 ").unwrap();
    assert_eq!(chip.tag.name, "参考 资料");

    assert!(matches!(
        lib.tag_add(&item.id, "   "),
        Err(AppError::InvalidInput(_))
    ));
    assert!(matches!(
        lib.tag_add(&item.id, &"字".repeat(33)),
        Err(AppError::InvalidInput(_))
    ));
}

#[test]
fn tag_add_on_missing_item_is_not_found() {
    let (_dir, lib) = library();
    assert!(matches!(
        lib.tag_add("no-such-item", "work"),
        Err(AppError::NotFound)
    ));
    assert_eq!(count_tags(&lib), 0);
}

#[test]
fn detaching_manual_tag_keeps_tag_row() {
    let (_dir, lib) = library();
    let item = lib.insert_item(new_item(1)).unwrap();
    let chip = lib.tag_add(&item.id, "待读").unwrap();

    lib.tag_detach(&item.id, &chip.tag.id).unwrap();

    assert!(lib.tags_for_item(&item.id).unwrap().is_empty());
    // 手动标签保留（可复用、可作为输入建议）。
    assert_eq!(count_tags(&lib), 1);
    assert!(lib
        .suppressed_tag_names_for_test(&item.id)
        .unwrap()
        .is_empty());
}

#[test]
fn detaching_auto_tag_writes_suppression_and_drops_tag() {
    let (_dir, lib) = library();
    let item = lib.insert_item(new_item(1)).unwrap();
    let tag_id = lib.attach_auto_tag_for_test(&item.id, "receipt").unwrap();

    let chips = lib.tags_for_item(&item.id).unwrap();
    assert_eq!(chips.len(), 1);
    assert_eq!(chips[0].origin, TagSource::Auto);

    lib.tag_detach(&item.id, &tag_id).unwrap();

    assert!(lib.tags_for_item(&item.id).unwrap().is_empty());
    // 自动标签无关联后删除，但抑制记录留下，阶段 03 不得贴回。
    assert_eq!(count_tags(&lib), 0);
    assert_eq!(
        lib.suppressed_tag_names_for_test(&item.id).unwrap(),
        vec!["receipt".to_string()]
    );
}

#[test]
fn manual_add_clears_suppression() {
    let (_dir, lib) = library();
    let item = lib.insert_item(new_item(1)).unwrap();
    let tag_id = lib.attach_auto_tag_for_test(&item.id, "receipt").unwrap();
    lib.tag_detach(&item.id, &tag_id).unwrap();
    assert_eq!(
        lib.suppressed_tag_names_for_test(&item.id).unwrap().len(),
        1
    );

    lib.tag_add(&item.id, "receipt").unwrap();
    assert!(lib
        .suppressed_tag_names_for_test(&item.id)
        .unwrap()
        .is_empty());
}

#[test]
fn detach_missing_link_is_not_found() {
    let (_dir, lib) = library();
    let item = lib.insert_item(new_item(1)).unwrap();
    let chip = lib.tag_add(&item.id, "work").unwrap();
    lib.tag_detach(&item.id, &chip.tag.id).unwrap();

    assert!(matches!(
        lib.tag_detach(&item.id, &chip.tag.id),
        Err(AppError::NotFound)
    ));
}

#[test]
fn rename_updates_name_and_rejects_collision() {
    let (_dir, lib) = library();
    let item = lib.insert_item(new_item(1)).unwrap();
    let a = lib.tag_add(&item.id, "work").unwrap();
    let b = lib.tag_add(&item.id, "life").unwrap();

    lib.tag_rename(&a.tag.id, "job").unwrap();
    let names: Vec<String> = lib
        .tags_for_item(&item.id)
        .unwrap()
        .into_iter()
        .map(|chip| chip.tag.name)
        .collect();
    assert!(names.contains(&"job".to_string()));

    assert!(matches!(
        lib.tag_rename(&b.tag.id, "JOB"),
        Err(AppError::InvalidInput(_))
    ));
    // 撞名失败不改变原值。
    assert_eq!(
        lib.tags_for_item(&item.id)
            .unwrap()
            .into_iter()
            .find(|chip| chip.tag.id == b.tag.id)
            .unwrap()
            .tag
            .name,
        "life"
    );
}

#[test]
fn rename_missing_tag_is_not_found() {
    let (_dir, lib) = library();
    assert!(matches!(
        lib.tag_rename("no-such-tag", "x"),
        Err(AppError::NotFound)
    ));
}

#[test]
fn delete_returns_removed_link_count() {
    let (_dir, lib) = library();
    let a = lib.insert_item(new_item(1)).unwrap();
    let b = lib.insert_item(new_item(2)).unwrap();
    let chip = lib.tag_add(&a.id, "work").unwrap();
    lib.tag_add(&b.id, "work").unwrap();

    let removed = lib.tag_delete(&chip.tag.id).unwrap();
    assert_eq!(removed, 2);
    assert_eq!(count_tags(&lib), 0);
    assert!(lib.tags_for_item(&a.id).unwrap().is_empty());
    assert!(lib.tags_for_item(&b.id).unwrap().is_empty());
}

#[test]
fn delete_missing_tag_is_not_found() {
    let (_dir, lib) = library();
    assert!(matches!(
        lib.tag_delete("no-such-tag"),
        Err(AppError::NotFound)
    ));
}

#[test]
fn tag_cloud_orders_by_count_then_name() {
    let (_dir, lib) = library();
    let items: Vec<_> = (0..3)
        .map(|n| lib.insert_item(new_item(n)).unwrap())
        .collect();

    // beta 关联 2 条、alpha 关联 2 条（同数量按名称升序）、gamma 关联 1 条。
    for item in &items[..2] {
        lib.tag_add(&item.id, "beta").unwrap();
        lib.tag_add(&item.id, "alpha").unwrap();
    }
    lib.tag_add(&items[0].id, "gamma").unwrap();

    let cloud = lib.tags_list().unwrap();
    let ordered: Vec<(String, u64)> = cloud
        .into_iter()
        .map(|chip| (chip.tag.name, chip.item_count))
        .collect();
    assert_eq!(
        ordered,
        vec![
            ("alpha".to_string(), 2),
            ("beta".to_string(), 2),
            ("gamma".to_string(), 1)
        ]
    );
}

#[test]
fn item_tags_follow_item_deletion_by_cascade() {
    let (_dir, lib) = library();
    let item = lib.insert_item(new_item(1)).unwrap();
    let chip = lib.tag_add(&item.id, "work").unwrap();

    // 级联删除本身在 `tags.rs` 的单元测试里断言（同 crate 才拿得到连接）；
    // 这里只确认「素材不存在时标签查询为空、解除关联返回 NOT_FOUND」。
    assert!(matches!(
        lib.tag_detach("no-such-item", &chip.tag.id),
        Err(AppError::NotFound)
    ));
    assert!(lib.tags_for_item("no-such-item").unwrap().is_empty());
}
