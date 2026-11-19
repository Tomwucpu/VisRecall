//! 主动导入的原生选择、短期令牌、任务查询与来源管理命令。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::{Emitter, Manager};
use visrecall_app_core::{AppCore, AppError, DirectoryScanOptions, ImportTaskEvent};
use visrecall_library::{
    ImportSource, ImportTask, ImportTaskEntry, ImportTaskStatus, NewImportTask,
};

use crate::ipc::{run_blocking, ApiError};

const SELECTION_TTL: Duration = Duration::from_secs(10 * 60);
const MAX_IMPORT_FILES: usize = 20;

#[derive(Clone, Debug)]
enum SelectionKind {
    Files(Vec<PathBuf>),
    Directory(PathBuf),
}

#[derive(Clone, Debug)]
struct Selection {
    window: String,
    kind: SelectionKind,
    expires_at: Instant,
}

#[derive(Default)]
pub struct ImportSelectionState {
    selections: Mutex<HashMap<String, Selection>>,
    targets: Mutex<HashMap<String, DropTargetDto>>,
}

impl ImportSelectionState {
    fn insert(&self, window: &str, kind: SelectionKind) -> String {
        let token = visrecall_domain::new_item_id();
        let mut selections = self
            .selections
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        selections.retain(|_, selection| selection.expires_at > Instant::now());
        selections.insert(
            token.clone(),
            Selection {
                window: window.into(),
                kind,
                expires_at: Instant::now() + SELECTION_TTL,
            },
        );
        token
    }

    fn take(&self, window: &str, token: &str) -> Result<Selection, ApiError> {
        let mut selections = self
            .selections
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let selection = selections
            .remove(token)
            .ok_or_else(|| ApiError::new("SELECTION_EXPIRED", "选择已提交或过期，请重新选择"))?;
        if selection.window != window || selection.expires_at <= Instant::now() {
            return Err(ApiError::new(
                "SELECTION_EXPIRED",
                "选择已提交或过期，请重新选择",
            ));
        }
        Ok(selection)
    }

    fn discard_window(&self, window: &str) {
        self.selections
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .retain(|_, selection| selection.window != window);
        self.targets
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .remove(window);
    }
}

#[derive(Clone, Debug, Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct ImportFileSummaryDto {
    pub entry_id: String,
    pub file_name: String,
    pub size: u64,
    pub supported: bool,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct FileSelectionDto {
    pub token: String,
    pub files: Vec<ImportFileSummaryDto>,
}

#[derive(Clone, Debug, Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct DirectorySelectionDto {
    pub token: String,
    pub path: String,
}

#[derive(Clone, Debug, Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct ImportTaskDto {
    pub id: String,
    pub source_id: Option<String>,
    pub kind: String,
    pub status: String,
    pub total_count: u32,
    pub processed_count: u32,
    pub succeeded_count: u32,
    pub skipped_count: u32,
    pub failed_count: u32,
    pub original_bytes: u64,
    pub cancel_requested: bool,
    pub error_summary: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl From<ImportTask> for ImportTaskDto {
    fn from(task: ImportTask) -> Self {
        Self {
            id: task.id,
            source_id: task.source_id,
            kind: task.kind,
            status: task.status.as_str().into(),
            total_count: task.total_count,
            processed_count: task.processed_count,
            succeeded_count: task.succeeded_count,
            skipped_count: task.skipped_count,
            failed_count: task.failed_count,
            original_bytes: task.original_bytes,
            cancel_requested: task.cancel_requested,
            error_summary: task.error_summary,
            created_at: task.created_at,
            updated_at: task.updated_at,
        }
    }
}

#[derive(Clone, Debug, Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct ImportTaskEntryDto {
    pub rowid: i64,
    pub entry_key: String,
    pub relative_path: String,
    pub display_name: String,
    pub size: u64,
    pub status: String,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub item_id: Option<String>,
}

impl From<ImportTaskEntry> for ImportTaskEntryDto {
    fn from(entry: ImportTaskEntry) -> Self {
        Self {
            rowid: entry.rowid,
            entry_key: entry.entry_key,
            relative_path: entry.relative_path,
            display_name: entry.display_name,
            size: entry.size,
            status: entry.status,
            error_code: entry.error_code,
            error_message: entry.error_message,
            item_id: entry.item_id,
        }
    }
}

#[derive(Clone, Debug, Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct ImportTaskEntriesPageDto {
    pub entries: Vec<ImportTaskEntryDto>,
    pub next_cursor: Option<i64>,
}

#[derive(Clone, Debug, Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct ImportSourceDto {
    pub id: String,
    pub root_path: String,
    pub include_subdirectories: bool,
    pub include_images: bool,
    pub include_text: bool,
    pub status: String,
    pub last_scanned_at: Option<i64>,
    pub item_count: u64,
}

impl From<ImportSource> for ImportSourceDto {
    fn from(source: ImportSource) -> Self {
        Self {
            id: source.id,
            root_path: source.root_path,
            include_subdirectories: source.include_subdirectories,
            include_images: source.include_images,
            include_text: source.include_text,
            status: source.status.as_str().into(),
            last_scanned_at: source.last_scanned_at,
            item_count: source.item_count,
        }
    }
}

#[derive(Clone, Debug, Deserialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryScanOptionsDto {
    pub include_subdirectories: bool,
    pub include_images: bool,
    pub include_text: bool,
}

impl From<DirectoryScanOptionsDto> for DirectoryScanOptions {
    fn from(options: DirectoryScanOptionsDto) -> Self {
        Self {
            include_subdirectories: options.include_subdirectories,
            include_images: options.include_images,
            include_text: options.include_text,
        }
    }
}

fn summaries(paths: &[PathBuf]) -> Vec<ImportFileSummaryDto> {
    AppCore::inspect_import_files(paths)
        .into_iter()
        .map(|file| ImportFileSummaryDto {
            entry_id: file.entry_id,
            file_name: file.file_name,
            size: file.size,
            supported: file.supported,
            reason: file.reason,
        })
        .collect()
}

#[tauri::command]
#[specta::specta]
pub async fn pick_import_files(
    window: tauri::Window,
) -> Result<Option<FileSelectionDto>, ApiError> {
    use tauri_plugin_dialog::DialogExt;
    let (tx, rx) = std::sync::mpsc::channel();
    window
        .app_handle()
        .dialog()
        .file()
        .add_filter(
            "支持的素材",
            &["png", "jpg", "jpeg", "webp", "gif", "txt", "md"],
        )
        .pick_files(move |paths| {
            let _ = tx.send(paths);
        });
    let picked = tauri::async_runtime::spawn_blocking(move || {
        rx.recv_timeout(Duration::from_secs(600)).ok().flatten()
    })
    .await
    .map_err(|err| ApiError::new("INTERNAL", err.to_string()))?;
    let Some(paths) = picked else { return Ok(None) };
    let paths: Vec<_> = paths
        .into_iter()
        .filter_map(|path| path.into_path().ok())
        .collect();
    if paths.is_empty() {
        return Ok(None);
    }
    if paths.len() > MAX_IMPORT_FILES {
        return Err(ApiError::new("INVALID_INPUT", "每批最多选择 20 个文件"));
    }
    let files = summaries(&paths);
    let state = window.state::<ImportSelectionState>();
    let token = state.insert(window.label(), SelectionKind::Files(paths));
    Ok(Some(FileSelectionDto { token, files }))
}

#[tauri::command]
#[specta::specta]
pub async fn pick_import_directory(
    window: tauri::Window,
) -> Result<Option<DirectorySelectionDto>, ApiError> {
    use tauri_plugin_dialog::DialogExt;
    let (tx, rx) = std::sync::mpsc::channel();
    window
        .app_handle()
        .dialog()
        .file()
        .pick_folder(move |path| {
            let _ = tx.send(path);
        });
    let picked = tauri::async_runtime::spawn_blocking(move || {
        rx.recv_timeout(Duration::from_secs(600)).ok().flatten()
    })
    .await
    .map_err(|err| ApiError::new("INTERNAL", err.to_string()))?;
    let Some(path) = picked.and_then(|path| path.into_path().ok()) else {
        return Ok(None);
    };
    let display = path.display().to_string();
    let state = window.state::<ImportSelectionState>();
    let token = state.insert(window.label(), SelectionKind::Directory(path));
    Ok(Some(DirectorySelectionDto {
        token,
        path: display,
    }))
}

#[tauri::command]
#[specta::specta]
pub fn discard_import_selection(window: tauri::Window) {
    window
        .state::<ImportSelectionState>()
        .discard_window(window.label());
}

#[tauri::command]
#[specta::specta]
pub async fn start_file_import(
    window: tauri::Window,
    token: String,
    entry_ids: Vec<String>,
) -> Result<ImportTaskDto, ApiError> {
    let selection = window
        .state::<ImportSelectionState>()
        .take(window.label(), &token)?;
    let SelectionKind::Files(paths) = selection.kind else {
        return Err(ApiError::new("INVALID_INPUT", "该选择不是文件批次"));
    };
    let selected: Vec<_> = paths.iter().cloned().zip(summaries(&paths)).collect();
    let selected: Vec<_> = selected
        .into_iter()
        .filter(|(_, summary)| entry_ids.contains(&summary.entry_id) && summary.supported)
        .collect();
    if selected.is_empty() || selected.len() > MAX_IMPORT_FILES {
        return Err(ApiError::new(
            "INVALID_INPUT",
            "请选择 1 至 20 个支持的文件",
        ));
    }
    let app = window.app_handle().clone();
    let task = run_blocking(app.clone(), move |core| {
        let library = core.library_arc().map_err(|err| ApiError::from(&err))?;
        let task = library
            .create_import_task(NewImportTask {
                source_id: None,
                kind: "files".into(),
                options_json: "{}".into(),
            })
            .map_err(|err| ApiError::from(&err))?;
        for ((path, summary), index) in selected.iter().zip(0..) {
            library
                .append_import_task_entry(&ImportTaskEntry {
                    rowid: 0,
                    task_id: task.id.clone(),
                    entry_key: index.to_string(),
                    display_name: summary.file_name.clone(),
                    relative_path: path.display().to_string(),
                    size: summary.size,
                    status: "ready".into(),
                    error_code: None,
                    error_message: None,
                    item_id: None,
                })
                .map_err(|err| ApiError::from(&err))?;
        }
        library
            .recount_import_task(&task.id)
            .map_err(|err| ApiError::from(&err))?;
        get_task(core, &task.id)
    })
    .await?;
    spawn_import_worker(app, task.id.clone(), run_file_task);
    Ok(task)
}

fn run_file_task(core: &AppCore, task_id: &str) -> Result<ImportTaskDto, ApiError> {
    let library = core.library_arc().map_err(|err| ApiError::from(&err))?;
    library
        .set_import_task_status(task_id, ImportTaskStatus::Running)
        .map_err(|err| ApiError::from(&err))?;
    let entries = library
        .ready_import_task_entries(task_id)
        .map_err(|err| ApiError::from(&err))?;
    for entry in entries {
        if library
            .import_task_cancel_requested(task_id)
            .map_err(|err| ApiError::from(&err))?
        {
            break;
        }
        let result = core.import_files(&[PathBuf::from(&entry.relative_path)]);
        match result {
            Ok(report) if !report.items.is_empty() => {
                library
                    .update_import_task_entry(
                        entry.rowid,
                        "imported",
                        Some(&report.items[0].id),
                        None,
                        None,
                    )
                    .map_err(|err| ApiError::from(&err))?;
            }
            Ok(report) => {
                let message = report
                    .failures
                    .first()
                    .map(|failure| failure.1.clone())
                    .unwrap_or_else(|| "文件未导入".into());
                library
                    .update_import_task_entry(
                        entry.rowid,
                        "failed",
                        None,
                        Some("IMPORT_FAILED"),
                        Some(&message),
                    )
                    .map_err(|err| ApiError::from(&err))?;
            }
            Err(err) => {
                library
                    .update_import_task_entry(
                        entry.rowid,
                        "failed",
                        None,
                        Some("IMPORT_FAILED"),
                        Some(&err.to_string()),
                    )
                    .map_err(|err| ApiError::from(&err))?;
            }
        }
        core.sink()
            .emit(visrecall_app_core::AppEvent::ImportTaskProgress(
                ImportTaskEvent { id: task_id.into() },
            ));
    }
    let current = library
        .get_import_task(task_id)
        .map_err(|err| ApiError::from(&err))?
        .ok_or(AppError::NotFound)
        .map_err(|err| ApiError::from(&err))?;
    let cancelled = library
        .import_task_cancel_requested(task_id)
        .map_err(|err| ApiError::from(&err))?;
    let status = if cancelled {
        ImportTaskStatus::Cancelled
    } else if current.failed_count > 0 {
        ImportTaskStatus::CompletedWithErrors
    } else {
        ImportTaskStatus::Completed
    };
    library
        .set_import_task_status(task_id, status)
        .map_err(|err| ApiError::from(&err))?;
    let finished = get_task(core, task_id)?;
    // 终态结果随事件推给前端，任务行随即删除：库里不保留导入历史。
    visrecall_app_core::finish_import_task(&library, core.sink().as_ref(), task_id);
    Ok(finished)
}

fn get_task(core: &AppCore, task_id: &str) -> Result<ImportTaskDto, ApiError> {
    core.library_arc()
        .map_err(|err| ApiError::from(&err))?
        .get_import_task(task_id)
        .map_err(|err| ApiError::from(&err))?
        .map(ImportTaskDto::from)
        .ok_or_else(|| ApiError::new("NOT_FOUND", "导入任务不存在"))
}

/// IPC 只负责任务入队；真正的扫描和导入在独立 blocking 线程执行。
fn spawn_import_worker<F>(app: tauri::AppHandle, task_id: String, worker: F)
where
    F: FnOnce(&AppCore, &str) -> Result<ImportTaskDto, ApiError> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(move || {
        let core = app.state::<AppCore>();
        if let Err(err) = worker(&core, &task_id) {
            tracing::error!(task = %task_id, code = %err.code, message = %err.message, "导入后台任务失败");
            match core.library_arc() {
                Ok(library) => {
                    let should_fail = library
                        .get_import_task(&task_id)
                        .ok()
                        .flatten()
                        .is_some_and(|task| {
                            matches!(
                                task.status,
                                ImportTaskStatus::Pending | ImportTaskStatus::Running
                            )
                        });
                    if should_fail {
                        let _ = library.set_import_task_error(&task_id, &err.message);
                    }
                    visrecall_app_core::finish_import_task(&library, core.sink().as_ref(), &task_id);
                }
                // 库都打不开时任务行不存在；仍要发终态事件，避免前端一直等。
                Err(_) => core.sink().emit(visrecall_app_core::AppEvent::ImportTaskFinished(
                    visrecall_app_core::ImportTaskFinishedEvent {
                        id: task_id.clone(),
                        task: None,
                        failures: Vec::new(),
                    },
                )),
            }
        }
    });
}

#[tauri::command]
#[specta::specta]
pub async fn create_text_item(
    app: tauri::AppHandle,
    title: Option<String>,
    body: String,
    tags: Vec<String>,
) -> Result<String, ApiError> {
    run_blocking(app, move |core| {
        core.create_text_item(title.as_deref(), &body, &tags)
            .map(|item| item.id)
            .map_err(|err| ApiError::from(&err))
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn scan_import_directory(
    window: tauri::Window,
    token: String,
    options: DirectoryScanOptionsDto,
) -> Result<ImportTaskDto, ApiError> {
    let selection = window
        .state::<ImportSelectionState>()
        .take(window.label(), &token)?;
    let SelectionKind::Directory(path) = selection.kind else {
        return Err(ApiError::new("INVALID_INPUT", "该选择不是目录"));
    };
    let app = window.app_handle().clone();
    let task = run_blocking(app.clone(), move |core| {
        core.directory_imports()
            .map_err(|err| ApiError::from(&err))?
            .prepare_scan(&path, options.into())
            .map(ImportTaskDto::from)
            .map_err(|err| ApiError::from(&err))
    })
    .await?;
    spawn_import_worker(app, task.id.clone(), |core, id| {
        core.directory_imports()
            .map_err(|err| ApiError::from(&err))?
            .run_scan(id)
            .map(ImportTaskDto::from)
            .map_err(|err| ApiError::from(&err))
    });
    Ok(task)
}

#[tauri::command]
#[specta::specta]
pub async fn start_directory_import(
    app: tauri::AppHandle,
    scan_task_id: String,
) -> Result<ImportTaskDto, ApiError> {
    let task = run_blocking(app.clone(), move |core| {
        core.directory_imports()
            .map_err(|err| ApiError::from(&err))?
            .prepare_import(&scan_task_id)
            .map(ImportTaskDto::from)
            .map_err(|err| ApiError::from(&err))
    })
    .await?;
    spawn_import_worker(app, task.id.clone(), |core, id| {
        core.directory_imports()
            .map_err(|err| ApiError::from(&err))?
            .run_import_task(id)
            .map(ImportTaskDto::from)
            .map_err(|err| ApiError::from(&err))
    });
    Ok(task)
}

#[tauri::command]
#[specta::specta]
pub async fn get_import_task(app: tauri::AppHandle, id: String) -> Result<ImportTaskDto, ApiError> {
    run_blocking(app, move |core| get_task(core, &id)).await
}

#[tauri::command]
#[specta::specta]
pub async fn list_import_tasks(
    app: tauri::AppHandle,
    limit: Option<u32>,
) -> Result<Vec<ImportTaskDto>, ApiError> {
    run_blocking(app, move |core| {
        core.library_arc()
            .map_err(|err| ApiError::from(&err))?
            .list_import_tasks(limit.unwrap_or(20).clamp(1, 100))
            .map(|tasks| tasks.into_iter().map(ImportTaskDto::from).collect())
            .map_err(|err| ApiError::from(&err))
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn list_import_task_entries(
    app: tauri::AppHandle,
    task_id: String,
    cursor: Option<i64>,
    limit: Option<u32>,
) -> Result<ImportTaskEntriesPageDto, ApiError> {
    run_blocking(app, move |core| {
        let limit = limit.unwrap_or(100).clamp(1, 200);
        let entries = core
            .library_arc()
            .map_err(|err| ApiError::from(&err))?
            .list_import_task_entries(&task_id, cursor, limit)
            .map_err(|err| ApiError::from(&err))?;
        let next_cursor = if entries.len() == limit as usize {
            entries.last().map(|entry| entry.rowid)
        } else {
            None
        };
        Ok(ImportTaskEntriesPageDto {
            entries: entries.into_iter().map(ImportTaskEntryDto::from).collect(),
            next_cursor,
        })
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn cancel_import_task(
    app: tauri::AppHandle,
    id: String,
) -> Result<ImportTaskDto, ApiError> {
    run_blocking(app, move |core| {
        core.library_arc()
            .map_err(|err| ApiError::from(&err))?
            .request_import_task_cancel(&id)
            .map_err(|err| ApiError::from(&err))?;
        get_task(core, &id)
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn list_import_sources(app: tauri::AppHandle) -> Result<Vec<ImportSourceDto>, ApiError> {
    run_blocking(app, move |core| {
        core.library_arc()
            .map_err(|err| ApiError::from(&err))?
            .list_import_sources()
            .map(|sources| sources.into_iter().map(ImportSourceDto::from).collect())
            .map_err(|err| ApiError::from(&err))
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn refresh_import_source(
    app: tauri::AppHandle,
    id: String,
) -> Result<ImportTaskDto, ApiError> {
    let task = run_blocking(app.clone(), move |core| {
        core.directory_imports()
            .map_err(|err| ApiError::from(&err))?
            .prepare_refresh(&id)
            .map(ImportTaskDto::from)
            .map_err(|err| ApiError::from(&err))
    })
    .await?;
    spawn_import_worker(app, task.id.clone(), |core, id| {
        core.directory_imports()
            .map_err(|err| ApiError::from(&err))?
            .run_refresh(id)
            .map(ImportTaskDto::from)
            .map_err(|err| ApiError::from(&err))
    });
    Ok(task)
}

#[tauri::command]
#[specta::specta]
pub async fn relocate_import_source(
    window: tauri::Window,
    id: String,
    token: String,
) -> Result<ImportSourceDto, ApiError> {
    let selection = window
        .state::<ImportSelectionState>()
        .take(window.label(), &token)?;
    let SelectionKind::Directory(path) = selection.kind else {
        return Err(ApiError::new("INVALID_INPUT", "该选择不是目录"));
    };
    run_blocking(window.app_handle().clone(), move |core| {
        core.directory_imports()
            .map_err(|err| ApiError::from(&err))?
            .relocate(&id, &path)
            .map(ImportSourceDto::from)
            .map_err(|err| ApiError::from(&err))
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn remove_import_source(
    app: tauri::AppHandle,
    id: String,
    remove_items: bool,
) -> Result<u64, ApiError> {
    run_blocking(app, move |core| {
        core.directory_imports()
            .map_err(|err| ApiError::from(&err))?
            .remove(&id, remove_items)
            .map_err(|err| ApiError::from(&err))
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn open_reference_file(
    app: tauri::AppHandle,
    item_id: String,
    reveal: Option<bool>,
) -> Result<(), ApiError> {
    let path: PathBuf = run_blocking(app.clone(), move |core| {
        let library = core.library().map_err(ApiError::from)?;
        let asset = visrecall_library::AssetReader::new(library)
            .resolve(&item_id)
            .map_err(|err| ApiError::from(&err))?;
        if asset.storage_mode != visrecall_domain::item::StorageMode::Reference {
            return Err(ApiError::new("INVALID_INPUT", "该素材不是引用文件"));
        }
        Ok(asset.path)
    })
    .await?;
    let target = if reveal.unwrap_or(false) {
        path.parent().unwrap_or(Path::new(&path)).to_path_buf()
    } else {
        path
    };
    tauri::async_runtime::spawn_blocking(move || {
        use tauri_plugin_opener::OpenerExt;
        app.opener()
            .open_path(target.display().to_string(), None::<&str>)
            .map_err(|err| ApiError::new("INTERNAL", format!("无法打开原文件：{err}")))
    })
    .await
    .map_err(|err| ApiError::new("INTERNAL", err.to_string()))?
}

#[derive(Clone, Copy, Debug, Deserialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct DropTargetDto {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[tauri::command]
#[specta::specta]
pub fn set_import_drop_target(window: tauri::Window, target: Option<DropTargetDto>) {
    let state = window.state::<ImportSelectionState>();
    let mut targets = state.targets.lock().unwrap_or_else(|err| err.into_inner());
    if let Some(target) = target {
        targets.insert(window.label().into(), target);
    } else {
        targets.remove(window.label());
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DropHoverPayload {
    over_target: bool,
}

/// 原生拖放只把受限弹窗区域转换为选择令牌，路径不会发送到 WebView。
pub fn handle_drag_drop(window: &tauri::Window, event: tauri::DragDropEvent) {
    use tauri::DragDropEvent;
    match event {
        DragDropEvent::Enter { position, .. } | DragDropEvent::Over { position } => {
            let over_target = over_target(window, position.x, position.y);
            let _ = window.emit("import_drop_hover", DropHoverPayload { over_target });
        }
        DragDropEvent::Drop { paths, position } => {
            let _ = window.emit("import_drop_hover", DropHoverPayload { over_target: false });
            if !over_target(window, position.x, position.y) || paths.len() > MAX_IMPORT_FILES {
                return;
            }
            let files = summaries(&paths);
            if files.is_empty() {
                return;
            }
            let token = window
                .state::<ImportSelectionState>()
                .insert(window.label(), SelectionKind::Files(paths));
            let _ = window.emit("import_drop_ready", FileSelectionDto { token, files });
        }
        DragDropEvent::Leave => {
            let _ = window.emit("import_drop_hover", DropHoverPayload { over_target: false });
        }
        _ => {}
    }
}

fn over_target(window: &tauri::Window, x: f64, y: f64) -> bool {
    let state = window.state::<ImportSelectionState>();
    let targets = state.targets.lock().unwrap_or_else(|err| err.into_inner());
    let Some(target) = targets.get(window.label()) else {
        return false;
    };
    let scale = window.scale_factor().unwrap_or(1.0);
    let logical_x = x / scale;
    let logical_y = y / scale;
    logical_x >= target.x
        && logical_y >= target.y
        && logical_x < target.x + target.width
        && logical_y < target.y + target.height
}
