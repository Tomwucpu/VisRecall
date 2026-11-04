//! 应用编排核心：持有库句柄、配置与事件发送端。
//!
//! 本模块不依赖 Tauri；`src-tauri` 只负责把命令参数/返回值映射成 DTO。
//! 库打开失败不会 panic：错误保存在 [`AppCore::library`] 里，由命令层转成
//! `CONFIG_MISSING` 等稳定错误码。

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use visrecall_domain::AppError;
use visrecall_library::paths::AppPaths;
use visrecall_library::{AppConfig, Library};

use crate::events::{AppEvent, AppRecoveredEvent, EventSink};

/// Tauri 托管的应用状态。
pub struct AppCore {
    paths: AppPaths,
    /// 库打开失败时保留错误；后续命令据此返回错误码。
    library: Result<Library, AppError>,
    config: Mutex<AppConfig>,
    sink: Arc<dyn EventSink>,
}

impl AppCore {
    /// 加载配置 → 打开库 → 恢复遗留任务。
    ///
    /// - 配置缺失/损坏由 [`AppConfig::load_or_default`] 兜底为默认值。
    /// - 库打开失败只记录日志并把错误留在结构体里，主窗口据此显示错误状态。
    /// - 恢复任务数 > 0 时发出 [`AppEvent::AppRecovered`]。
    pub fn open(paths: AppPaths, sink: Arc<dyn EventSink>) -> AppCore {
        let config = AppConfig::load_or_default(&paths.config_file);
        let library_path = config.library_path.clone();
        tracing::info!(
            config = %paths.config_file.display(),
            library = %library_path.display(),
            "opening app core"
        );

        let library = match Library::open(library_path.clone()) {
            Ok(library) => {
                match library.recover_jobs() {
                    Ok(0) => {}
                    Ok(count) => {
                        tracing::info!(count, "recovered unfinished jobs");
                        sink.emit(AppEvent::AppRecovered(AppRecoveredEvent { count }));
                    }
                    Err(err) => {
                        tracing::warn!(error = %err, "failed to recover jobs");
                    }
                }
                Ok(library)
            }
            Err(err) => {
                tracing::error!(
                    library = %library_path.display(),
                    error = %err,
                    "failed to open library"
                );
                Err(err)
            }
        };

        AppCore {
            paths,
            library,
            config: Mutex::new(config),
            sink,
        }
    }

    pub fn paths(&self) -> &AppPaths {
        &self.paths
    }

    /// 库句柄；打开失败时返回启动时的错误。
    pub fn library(&self) -> Result<&Library, &AppError> {
        self.library.as_ref()
    }

    pub fn library_ok(&self) -> bool {
        self.library.is_ok()
    }

    /// 库目录：优先取已打开的库根目录，库不可用时退回配置里的路径
    /// （便于用户手工检查损坏的库）。
    pub fn library_root(&self) -> PathBuf {
        match &self.library {
            Ok(library) => library.root().to_path_buf(),
            Err(_) => self.config().library_path,
        }
    }

    /// 当前配置快照；命令层只读取，不直接改写。
    pub fn config(&self) -> AppConfig {
        self.lock_config().clone()
    }

    /// 事件发送端，供后续任务（捕获流程）复用。
    pub fn sink(&self) -> &Arc<dyn EventSink> {
        &self.sink
    }

    /// 应用设置补丁：先写盘再更新内存，避免写失败后内存与磁盘不一致。
    ///
    /// 阶段 01 不支持快捷键热重载：`capture_hotkey` 与当前值不同时整体拒绝
    /// （不保存任何字段，保留旧值）。
    pub fn update_settings(
        &self,
        ball_enabled: Option<bool>,
        ball_size: Option<u32>,
        capture_hotkey: Option<String>,
    ) -> Result<AppConfig, AppError> {
        let mut guard = self.lock_config();

        if let Some(hotkey) = capture_hotkey.as_deref() {
            if hotkey != guard.hotkeys.capture {
                return Err(AppError::InvalidInput("restart required".into()));
            }
        }

        let mut next = guard.clone();
        if let Some(enabled) = ball_enabled {
            next.ball.enabled = enabled;
        }
        if let Some(size) = ball_size {
            next.ball.size = size;
        }

        next.save_atomic(&self.paths.config_file)?;
        *guard = next.clone();
        tracing::info!(config = %self.paths.config_file.display(), "settings saved");
        Ok(next)
    }

    fn lock_config(&self) -> MutexGuard<'_, AppConfig> {
        // 持锁期间没有会 panic 的操作；被毒化时也继续使用，避免启动后永久不可用。
        self.config
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use visrecall_domain::ItemKind;
    use visrecall_library::AppConfig;

    use crate::events::RecordingSink;

    fn paths_in(dir: &std::path::Path) -> AppPaths {
        AppPaths {
            config_file: dir.join("config.json"),
            default_library_root: dir.join("library"),
        }
    }

    /// 写入指向 `library` 的配置，避免测试依赖真实用户目录。
    fn seed_config(paths: &AppPaths) -> AppConfig {
        let mut config = AppConfig::default();
        config.library_path = paths.default_library_root.clone();
        config
            .save_atomic(&paths.config_file)
            .expect("write test config");
        config
    }

    fn open(dir: &std::path::Path) -> (AppCore, Arc<RecordingSink>) {
        let paths = paths_in(dir);
        seed_config(&paths);
        let sink = Arc::new(RecordingSink::default());
        (AppCore::open(paths, sink.clone()), sink)
    }

    #[test]
    fn open_creates_library_and_reads_config() {
        let dir = tempfile::tempdir().unwrap();
        let (core, sink) = open(dir.path());

        assert!(core.library_ok());
        assert!(core.library().is_ok());
        assert_eq!(
            core.library_root(),
            dir.path().join("library").to_path_buf()
        );
        assert!(core.config().library_path.ends_with("library"));
        assert!(sink.events().is_empty());
    }

    #[test]
    fn open_recovers_unfinished_jobs_and_emits_event() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        seed_config(&paths);

        {
            let library = Library::open(paths.default_library_root.clone()).unwrap();
            let item = library
                .insert_item(visrecall_domain::NewItem {
                    kind: ItemKind::Text,
                    captured_at: visrecall_domain::now_millis(),
                    source_app: None,
                    source_title: None,
                    source_url: None,
                    width: None,
                    height: None,
                    char_count: Some(3),
                    original_path: "texts/a.txt".into(),
                    content_hash: "deadbeef".into(),
                })
                .unwrap();
            library.enqueue_job(&item.id, "thumbnail", 1).unwrap();
            library
                .mark_job_running_for_test(&item.id, "thumbnail")
                .unwrap();
        }

        let sink = Arc::new(RecordingSink::default());
        let core = AppCore::open(paths, sink.clone());
        assert!(core.library_ok());

        let events = sink.events();
        assert_eq!(events.len(), 1);
        assert!(matches!(
            events[0],
            AppEvent::AppRecovered(AppRecoveredEvent { count: 1 })
        ));
    }

    #[test]
    fn open_keeps_error_when_library_path_is_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        let blocked = dir.path().join("blocked");
        std::fs::write(&blocked, b"not a directory").unwrap();

        let mut config = AppConfig::default();
        config.library_path = blocked;
        config.save_atomic(&paths.config_file).unwrap();

        let core = AppCore::open(paths, Arc::new(RecordingSink::default()));
        assert!(!core.library_ok());
        assert!(matches!(core.library(), Err(AppError::Io(_))));
        // 库不可用时命令层仍能拿到路径用于展示。
        assert!(core.library_root().ends_with("blocked"));
    }

    #[test]
    fn update_settings_persists_ball_fields() {
        let dir = tempfile::tempdir().unwrap();
        let (core, _sink) = open(dir.path());

        let updated = core.update_settings(Some(false), Some(64), None).unwrap();
        assert!(!updated.ball.enabled);
        assert_eq!(updated.ball.size, 64);

        let reloaded = AppConfig::load_or_default(&core.paths().config_file);
        assert!(!reloaded.ball.enabled);
        assert_eq!(reloaded.ball.size, 64);
    }

    #[test]
    fn update_settings_rejects_hotkey_change_without_saving() {
        let dir = tempfile::tempdir().unwrap();
        let (core, _sink) = open(dir.path());
        let before = core.config();

        let err = core
            .update_settings(Some(false), Some(64), Some("Ctrl+Alt+X".into()))
            .unwrap_err();
        assert!(matches!(err, AppError::InvalidInput(_)));
        assert_eq!(err.to_string(), "invalid input: restart required");

        // 整个补丁被拒绝：球的字段也没有写入。
        let after = core.config();
        assert_eq!(after.ball, before.ball);
        let reloaded = AppConfig::load_or_default(&core.paths().config_file);
        assert_eq!(reloaded.ball, before.ball);
    }

    #[test]
    fn update_settings_accepts_unchanged_hotkey() {
        let dir = tempfile::tempdir().unwrap();
        let (core, _sink) = open(dir.path());
        let hotkey = core.config().hotkeys.capture;

        let updated = core
            .update_settings(Some(true), Some(56), Some(hotkey))
            .unwrap();
        assert_eq!(updated.ball.size, 56);
    }
}
