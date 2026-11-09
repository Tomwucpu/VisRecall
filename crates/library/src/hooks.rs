//! 元数据变更钩子（design §6.4 / tasks Task 9 Step 5）。
//!
//! 「备注/标签变了要重算索引」属于 pipeline 的责任，但写路径在 `library`。为了不让库层
//! 反向依赖 pipeline，这里只留一个回调注入点：
//!
//! ```text
//! app-core 启动时: library.set_metadata_hook(Some(Arc::new(|ids| { bump_revision + enqueue(index) })))
//! ```
//!
//! **纪律**：钩子必须在写事务提交后、连接 guard 已释放时调用（否则钩子里的数据库操作会
//! 自锁，长事务也会拖住其它写）。

use std::sync::Arc;

/// 元数据变更回调：参数是受影响的素材 id（标签改名/删除可能涉及多条）。
pub type MetadataHook = Arc<dyn Fn(&[String]) + Send + Sync>;

impl super::db::Library {
    /// 注册（或清除）元数据变更钩子。重复注册覆盖旧值。
    pub fn set_metadata_hook(&self, hook: Option<MetadataHook>) {
        if let Ok(mut slot) = self.metadata_hook.lock() {
            *slot = hook;
        }
    }

    /// 是否有钩子（测试断言用）。
    pub fn has_metadata_hook(&self) -> bool {
        self.metadata_hook
            .lock()
            .map(|slot| slot.is_some())
            .unwrap_or(false)
    }

    /// 通知钩子（幂等；无钩子时是空操作）。调用方必须已释放连接 guard。
    pub(crate) fn notify_metadata_changed(&self, ids: &[String]) {
        if ids.is_empty() {
            return;
        }
        let hook = match self.metadata_hook.lock() {
            Ok(slot) => slot.clone(),
            Err(_) => return,
        };
        if let Some(hook) = hook {
            hook(ids);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Library;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    use visrecall_domain::{ItemKind, NewItem};

    fn temp_library() -> (tempfile::TempDir, Library) {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::open(dir.path().to_path_buf()).unwrap();
        (dir, lib)
    }

    fn insert_item(lib: &Library) -> String {
        lib.insert_item(NewItem {
            kind: ItemKind::Image,
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
    fn note_change_fires_hook_after_commit() {
        let (_dir, lib) = temp_library();
        let item = insert_item(&lib);
        let calls = Arc::new(AtomicUsize::new(0));
        let ids: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        {
            let calls = Arc::clone(&calls);
            let ids = Arc::clone(&ids);
            lib.set_metadata_hook(Some(Arc::new(move |changed: &[String]| {
                calls.fetch_add(1, Ordering::SeqCst);
                // 钩子内部可以安全地读库（证明 guard 已释放）
                for id in changed {
                    ids.lock().unwrap().push(id.clone());
                }
            })));
        }
        assert!(lib.has_metadata_hook());

        lib.set_note(&item, "新备注").unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(ids.lock().unwrap().clone(), vec![item.clone()]);
        assert_eq!(lib.get_item(&item).unwrap().unwrap().note, "新备注");
    }

    #[test]
    fn tag_changes_fire_hook() {
        let (_dir, lib) = temp_library();
        let item = insert_item(&lib);
        let calls = Arc::new(AtomicUsize::new(0));
        {
            let calls = Arc::clone(&calls);
            lib.set_metadata_hook(Some(Arc::new(move |_ids: &[String]| {
                calls.fetch_add(1, Ordering::SeqCst);
            })));
        }

        let tag = lib.tag_add(&item, "工作").unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        lib.tag_detach(&item, &tag.tag.id).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        // 重新贴一个标签再改名：受影响素材集合要再次触发钩子
        let tag = lib.tag_add(&item, "工作").unwrap();
        let before = calls.load(Ordering::SeqCst);
        lib.tag_rename(&tag.tag.id, "工作2").unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), before + 1);
    }

    #[test]
    fn hook_can_be_cleared() {
        let (_dir, lib) = temp_library();
        let item = insert_item(&lib);
        let calls = Arc::new(AtomicUsize::new(0));
        {
            let calls = Arc::clone(&calls);
            lib.set_metadata_hook(Some(Arc::new(move |_ids: &[String]| {
                calls.fetch_add(1, Ordering::SeqCst);
            })));
        }
        lib.set_metadata_hook(None);
        lib.set_note(&item, "备注").unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}
