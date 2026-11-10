//! 主窗口单文件导入的预检与批次结果。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use visrecall_domain::{AppError, Item};

use crate::import_flow::{ImportFlow, MAX_IMPORT_FILES};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportFileSummary {
    pub entry_id: String,
    pub file_name: String,
    pub size: u64,
    pub supported: bool,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct FileImportReport {
    pub items: Vec<Item>,
    pub failures: Vec<(String, String)>,
}

pub struct ImportService {
    flow: Arc<ImportFlow>,
}

impl ImportService {
    pub fn new(flow: Arc<ImportFlow>) -> Self {
        Self { flow }
    }

    pub fn inspect(paths: &[PathBuf]) -> Vec<ImportFileSummary> {
        paths
            .iter()
            .enumerate()
            .map(|(index, path)| inspect_file(index, path))
            .collect()
    }

    pub fn import_files(&self, paths: &[PathBuf]) -> Result<FileImportReport, AppError> {
        if paths.is_empty() {
            return Err(AppError::InvalidInput("请选择要导入的文件".into()));
        }
        if paths.len() > MAX_IMPORT_FILES {
            return Err(AppError::InvalidInput(format!(
                "每批最多导入 {MAX_IMPORT_FILES} 个文件"
            )));
        }
        let mut report = FileImportReport::default();
        for path in paths {
            match self.flow.import_manual_file(path) {
                Ok(item) => {
                    self.flow.emit_item_created(&item);
                    report.items.push(item);
                }
                Err(err) => report
                    .failures
                    .push((path.display().to_string(), err.to_string())),
            }
        }
        Ok(report)
    }
}

fn inspect_file(index: usize, path: &Path) -> ImportFileSummary {
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let mut summary = ImportFileSummary {
        entry_id: format!("file-{index}"),
        file_name,
        size: 0,
        supported: false,
        reason: None,
    };
    if path.is_dir() {
        summary.reason = Some("目录请使用“导入目录”，不会复制展开".into());
        return summary;
    }
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let limit = match extension.as_str() {
        "png" | "jpg" | "jpeg" | "webp" | "gif" => 20 * 1024 * 1024,
        "txt" | "md" => 500 * 1024,
        _ => {
            summary.reason = Some("仅支持图片、TXT 和 Markdown".into());
            return summary;
        }
    };
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.len() > limit => {
            summary.size = metadata.len();
            summary.reason = Some(if limit == 500 * 1024 {
                "文本文件超过 500 KiB".into()
            } else {
                "图片超过 20 MiB".into()
            });
        }
        Ok(metadata) => {
            summary.size = metadata.len();
            summary.supported = true;
        }
        Err(err) => summary.reason = Some(format!("无法读取文件：{err}")),
    }
    summary
}
