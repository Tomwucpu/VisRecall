//! 目录扫描、只读引用导入与手动刷新。

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use visrecall_domain::item::{Item, ItemKind, NewItem};
use visrecall_domain::{now_millis, AppError};
use visrecall_library::import_sources::{path_identity, ExternalAsset};
use visrecall_library::{
    validate_relative_path, ImportSource, ImportTask, ImportTaskEntry, ImportTaskStatus, Library,
    NewImportSource, NewImportTask, SourceStatus,
};

use crate::events::{
    AppEvent, EventSink, ImportTaskEvent, ImportTaskFinishedEvent, ItemEvent, ItemUpdatedEvent,
    MAX_FINISHED_FAILURES,
};
use crate::import_flow::{decode_imported_text, MAX_IMAGE_BYTES, MAX_TEXT_BYTES};

pub const MAX_DIRECTORY_FILES: usize = 50_000;
pub const MAX_DIRECTORY_DEPTH: usize = 32;
const SKIPPED_DIRECTORIES: [&str; 6] = [
    ".git",
    "node_modules",
    "target",
    "dist",
    "build",
    ".visrecall",
];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryScanOptions {
    pub include_subdirectories: bool,
    pub include_images: bool,
    pub include_text: bool,
}

impl Default for DirectoryScanOptions {
    fn default() -> Self {
        Self {
            include_subdirectories: true,
            include_images: true,
            include_text: true,
        }
    }
}

#[derive(Clone)]
struct Candidate {
    relative_path: String,
    display_name: String,
    size: u64,
    /// 文件修改时间（毫秒）；只用于扫描阶段判断“内容有没有变”，不写库。
    modified_at: i64,
    status: String,
    error_code: Option<String>,
    error_message: Option<String>,
}

/// 扫描阶段标记「已导入且未删除」时写入的 `error_code`。
pub const ALREADY_IMPORTED: &str = "ALREADY_IMPORTED";

/// 推送终态快照，并删除不再需要的任务行。
///
/// 导入任务不保留历史：任务一进终态就删行（条目由外键级联删除），所以终态计数与
/// 失败明细必须先随事件推给前端。唯一例外是 `scanned` 状态的目录扫描任务——它是
/// 「关闭弹窗后继续确认扫描结果」的载体，会在下次扫描或确认导入时作废。
pub fn finish_import_task(library: &Library, sink: &dyn EventSink, task_id: &str) {
    let task = match library.get_import_task(task_id) {
        Ok(Some(task)) => task,
        Ok(None) => {
            sink.emit(AppEvent::ImportTaskFinished(ImportTaskFinishedEvent {
                id: task_id.into(),
                task: None,
                failures: Vec::new(),
            }));
            return;
        }
        Err(err) => {
            tracing::warn!(task = %task_id, %err, "读取导入任务终态失败");
            sink.emit(AppEvent::ImportTaskFinished(ImportTaskFinishedEvent {
                id: task_id.into(),
                task: None,
                failures: Vec::new(),
            }));
            return;
        }
    };
    let failures = if task.failed_count > 0 {
        library
            .failed_import_task_entries(task_id, MAX_FINISHED_FAILURES as u32)
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let keep = task.kind == "directory_scan" && task.status == ImportTaskStatus::Scanned;
    sink.emit(AppEvent::ImportTaskFinished(ImportTaskFinishedEvent {
        id: task_id.into(),
        task: Some(task),
        failures,
    }));
    if !keep {
        if let Err(err) = library.delete_import_task(task_id) {
            tracing::warn!(task = %task_id, %err, "导入任务清理失败");
        }
    }
}

#[derive(Default)]
struct ScanStats {
    total_count: u32,
    skipped_count: u32,
    original_bytes: u64,
}

impl ScanStats {
    fn record(&mut self, candidate: &Candidate) {
        self.total_count = self.total_count.saturating_add(1);
        if candidate.status == "skipped" {
            self.skipped_count = self.skipped_count.saturating_add(1);
        } else {
            self.original_bytes = self.original_bytes.saturating_add(candidate.size);
        }
    }
}

pub struct DirectoryImportService {
    library: Arc<Library>,
    sink: Arc<dyn EventSink>,
}

impl DirectoryImportService {
    pub fn new(library: Arc<Library>, sink: Arc<dyn EventSink>) -> Self {
        Self { library, sink }
    }

    /// 只创建来源和任务，让 IPC 可以立即返回任务 ID。
    pub fn prepare_scan(
        &self,
        root: &Path,
        options: DirectoryScanOptions,
    ) -> Result<ImportTask, AppError> {
        if !options.include_images && !options.include_text {
            return Err(AppError::InvalidInput("至少选择图片或文本中的一种".into()));
        }
        let root = validate_new_root(&self.library, root, None)?;
        let source = self.library.create_import_source(NewImportSource {
            root_path: root.display().to_string(),
            include_subdirectories: options.include_subdirectories,
            include_images: options.include_images,
            include_text: options.include_text,
        })?;
        // 重新扫描同一目录时，上一次没确认的扫描结果立即作废，不再留在库里。
        self.library.delete_directory_scan_tasks(&source.id)?;
        let task = self.library.create_import_task(NewImportTask {
            source_id: Some(source.id.clone()),
            kind: "directory_scan".into(),
            options_json: serde_json::to_string(&options).unwrap_or_else(|_| "{}".into()),
        })?;
        Ok(task)
    }

    pub fn run_scan(&self, task_id: &str) -> Result<ImportTask, AppError> {
        let result = (|| {
            let task = self.task(task_id)?;
            let source = self.source(task.source_id.as_deref().ok_or(AppError::NotFound)?)?;
            let root = validate_existing_root(&self.library, Path::new(&source.root_path))?;
            let options: DirectoryScanOptions = serde_json::from_str(&task.options_json)
                .map_err(|err| AppError::InvalidInput(format!("目录扫描选项无效：{err}")))?;
            self.scan_into(&task, &root, &options)
        })();
        if let Err(err) = &result {
            self.fail_task(task_id, err);
        }
        finish_import_task(&self.library, self.sink.as_ref(), task_id);
        result
    }

    pub fn scan(&self, root: &Path, options: DirectoryScanOptions) -> Result<ImportTask, AppError> {
        let task = self.prepare_scan(root, options)?;
        self.run_scan(&task.id)
    }

    /// 从已完成的扫描复制可导入项，后台执行由 `run_import_task` 负责。
    pub fn prepare_import(&self, scan_task_id: &str) -> Result<ImportTask, AppError> {
        let scan = self.task(scan_task_id)?;
        if !matches!(scan.kind.as_str(), "directory_scan" | "refresh") {
            return Err(AppError::InvalidInput("该任务不是目录扫描".into()));
        }
        if scan.status != ImportTaskStatus::Scanned {
            return Err(AppError::Busy("目录扫描尚未完成".into()));
        }
        let entries: Vec<_> = self
            .library
            .ready_import_task_entries(scan_task_id)?
            .into_iter()
            .map(|mut entry| {
                entry.task_id.clear();
                entry.rowid = 0;
                entry.status = "ready".into();
                entry
            })
            .collect();
        let task = self.library.create_import_task(NewImportTask {
            source_id: scan.source_id.clone(),
            kind: if scan.kind == "refresh" {
                "refresh".into()
            } else {
                "directory_import".into()
            },
            options_json: scan.options_json.clone(),
        })?;
        for mut entry in entries {
            entry.task_id = task.id.clone();
            self.library.append_import_task_entry(&entry)?;
        }
        self.library.recount_import_task(&task.id)?;
        // 扫描结果已消费到导入任务：删除扫描任务，避免在库里留下第二份历史。
        self.library.delete_import_task(scan_task_id)?;
        Ok(task)
    }

    pub fn start_import(&self, scan_task_id: &str) -> Result<ImportTask, AppError> {
        let task = self.prepare_import(scan_task_id)?;
        self.run_import_task(&task.id)
    }

    pub fn prepare_refresh(&self, source_id: &str) -> Result<ImportTask, AppError> {
        let source = self.source(source_id)?;
        let root = PathBuf::from(&source.root_path);
        let options = options_from_source(&source);
        validate_existing_root(&self.library, &root).inspect_err(|_| {
            let _ = self
                .library
                .set_import_source_status(source_id, SourceStatus::Offline);
        })?;
        // 刷新后旧的未确认扫描结果过期。
        self.library.delete_directory_scan_tasks(&source.id)?;
        self.library.create_import_task(NewImportTask {
            source_id: Some(source.id.clone()),
            kind: "refresh".into(),
            options_json: serde_json::to_string(&options).unwrap_or_else(|_| "{}".into()),
        })
    }

    /// 刷新在同一个任务中依次扫描和导入，前端无需追踪第二个任务 ID。
    pub fn run_refresh(&self, task_id: &str) -> Result<ImportTask, AppError> {
        let result = (|| {
            let scan = self.task(task_id)?;
            let source = self.source(scan.source_id.as_deref().ok_or(AppError::NotFound)?)?;
            let root = validate_existing_root(&self.library, Path::new(&source.root_path))?;
            let options = options_from_source(&source);
            let scanned = self.scan_into(&scan, &root, &options)?;
            // 扫描里仍然出现的文件都算“还在”：已存在而跳过的条目也算，
            // 否则刷新会把它们误标成失联。
            let seen: HashSet<String> = self
                .library
                .import_task_entry_paths(&scanned.id)?
                .into_iter()
                .map(|path| path_identity(&path))
                .collect();
            let imported = self.run_import(&scanned.id)?;
            if matches!(
                imported.status,
                ImportTaskStatus::Completed | ImportTaskStatus::CompletedWithErrors
            ) {
                self.mark_missing_after_complete_scan(&seen, &source.id)?;
            }
            Ok(imported)
        })();
        if let Err(err) = &result {
            self.fail_task(task_id, err);
            if !matches!(err, AppError::CaptureCancelled) {
                if let Ok(task) = self.task(task_id) {
                    if let Some(source_id) = task.source_id {
                        let _ = self
                            .library
                            .set_import_source_status(&source_id, SourceStatus::Offline);
                    }
                }
            }
        }
        finish_import_task(&self.library, self.sink.as_ref(), task_id);
        result
    }

    pub fn refresh(&self, source_id: &str) -> Result<ImportTask, AppError> {
        let task = self.prepare_refresh(source_id)?;
        self.run_refresh(&task.id)
    }

    pub fn relocate(&self, source_id: &str, new_root: &Path) -> Result<ImportSource, AppError> {
        self.source(source_id)?;
        let root = validate_new_root(&self.library, new_root, Some(source_id))?;
        let assets = self.library.list_external_assets(source_id)?;
        if !assets.is_empty() {
            let matches = assets
                .iter()
                .take(20)
                .filter(|asset| {
                    fs::metadata(root.join(&asset.relative_path))
                        .is_ok_and(|metadata| metadata.is_file() && metadata.len() == asset.size)
                })
                .count();
            if matches == 0 {
                return Err(AppError::InvalidInput(
                    "新目录中没有匹配的原有文件，请确认目录位置".into(),
                ));
            }
        }
        self.library
            .update_import_source_root(source_id, &root.display().to_string())?;
        self.source(source_id)
    }

    /// 移除来源只删除索引和派生缓存，不写入外部目录。
    pub fn remove(&self, source_id: &str, remove_items: bool) -> Result<u64, AppError> {
        self.source(source_id)?;
        let mut removed = 0_u64;
        if remove_items {
            for asset in self.library.list_external_assets(source_id)? {
                self.library.delete_item(&asset.item_id)?;
                self.sink
                    .emit(AppEvent::ItemDeleted(ItemEvent { id: asset.item_id }));
                removed += 1;
            }
        }
        // 素材已逐条删除以清理派生文件；仍传入 remove_items，让来源排除记录一并清除。
        self.library.remove_import_source(source_id, remove_items)?;
        Ok(removed)
    }

    fn scan_into(
        &self,
        task: &ImportTask,
        root: &Path,
        options: &DirectoryScanOptions,
    ) -> Result<ImportTask, AppError> {
        self.library
            .set_import_task_status(&task.id, ImportTaskStatus::Running)?;
        self.library
            .update_import_task_scan_progress(&task.id, 0, 0, 0)?;
        self.sink
            .emit(AppEvent::ImportTaskProgress(ImportTaskEvent {
                id: task.id.clone(),
            }));
        let source_id = task.source_id.as_deref().ok_or(AppError::NotFound)?;
        // 该来源已有的引用资产：扫描时就能认出「导入过且没删」的文件。
        let existing: HashMap<String, ExternalAsset> = self
            .library
            .list_external_assets(source_id)?
            .into_iter()
            .map(|asset| (path_identity(&asset.relative_path), asset))
            .collect();
        let mut candidates = Vec::new();
        let mut stats = ScanStats::default();
        self.walk(
            root,
            root,
            0,
            options,
            task,
            &existing,
            &mut candidates,
            &mut stats,
        )?;
        let mut entries = Vec::with_capacity(candidates.len());
        let mut final_stats = ScanStats::default();
        for (index, candidate) in candidates.into_iter().enumerate() {
            if self
                .library
                .source_path_excluded(source_id, &candidate.relative_path)?
            {
                continue;
            }
            final_stats.record(&candidate);
            entries.push(ImportTaskEntry {
                rowid: 0,
                task_id: task.id.clone(),
                entry_key: index.to_string(),
                display_name: candidate.display_name,
                relative_path: candidate.relative_path,
                size: candidate.size,
                status: candidate.status,
                error_code: candidate.error_code,
                error_message: candidate.error_message,
                item_id: None,
            });
        }
        self.library
            .replace_import_task_entries(&task.id, &entries, final_stats.original_bytes)?;
        self.library
            .set_import_task_status(&task.id, ImportTaskStatus::Scanned)?;
        self.library.mark_import_source_scanned(source_id)?;
        self.sink
            .emit(AppEvent::ImportTaskProgress(ImportTaskEvent {
                id: task.id.clone(),
            }));
        self.task(&task.id)
    }

    #[allow(clippy::too_many_arguments)]
    fn walk(
        &self,
        root: &Path,
        directory: &Path,
        depth: usize,
        options: &DirectoryScanOptions,
        task: &ImportTask,
        existing: &HashMap<String, ExternalAsset>,
        output: &mut Vec<Candidate>,
        stats: &mut ScanStats,
    ) -> Result<(), AppError> {
        if self.library.import_task_cancel_requested(&task.id)? {
            self.library
                .set_import_task_status(&task.id, ImportTaskStatus::Cancelled)?;
            return Err(AppError::CaptureCancelled);
        }
        let mut entries = fs::read_dir(directory)
            .map_err(|err| AppError::Io(format!("读取目录 {} 失败：{err}", directory.display())))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|err| AppError::Io(format!("枚举目录 {} 失败：{err}", directory.display())))?;
        entries.sort_by_key(|entry| entry.file_name());
        for (entry_index, entry) in entries.into_iter().enumerate() {
            // 单层目录也可能包含数万个文件，取消检查不能只放在递归入口。
            if entry_index % 32 == 0 && self.library.import_task_cancel_requested(&task.id)? {
                self.library
                    .set_import_task_status(&task.id, ImportTaskStatus::Cancelled)?;
                return Err(AppError::CaptureCancelled);
            }
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)
                .map_err(|err| AppError::Io(format!("读取 {} 失败：{err}", path.display())))?;
            if is_reparse_or_cloud(&metadata) {
                continue;
            }
            if metadata.is_dir() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if is_hidden_or_system(&metadata, &name)
                    || SKIPPED_DIRECTORIES
                        .iter()
                        .any(|skip| name.eq_ignore_ascii_case(skip))
                {
                    continue;
                }
                if options.include_subdirectories {
                    if depth >= MAX_DIRECTORY_DEPTH {
                        return Err(AppError::InvalidInput(format!(
                            "目录深度超过 {MAX_DIRECTORY_DEPTH}，请缩小范围"
                        )));
                    }
                    self.walk(
                        root,
                        &path,
                        depth + 1,
                        options,
                        task,
                        existing,
                        output,
                        stats,
                    )?;
                }
                continue;
            }
            if !metadata.is_file() {
                continue;
            }
            let relative = path
                .strip_prefix(root)
                .map_err(|_| AppError::InvalidInput("扫描路径逃逸来源目录".into()))?
                .components()
                .map(|part| part.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            validate_relative_path(&relative)?;
            let candidate = candidate_for(
                &path,
                options,
                entry.file_name().to_string_lossy().into_owned(),
                relative,
                metadata.len(),
                modified_millis(&metadata),
            );
            let existing_asset = existing.get(&path_identity(&candidate.relative_path));
            let candidate = mark_already_imported(candidate, existing_asset, &path);
            stats.record(&candidate);
            output.push(candidate);
            if output.len() > MAX_DIRECTORY_FILES {
                return Err(AppError::InvalidInput(format!(
                    "候选文件超过 {MAX_DIRECTORY_FILES} 个，请缩小目录范围"
                )));
            }
            // 批量写库和发事件，避免大目录下逐文件产生数据库与 UI 开销。
            if output.len().is_multiple_of(100) {
                self.library.update_import_task_scan_progress(
                    &task.id,
                    stats.total_count,
                    stats.skipped_count,
                    stats.original_bytes,
                )?;
                self.sink
                    .emit(AppEvent::ImportTaskProgress(ImportTaskEvent {
                        id: task.id.clone(),
                    }));
            }
        }
        Ok(())
    }

    /// 后台执行入口统一兜底任务状态和终态事件，包括取消与根目录失效。
    pub fn run_import_task(&self, task_id: &str) -> Result<ImportTask, AppError> {
        let result = self.run_import(task_id);
        if let Err(err) = &result {
            self.fail_task(task_id, err);
        }
        finish_import_task(&self.library, self.sink.as_ref(), task_id);
        result
    }

    fn run_import(&self, task_id: &str) -> Result<ImportTask, AppError> {
        let task = self.task(task_id)?;
        let source = self.source(task.source_id.as_deref().ok_or(AppError::NotFound)?)?;
        let root = validate_existing_root(&self.library, Path::new(&source.root_path))?;
        self.library
            .set_import_task_status(task_id, ImportTaskStatus::Running)?;
        let entries = self.library.ready_import_task_entries(task_id)?;
        for entry in entries {
            if self.library.import_task_cancel_requested(task_id)? {
                self.library
                    .set_import_task_status(task_id, ImportTaskStatus::Cancelled)?;
                return self.task(task_id);
            }
            match self.import_one(&source, &root, &entry) {
                Ok(Some((item, created))) => {
                    self.library.update_import_task_entry(
                        entry.rowid,
                        "imported",
                        Some(&item.id),
                        None,
                        None,
                    )?;
                    if created {
                        self.sink.emit(AppEvent::ItemCreated(ItemEvent {
                            id: item.id.clone(),
                        }));
                    } else {
                        self.sink.emit(AppEvent::ItemUpdated(ItemUpdatedEvent {
                            id: item.id.clone(),
                            fields: vec!["content".into(), "status".into()],
                        }));
                    }
                    self.queue_processing(&item);
                }
                Ok(None) => {
                    self.library.update_import_task_entry(
                        entry.rowid,
                        "skipped",
                        entry.item_id.as_deref(),
                        None,
                        None,
                    )?;
                }
                Err(err) => {
                    self.library.update_import_task_entry(
                        entry.rowid,
                        "failed",
                        None,
                        Some("IMPORT_FAILED"),
                        Some(&err.to_string()),
                    )?;
                }
            }
            self.sink
                .emit(AppEvent::ImportTaskProgress(ImportTaskEvent {
                    id: task_id.into(),
                }));
        }
        let current = self.task(task_id)?;
        let status = if current.failed_count > 0 {
            ImportTaskStatus::CompletedWithErrors
        } else {
            ImportTaskStatus::Completed
        };
        self.library.set_import_task_status(task_id, status)?;
        self.task(task_id)
    }

    /// 返回素材及是否新建；`None` 表示同路径同内容，无需重复处理。
    fn import_one(
        &self,
        source: &ImportSource,
        root: &Path,
        entry: &ImportTaskEntry,
    ) -> Result<Option<(Item, bool)>, AppError> {
        let snapshot = read_snapshot(root, entry)?;
        let existing = self
            .library
            .external_asset_at_path(&source.id, &entry.relative_path)?;
        if existing
            .as_ref()
            .and_then(|asset| asset.indexed_hash.as_deref())
            == Some(snapshot.hash.as_str())
        {
            if let Some(asset) = existing {
                self.library
                    .set_external_asset_status(&asset.item_id, "available")?;
                return Ok(None);
            }
        }
        let new = snapshot.new_item;
        if let Some(asset) = existing {
            let item = self.library.update_reference_item(
                &asset.item_id,
                snapshot.size,
                snapshot.modified_at,
                &new,
            )?;
            Ok(Some((item, false)))
        } else {
            let item = self.library.insert_reference_item(
                &source.id,
                &entry.relative_path,
                snapshot.size,
                snapshot.modified_at,
                new,
            )?;
            Ok(Some((item, true)))
        }
    }

    fn queue_processing(&self, item: &Item) {
        if item.kind == ItemKind::Image {
            if let Err(err) = crate::thumbnail::spawn_thumbnail(
                Arc::clone(&self.library),
                Arc::clone(&self.sink),
                item.id.clone(),
                String::new(),
            ) {
                tracing::warn!(item = %item.id, %err, "引用缩略图入队失败");
            }
        }
        // 阶段一 + （开关打开时的）阶段二统一从这里入队；批量导入的云端任务降优先级。
        crate::pipeline_host::enqueue_bulk_indexing(&self.library, item);
    }

    fn mark_missing_after_complete_scan(
        &self,
        seen: &HashSet<String>,
        source_id: &str,
    ) -> Result<(), AppError> {
        for asset in self.library.list_external_assets(source_id)? {
            let status = if seen.contains(&path_identity(&asset.relative_path)) {
                "available"
            } else {
                "missing"
            };
            self.library
                .set_external_asset_status(&asset.item_id, status)?;
        }
        Ok(())
    }

    fn source(&self, id: &str) -> Result<ImportSource, AppError> {
        self.library
            .get_import_source(id)?
            .ok_or(AppError::NotFound)
    }

    fn task(&self, id: &str) -> Result<ImportTask, AppError> {
        self.library.get_import_task(id)?.ok_or(AppError::NotFound)
    }

    fn fail_task(&self, id: &str, err: &AppError) {
        if matches!(err, AppError::CaptureCancelled) {
            let _ = self
                .library
                .set_import_task_status(id, ImportTaskStatus::Cancelled);
        } else {
            let _ = self.library.set_import_task_error(id, &err.to_string());
        }
    }
}

struct Snapshot {
    size: u64,
    modified_at: i64,
    hash: String,
    new_item: NewItem,
}

fn options_from_source(source: &ImportSource) -> DirectoryScanOptions {
    DirectoryScanOptions {
        include_subdirectories: source.include_subdirectories,
        include_images: source.include_images,
        include_text: source.include_text,
    }
}

fn candidate_for(
    path: &Path,
    options: &DirectoryScanOptions,
    display_name: String,
    relative_path: String,
    size: u64,
    modified_at: i64,
) -> Candidate {
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let maximum = match extension.as_str() {
        "png" | "jpg" | "jpeg" | "webp" | "gif" if options.include_images => Some(MAX_IMAGE_BYTES),
        "txt" | "md" if options.include_text => Some(MAX_TEXT_BYTES),
        _ => None,
    };
    let (status, error_code, error_message) = match maximum {
        None => (
            "skipped",
            Some("UNSUPPORTED".into()),
            Some("不支持的文件类型，或未勾选对应类型".into()),
        ),
        Some(maximum) if size > maximum => (
            "skipped",
            Some("TOO_LARGE".into()),
            Some("文件超过大小限制，未创建素材".into()),
        ),
        Some(_) => ("ready", None, None),
    };
    Candidate {
        relative_path,
        display_name,
        size,
        modified_at,
        status: status.into(),
        error_code,
        error_message,
    }
}

/// 扫描阶段就按「已导入且未删除」标成跳过，用户确认前就能看到哪些不会重复导入。
///
/// 判定顺序按开销递增：大小不同说明内容一定变了；大小与修改时间都一致时直接认定
/// 已存在（不读文件，重复扫描大目录时才不会把全目录重读一遍）；只有大小相同但
/// 改过时间，才读文件算哈希确认内容真的没变。
fn mark_already_imported(
    mut candidate: Candidate,
    existing: Option<&ExternalAsset>,
    path: &Path,
) -> Candidate {
    if candidate.status != "ready" {
        return candidate;
    }
    let Some(asset) = existing else {
        return candidate;
    };
    // 失联/权限异常的资产交给导入阶段处理，好让状态能恢复成 available。
    if asset.source_status != "available" || asset.size != candidate.size {
        return candidate;
    }
    let Some(indexed) = asset.indexed_hash.as_deref() else {
        return candidate;
    };
    let unchanged = asset.modified_at == candidate.modified_at
        || file_hash(path).is_some_and(|hash| hash == indexed);
    if unchanged {
        candidate.status = "skipped".into();
        candidate.error_code = Some(ALREADY_IMPORTED.into());
        candidate.error_message = Some("已导入且未删除，将跳过".into());
    }
    candidate
}

/// 只在“大小相同但改过时间”时才调用；读不到文件就退回导入阶段报错。
fn file_hash(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    Some(format!("{:x}", Sha256::digest(&bytes)))
}

fn modified_millis(metadata: &fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}

fn validate_existing_root(library: &Library, root: &Path) -> Result<PathBuf, AppError> {
    let raw = root.display().to_string();
    let normalized = raw.replace('/', "\\");
    // Windows 规范化本地路径以 \\?\ 开头，不应被误判为 UNC 网络路径。
    let is_unc = normalized.starts_with("\\\\?\\UNC\\")
        || (normalized.starts_with("\\\\") && !normalized.starts_with("\\\\?\\"));
    if is_unc {
        return Err(AppError::InvalidInput("首版不支持 UNC 网络目录".into()));
    }
    let canonical = fs::canonicalize(root)
        .map_err(|err| AppError::FileMissing(format!("{}: {err}", root.display())))?;
    if !canonical.is_dir() {
        return Err(AppError::InvalidInput("请选择可用目录".into()));
    }
    let library_root =
        fs::canonicalize(library.root()).map_err(|err| AppError::Io(err.to_string()))?;
    if paths_overlap(&canonical, &library_root) {
        return Err(AppError::InvalidInput(
            "导入目录不能与素材库目录重叠".into(),
        ));
    }
    Ok(canonical)
}

fn validate_new_root(
    library: &Library,
    root: &Path,
    except_source: Option<&str>,
) -> Result<PathBuf, AppError> {
    let canonical = validate_existing_root(library, root)?;
    if canonical.file_name().is_none() {
        return Err(AppError::InvalidInput("不能导入整个盘符根目录".into()));
    }
    if let Ok(windows) = std::env::var("SystemRoot") {
        if let Ok(system) = fs::canonicalize(windows) {
            if canonical.starts_with(system) {
                return Err(AppError::InvalidInput("不能导入 Windows 系统目录".into()));
            }
        }
    }
    for source in library.list_import_sources()? {
        if Some(source.id.as_str()) == except_source {
            continue;
        }
        // 同一个目录允许重复导入：数据层按 `path_key` 复用同一来源，
        // 已导入且未删除的文件会在扫描阶段标记为跳过。
        if same_directory(&canonical, &source.root_path) {
            continue;
        }
        if paths_overlap(&canonical, Path::new(&source.root_path)) {
            return Err(AppError::InvalidInput(format!(
                "该目录与已有来源的父目录或子目录重叠：{}",
                source.root_path
            )));
        }
    }
    Ok(canonical)
}

/// 路径比较用的规范形态：去掉 `\\?\` 前缀、统一分隔符后按组件切分（Windows 下小写）。
fn identity_components(path: &Path) -> Vec<String> {
    path_identity(&path.display().to_string())
        .split('\\')
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect()
}

/// 两个目录是否是同一个（忽略大小写、末尾分隔符与路径写法差异）。
fn same_directory(left: &Path, right: &str) -> bool {
    identity_components(left) == identity_components(Path::new(right))
}

/// 按路径组件判断父子/相同关系，避免 `C:\a` 与 `C:\ab` 这类字符串前缀误判。
fn paths_overlap(left: &Path, right: &Path) -> bool {
    let left = identity_components(left);
    let right = identity_components(right);
    left.starts_with(right.as_slice()) || right.starts_with(left.as_slice())
}

fn is_hidden_or_system(metadata: &fs::Metadata, name: &str) -> bool {
    if name.starts_with('.') {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const HIDDEN: u32 = 0x2;
        const SYSTEM: u32 = 0x4;
        metadata.file_attributes() & (HIDDEN | SYSTEM) != 0
    }
    #[cfg(not(windows))]
    {
        let _ = metadata;
        false
    }
}

fn is_reparse_or_cloud(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const REPARSE: u32 = 0x400;
        const OFFLINE: u32 = 0x1000;
        const RECALL_ON_OPEN: u32 = 0x40000;
        const RECALL_ON_DATA_ACCESS: u32 = 0x400000;
        metadata.file_attributes() & (REPARSE | OFFLINE | RECALL_ON_OPEN | RECALL_ON_DATA_ACCESS)
            != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// 读取前后都核验元数据，避免索引到的哈希与随后读取的原文件不是同一版本。
fn read_snapshot(root: &Path, entry: &ImportTaskEntry) -> Result<Snapshot, AppError> {
    validate_relative_path(&entry.relative_path)?;
    let path = root.join(&entry.relative_path);
    let before = fs::symlink_metadata(&path)
        .map_err(|err| AppError::FileMissing(format!("{}: {err}", path.display())))?;
    if is_reparse_or_cloud(&before) || !before.is_file() {
        return Err(AppError::InvalidInput("引用文件已变为不支持的对象".into()));
    }
    let canonical =
        fs::canonicalize(&path).map_err(|err| AppError::FileMissing(err.to_string()))?;
    if !canonical.starts_with(root) {
        return Err(AppError::InvalidInput("引用路径逃逸来源目录".into()));
    }
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let (kind, maximum) = match extension.as_str() {
        "png" | "jpg" | "jpeg" | "webp" | "gif" => (ItemKind::Image, MAX_IMAGE_BYTES),
        "txt" | "md" => (ItemKind::Text, MAX_TEXT_BYTES),
        _ => return Err(AppError::InvalidInput("不支持的文件类型".into())),
    };
    if before.len() > maximum {
        return Err(AppError::InvalidInput("文件超过大小限制".into()));
    }
    let bytes = fs::read(&canonical).map_err(|err| AppError::Io(format!("读取引用失败：{err}")))?;
    let after = fs::metadata(&canonical).map_err(|err| AppError::FileMissing(err.to_string()))?;
    let before_modified = modified_millis(&before);
    if before.len() != after.len() || before_modified != modified_millis(&after) {
        return Err(AppError::Busy(
            "文件在读取过程中发生变化，请稍后重试".into(),
        ));
    }
    let hash = format!("{:x}", Sha256::digest(&bytes));
    let title = Path::new(&entry.display_name)
        .file_stem()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| entry.display_name.clone());
    let (width, height, char_count) = if kind == ItemKind::Image {
        let dimensions = image::ImageReader::new(Cursor::new(&bytes))
            .with_guessed_format()
            .map_err(|err| AppError::InvalidInput(format!("无法识别图片：{err}")))?
            .into_dimensions()
            .map_err(|err| AppError::InvalidInput(format!("无法读取图片尺寸：{err}")))?;
        (Some(dimensions.0), Some(dimensions.1), None)
    } else {
        let text = decode_imported_text(&bytes);
        (
            None,
            None,
            Some(text.chars().count().min(u32::MAX as usize) as u32),
        )
    };
    Ok(Snapshot {
        size: before.len(),
        modified_at: before_modified,
        hash: hash.clone(),
        new_item: NewItem {
            kind,
            captured_at: now_millis(),
            source_app: Some("VisRecall".into()),
            source_title: Some(title),
            source_url: None,
            width,
            height,
            char_count,
            original_path: String::new(),
            content_hash: hash,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{NullEventSink, RecordingSink};

    /// 旧任务 `options_json` 里残留的 `aiAllowed` 必须被忽略，不能导致扫描无法恢复。
    #[test]
    fn legacy_directory_options_with_ai_allowed_still_deserialize() {
        let options: DirectoryScanOptions = serde_json::from_str(
            r#"{"includeSubdirectories":true,"includeImages":true,"includeText":false,"aiAllowed":false}"#,
        )
        .unwrap();
        assert!(options.include_subdirectories);
        assert!(options.include_images);
        assert!(!options.include_text);
    }

    #[test]
    fn scan_import_and_refresh_preserve_external_file_and_item_id() {
        let library_dir = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        fs::create_dir(source_dir.path().join("node_modules")).unwrap();
        fs::write(source_dir.path().join("node_modules/ignored.txt"), "忽略").unwrap();
        fs::write(source_dir.path().join("note.txt"), "第一版").unwrap();
        fs::write(source_dir.path().join("unsupported.pdf"), "不导入").unwrap();
        let library = Arc::new(Library::open(library_dir.path().to_path_buf()).unwrap());
        let service = DirectoryImportService::new(library.clone(), Arc::new(NullEventSink));

        let scan = service
            .scan(source_dir.path(), DirectoryScanOptions::default())
            .unwrap();
        assert_eq!(scan.total_count, 2);
        assert_eq!(scan.skipped_count, 1);
        assert_eq!(scan.original_bytes, "第一版".len() as u64);
        let entries = library
            .list_import_task_entries(&scan.id, None, 100)
            .unwrap();
        let unsupported = entries
            .iter()
            .find(|entry| entry.relative_path == "unsupported.pdf")
            .unwrap();
        assert_eq!(unsupported.status, "skipped");
        assert_eq!(unsupported.error_code.as_deref(), Some("UNSUPPORTED"));
        let imported = service.start_import(&scan.id).unwrap();
        assert_eq!(imported.total_count, 1);
        assert_eq!(imported.succeeded_count, 1);
        let source = library.list_import_sources().unwrap().remove(0);
        let asset = library.list_external_assets(&source.id).unwrap().remove(0);
        assert_eq!(
            library
                .get_item(&asset.item_id)
                .unwrap()
                .unwrap()
                .original_path,
            ""
        );
        assert_eq!(
            fs::read_to_string(source_dir.path().join("note.txt")).unwrap(),
            "第一版"
        );

        fs::write(source_dir.path().join("note.txt"), "第二版内容").unwrap();
        service.refresh(&source.id).unwrap();
        let refreshed = library.list_external_assets(&source.id).unwrap().remove(0);
        assert_eq!(refreshed.item_id, asset.item_id);
        assert!(library.get_item(&asset.item_id).unwrap().unwrap().revision > 0);
        assert_eq!(
            fs::read_to_string(source_dir.path().join("note.txt")).unwrap(),
            "第二版内容"
        );
    }

    #[test]
    fn same_directory_can_be_scanned_again_but_nested_roots_are_rejected() {
        let library_dir = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        fs::create_dir(source_dir.path().join("nested")).unwrap();
        let library = Arc::new(Library::open(library_dir.path().to_path_buf()).unwrap());
        let service = DirectoryImportService::new(library, Arc::new(NullEventSink));
        // 导入目录不能与素材库目录重叠。
        assert!(service
            .scan(library_dir.path(), DirectoryScanOptions::default())
            .is_err());
        let first = service
            .scan(source_dir.path(), DirectoryScanOptions::default())
            .unwrap();
        // 同一目录可以重复导入：复用同一来源，不新建。
        let again = service
            .scan(
                &source_dir.path().join("."),
                DirectoryScanOptions::default(),
            )
            .unwrap();
        assert_eq!(again.source_id, first.source_id);
        // 父子目录仍拒绝，避免同一文件被两个来源各收录一次。
        assert!(service
            .scan(
                &source_dir.path().join("nested"),
                DirectoryScanOptions::default()
            )
            .is_err());
    }

    #[test]
    fn rescanning_a_directory_marks_imported_files_as_skipped() {
        let library_dir = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        fs::write(source_dir.path().join("note.txt"), "已经导入过").unwrap();
        let library = Arc::new(Library::open(library_dir.path().to_path_buf()).unwrap());
        let service = DirectoryImportService::new(library.clone(), Arc::new(NullEventSink));

        let first = service
            .scan(source_dir.path(), DirectoryScanOptions::default())
            .unwrap();
        let imported = service.start_import(&first.id).unwrap();
        assert_eq!(imported.succeeded_count, 1);
        // 导入任务不保留历史：终态后任务行被删除。
        assert!(library.list_import_tasks(10).unwrap().is_empty());

        let second = service
            .scan(source_dir.path(), DirectoryScanOptions::default())
            .unwrap();
        assert_eq!(second.total_count, 1);
        assert_eq!(second.skipped_count, 1);
        let entries = library
            .list_import_task_entries(&second.id, None, 10)
            .unwrap();
        assert_eq!(entries[0].status, "skipped");
        assert_eq!(entries[0].error_code.as_deref(), Some(ALREADY_IMPORTED));

        // 没有可导入项，也不会重复建素材。
        let reimport = service.start_import(&second.id).unwrap();
        assert_eq!(reimport.total_count, 0);
        assert_eq!(
            library
                .list_external_assets(second.source_id.as_deref().unwrap())
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn unchanged_file_is_not_rewritten_or_marked_missing_by_refresh() {
        let library_dir = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        fs::write(source_dir.path().join("note.txt"), "内容不变").unwrap();
        let library = Arc::new(Library::open(library_dir.path().to_path_buf()).unwrap());
        let service = DirectoryImportService::new(library.clone(), Arc::new(NullEventSink));

        let scan = service
            .scan(source_dir.path(), DirectoryScanOptions::default())
            .unwrap();
        service.start_import(&scan.id).unwrap();
        let source_id = scan.source_id.unwrap();
        let before = library.list_external_assets(&source_id).unwrap();
        assert_eq!(before[0].source_status, "available");

        service.refresh(&source_id).unwrap();
        let after = library.list_external_assets(&source_id).unwrap();
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].item_id, before[0].item_id);
        // 已存在而跳过的文件仍然「在扫描结果里」，不能被刷新当成失联。
        assert_eq!(after[0].source_status, "available");
    }

    #[test]
    fn scan_marks_oversized_files_and_imports_only_ready_entries() {
        let library_dir = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        fs::write(source_dir.path().join("ready.txt"), "可导入").unwrap();
        fs::write(
            source_dir.path().join("large.md"),
            vec![b'x'; MAX_TEXT_BYTES as usize + 1],
        )
        .unwrap();
        fs::write(source_dir.path().join("ignored.pdf"), "不支持").unwrap();
        let library = Arc::new(Library::open(library_dir.path().to_path_buf()).unwrap());
        let service = DirectoryImportService::new(library.clone(), Arc::new(NullEventSink));

        let scan = service
            .scan(source_dir.path(), DirectoryScanOptions::default())
            .unwrap();
        assert_eq!(scan.total_count, 3);
        assert_eq!(scan.skipped_count, 2);
        assert_eq!(scan.original_bytes, "可导入".len() as u64);
        let entries = library
            .list_import_task_entries(&scan.id, None, 100)
            .unwrap();
        assert_eq!(
            entries
                .iter()
                .find(|entry| entry.relative_path == "large.md")
                .unwrap()
                .error_code
                .as_deref(),
            Some("TOO_LARGE")
        );
        let imported = service.start_import(&scan.id).unwrap();
        assert_eq!(imported.total_count, 1);
        assert_eq!(imported.succeeded_count, 1);
        assert_eq!(
            library
                .list_external_assets(scan.source_id.as_deref().unwrap())
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn scan_persists_progress_and_emits_one_terminal_event() {
        let library_dir = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        for index in 0..101 {
            fs::write(source_dir.path().join(format!("note-{index:03}.txt")), "x").unwrap();
        }
        let library = Arc::new(Library::open(library_dir.path().to_path_buf()).unwrap());
        let sink = Arc::new(RecordingSink::default());
        let service = DirectoryImportService::new(library, sink.clone());

        let scan = service
            .scan(source_dir.path(), DirectoryScanOptions::default())
            .unwrap();
        assert_eq!(scan.total_count, 101);
        assert_eq!(scan.original_bytes, 101);
        let events = sink.events();
        let progress = events
            .iter()
            .filter(|event| matches!(event, AppEvent::ImportTaskProgress(_)))
            .count();
        let finished = events
            .iter()
            .filter(
                |event| matches!(event, AppEvent::ImportTaskFinished(event) if event.id == scan.id),
            )
            .count();
        assert!(
            progress >= 3,
            "应包含开始、第 100 项和扫描完成进度事件：{events:?}"
        );
        assert_eq!(finished, 1);
    }

    #[test]
    fn cancelled_or_failed_refresh_does_not_mark_existing_assets_missing() {
        let library_dir = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        fs::write(source_dir.path().join("note.txt"), "保留").unwrap();
        let library = Arc::new(Library::open(library_dir.path().to_path_buf()).unwrap());
        let sink = Arc::new(RecordingSink::default());
        let service = DirectoryImportService::new(library.clone(), sink.clone());
        let scan = service
            .scan(source_dir.path(), DirectoryScanOptions::default())
            .unwrap();
        service.start_import(&scan.id).unwrap();
        let source_id = scan.source_id.unwrap();
        let item_id = library
            .list_external_assets(&source_id)
            .unwrap()
            .remove(0)
            .item_id;
        fs::remove_file(source_dir.path().join("note.txt")).unwrap();

        let cancelled = service.prepare_refresh(&source_id).unwrap();
        library.request_import_task_cancel(&cancelled.id).unwrap();
        assert!(matches!(
            service.run_refresh(&cancelled.id),
            Err(AppError::CaptureCancelled)
        ));
        // 终态任务行随即删除，结果只在完成事件里。
        assert!(library.get_import_task(&cancelled.id).unwrap().is_none());
        assert_eq!(
            library
                .list_external_assets(&source_id)
                .unwrap()
                .remove(0)
                .source_status,
            "available"
        );
        assert_eq!(library.get_item(&item_id).unwrap().unwrap().id, item_id);

        let failed = service.prepare_refresh(&source_id).unwrap();
        fs::remove_dir_all(source_dir.path()).unwrap();
        assert!(service.run_refresh(&failed.id).is_err());
        assert!(library.get_import_task(&failed.id).unwrap().is_none());
        let events = sink.events();
        for task_id in [cancelled.id, failed.id] {
            assert_eq!(events.iter().filter(|event| matches!(event, AppEvent::ImportTaskFinished(event) if event.id == task_id)).count(), 1);
        }
    }

    #[test]
    fn removed_source_can_be_readded_without_duplicating_kept_items() {
        let library_dir = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        let original = source_dir.path().join("note.txt");
        fs::write(&original, "原文件保持不变").unwrap();
        let library = Arc::new(Library::open(library_dir.path().to_path_buf()).unwrap());
        let service = DirectoryImportService::new(library.clone(), Arc::new(NullEventSink));
        let first_scan = service
            .scan(source_dir.path(), DirectoryScanOptions::default())
            .unwrap();
        service.start_import(&first_scan.id).unwrap();
        let source_id = first_scan.source_id.unwrap();
        let item_id = library
            .list_external_assets(&source_id)
            .unwrap()
            .remove(0)
            .item_id;

        assert_eq!(service.remove(&source_id, false).unwrap(), 0);
        assert!(library.get_item(&item_id).unwrap().is_some());
        let second_scan = service
            .scan(source_dir.path(), DirectoryScanOptions::default())
            .unwrap();
        assert_eq!(second_scan.source_id.as_deref(), Some(source_id.as_str()));
        // 重新添加时原有素材还在，扫描阶段就标成「已存在」。
        assert_eq!(second_scan.skipped_count, 1);
        let second_import = service.start_import(&second_scan.id).unwrap();
        assert_eq!(second_import.total_count, 0);
        let assets = library.list_external_assets(&source_id).unwrap();
        assert_eq!(assets.len(), 1);
        assert_eq!(assets[0].item_id, item_id);
        assert_eq!(assets[0].source_status, "available");
        assert_eq!(fs::read_to_string(original).unwrap(), "原文件保持不变");
    }

    #[test]
    fn deleting_item_lets_a_followup_scan_import_it_again() {
        let library_dir = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        let note = source_dir.path().join("note.txt");
        let old = source_dir.path().join("old.txt");
        fs::write(&note, "保留到重新添加").unwrap();
        fs::write(&old, "此前单条删除").unwrap();
        let library = Arc::new(Library::open(library_dir.path().to_path_buf()).unwrap());
        let service = DirectoryImportService::new(library.clone(), Arc::new(NullEventSink));
        let first_scan = service
            .scan(source_dir.path(), DirectoryScanOptions::default())
            .unwrap();
        service.start_import(&first_scan.id).unwrap();
        let source_id = first_scan.source_id.unwrap();
        let old_asset = library
            .external_asset_at_path(&source_id, "old.txt")
            .unwrap()
            .unwrap();
        library.delete_item(&old_asset.item_id).unwrap();

        // 删除后重扫同一目录：来源复用，未删的标记跳过，删掉的那条回到可导入。
        let second_scan = service
            .scan(source_dir.path(), DirectoryScanOptions::default())
            .unwrap();
        assert_eq!(second_scan.source_id.as_deref(), Some(source_id.as_str()));
        assert_eq!(second_scan.total_count, 2);
        assert_eq!(second_scan.skipped_count, 1);
        let second_import = service.start_import(&second_scan.id).unwrap();
        assert_eq!(second_import.succeeded_count, 1);
        let rebuilt = library.list_external_assets(&source_id).unwrap();
        assert_eq!(rebuilt.len(), 2);
        assert!(rebuilt
            .iter()
            .all(|asset| asset.item_id != old_asset.item_id));
        assert_eq!(fs::read_to_string(note).unwrap(), "保留到重新添加");
        assert_eq!(fs::read_to_string(old).unwrap(), "此前单条删除");
    }
}
