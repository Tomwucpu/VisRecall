use visrecall_domain::item::{ItemKind, NewItem};
use visrecall_library::{
    AssetReader, ImportTaskEntry, ImportTaskStatus, Library, NewImportSource, NewImportTask,
};

fn new_text() -> NewItem {
    NewItem {
        kind: ItemKind::Text,
        captured_at: 1,
        source_app: Some("VisRecall".into()),
        source_title: Some("说明".into()),
        source_url: None,
        width: None,
        height: None,
        char_count: Some(4),
        original_path: String::new(),
        content_hash: "hash".into(),
    }
}

#[test]
fn reference_read_and_delete_never_touch_external_file() {
    let library_dir = tempfile::tempdir().unwrap();
    let source_dir = tempfile::tempdir().unwrap();
    std::fs::write(source_dir.path().join("note.txt"), "外部内容").unwrap();
    let library = Library::open(library_dir.path().to_path_buf()).unwrap();
    let source = library
        .create_import_source(NewImportSource {
            root_path: source_dir.path().display().to_string(),
            include_subdirectories: true,
            include_images: true,
            include_text: true,
            ai_allowed: false,
        })
        .unwrap();
    let item = library
        .insert_reference_item(&source.id, "note.txt", 8, 1, new_text())
        .unwrap();

    let resolved = AssetReader::new(&library).resolve(&item.id).unwrap();
    assert_eq!(
        resolved.path,
        source_dir.path().join("note.txt").canonicalize().unwrap()
    );
    assert_eq!(item.original_path, "");
    assert_eq!(library.delete_item(&item.id).unwrap().original_path, "");
    assert_eq!(
        std::fs::read_to_string(source_dir.path().join("note.txt")).unwrap(),
        "外部内容"
    );
}

#[test]
fn reference_rejects_parent_and_absolute_paths() {
    let library_dir = tempfile::tempdir().unwrap();
    let source_dir = tempfile::tempdir().unwrap();
    let library = Library::open(library_dir.path().to_path_buf()).unwrap();
    let source = library
        .create_import_source(NewImportSource {
            root_path: source_dir.path().display().to_string(),
            include_subdirectories: true,
            include_images: true,
            include_text: true,
            ai_allowed: false,
        })
        .unwrap();
    let item = library
        .insert_reference_item(&source.id, "../secret.txt", 1, 1, new_text())
        .unwrap();
    assert!(AssetReader::new(&library).resolve(&item.id).is_err());
}

#[test]
fn task_entries_page_and_discard_unfinished() {
    let dir = tempfile::tempdir().unwrap();
    let library = Library::open(dir.path().to_path_buf()).unwrap();
    let task = library
        .create_import_task(NewImportTask {
            source_id: None,
            kind: "files".into(),
            options_json: "{}".into(),
        })
        .unwrap();
    let entries = vec![ImportTaskEntry {
        rowid: 0,
        task_id: task.id.clone(),
        entry_key: "0".into(),
        relative_path: "a.txt".into(),
        display_name: "a.txt".into(),
        size: 1,
        status: "ready".into(),
        error_code: None,
        error_message: None,
        item_id: None,
    }];
    library
        .replace_import_task_entries(&task.id, &entries, 1)
        .unwrap();
    library
        .set_import_task_status(&task.id, ImportTaskStatus::Running)
        .unwrap();
    let page = library
        .list_import_task_entries(&task.id, None, 20)
        .unwrap();
    library
        .update_import_task_entry(page[0].rowid, "failed", None, Some("IO"), Some("失败"))
        .unwrap();
    assert_eq!(
        library.failed_import_task_entries(&task.id, 10).unwrap().len(),
        1
    );
    library.request_import_task_cancel(&task.id).unwrap();
    assert!(library.import_task_cancel_requested(&task.id).unwrap());

    // 未完成的任务在重启时被丢弃：任务与条目一起消失，库里不留导入历史。
    assert_eq!(library.discard_unfinished_import_tasks().unwrap(), 1);
    assert!(library.get_import_task(&task.id).unwrap().is_none());
    assert!(library
        .list_import_task_entries(&task.id, None, 20)
        .unwrap()
        .is_empty());
}

#[test]
fn deleting_an_import_task_drops_its_entries() {
    let dir = tempfile::tempdir().unwrap();
    let library = Library::open(dir.path().to_path_buf()).unwrap();
    let task = library
        .create_import_task(NewImportTask {
            source_id: None,
            kind: "files".into(),
            options_json: "{}".into(),
        })
        .unwrap();
    library
        .replace_import_task_entries(
            &task.id,
            &[ImportTaskEntry {
                rowid: 0,
                task_id: task.id.clone(),
                entry_key: "0".into(),
                relative_path: "a.txt".into(),
                display_name: "a.txt".into(),
                size: 1,
                status: "ready".into(),
                error_code: None,
                error_message: None,
                item_id: None,
            }],
            1,
        )
        .unwrap();

    library.delete_import_task(&task.id).unwrap();
    assert!(library.get_import_task(&task.id).unwrap().is_none());
    assert!(library.import_task_entry_paths(&task.id).unwrap().is_empty());
}

/// 目录来源按不带 `\\?\` 前缀的完整路径入库。
#[cfg(windows)]
#[test]
fn import_source_root_is_stored_without_extended_prefix() {
    let library_dir = tempfile::tempdir().unwrap();
    let source_dir = tempfile::tempdir().unwrap();
    let library = Library::open(library_dir.path().to_path_buf()).unwrap();
    let plain = source_dir.path().display().to_string();

    let source = library
        .create_import_source(NewImportSource {
            root_path: format!(r"\\?\{plain}"),
            include_subdirectories: true,
            include_images: true,
            include_text: true,
            ai_allowed: false,
        })
        .unwrap();
    assert_eq!(source.root_path, plain);
    assert!(!source.path_key.starts_with(r"\\?\"));
}

/// 同一目录只差末尾分隔符也算同一来源：复用，不新建一条。
#[test]
fn import_source_reuses_source_for_trailing_separator() {
    let library_dir = tempfile::tempdir().unwrap();
    let source_dir = tempfile::tempdir().unwrap();
    let library = Library::open(library_dir.path().to_path_buf()).unwrap();
    let plain = source_dir.path().display().to_string();
    let new_source = |root: String| NewImportSource {
        root_path: root,
        include_subdirectories: true,
        include_images: true,
        include_text: true,
        ai_allowed: false,
    };

    let first = library.create_import_source(new_source(plain.clone())).unwrap();
    let second = library
        .create_import_source(new_source(format!(
            "{plain}{}",
            std::path::MAIN_SEPARATOR
        )))
        .unwrap();
    assert_eq!(second.id, first.id);
    assert_eq!(second.root_path, plain);
}
