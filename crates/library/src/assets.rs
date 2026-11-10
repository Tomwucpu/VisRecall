//! 按素材 ID 统一解析托管文件和外部引用，调用方不接触任意绝对路径。

use std::fs;
use std::path::{Component, Path, PathBuf};

use encoding_rs::{GB18030, UTF_16BE, UTF_16LE};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use visrecall_domain::item::StorageMode;
use visrecall_domain::AppError;

use crate::db::{db_error, Library};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedAsset {
    pub item_id: String,
    pub storage_mode: StorageMode,
    pub path: PathBuf,
    pub source_id: Option<String>,
    pub relative_path: String,
    pub revision: i64,
}

pub struct AssetReader<'a> {
    library: &'a Library,
}

impl<'a> AssetReader<'a> {
    pub fn new(library: &'a Library) -> Self {
        Self { library }
    }

    pub fn resolve(&self, item_id: &str) -> Result<ResolvedAsset, AppError> {
        let conn = self.library.conn()?;
        let row = conn
            .query_row(
                "SELECT i.original_path, i.storage_mode, i.revision,
                        s.id, s.root_path, s.status, e.relative_path
                 FROM items i
                 LEFT JOIN external_assets e ON e.item_id = i.id
                 LEFT JOIN import_sources s ON s.id = e.source_id
                 WHERE i.id = ?1",
                [item_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, Option<String>>(6)?,
                    ))
                },
            )
            .optional()
            .map_err(db_error)?
            .ok_or(AppError::NotFound)?;
        drop(conn);

        let mode = StorageMode::parse(&row.1)
            .ok_or_else(|| AppError::SchemaViolation(format!("未知存储模式：{}", row.1)))?;
        match mode {
            StorageMode::Managed => {
                if row.0.is_empty() {
                    return Err(AppError::FileMissing(item_id.to_string()));
                }
                Ok(ResolvedAsset {
                    item_id: item_id.to_string(),
                    storage_mode: mode,
                    path: self.library.resolve_existing_or_join(&row.0)?,
                    source_id: None,
                    relative_path: row.0,
                    revision: row.2,
                })
            }
            StorageMode::Reference => {
                let (source_id, root, status, relative) = match (row.3, row.4, row.5, row.6) {
                    (Some(id), Some(root), Some(status), Some(relative)) => {
                        (id, root, status, relative)
                    }
                    _ => {
                        return Err(AppError::SchemaViolation(format!(
                            "引用素材 {item_id} 缺少来源映射"
                        )))
                    }
                };
                if status == "removed" {
                    return Err(AppError::FileMissing(relative));
                }
                validate_relative_path(&relative)?;
                let root = PathBuf::from(root);
                let canonical_root = fs::canonicalize(&root)
                    .map_err(|_| AppError::FileMissing(root.display().to_string()))?;
                let candidate = canonical_root.join(&relative);
                reject_reparse_point(&candidate)?;
                let canonical =
                    fs::canonicalize(&candidate).map_err(|err| map_read_error(&candidate, err))?;
                if !canonical.starts_with(&canonical_root) {
                    return Err(AppError::InvalidInput("引用路径逃逸来源目录".into()));
                }
                reject_reparse_point(&canonical)?;
                Ok(ResolvedAsset {
                    item_id: item_id.to_string(),
                    storage_mode: mode,
                    path: canonical,
                    source_id: Some(source_id),
                    relative_path: relative,
                    revision: row.2,
                })
            }
        }
    }

    pub fn read_bytes(&self, item_id: &str, maximum: u64) -> Result<Vec<u8>, AppError> {
        let asset = self.resolve(item_id)?;
        let metadata = fs::metadata(&asset.path).map_err(|err| map_read_error(&asset.path, err))?;
        if metadata.len() > maximum {
            return Err(AppError::InvalidInput(format!(
                "文件超过大小限制：{} 字节",
                metadata.len()
            )));
        }
        fs::read(&asset.path).map_err(|err| map_read_error(&asset.path, err))
    }

    /// 文本引用与托管导入共用编码兼容规则，输出 UTF-8 字符串。
    pub fn read_text(&self, item_id: &str, maximum: u64) -> Result<String, AppError> {
        let bytes = self.read_bytes(item_id, maximum)?;
        Ok(decode_text(&bytes))
    }
}

fn decode_text(bytes: &[u8]) -> String {
    let (text, _) = if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        (String::from_utf8_lossy(&bytes[3..]).into_owned(), ())
    } else if bytes.starts_with(&[0xFF, 0xFE]) {
        (UTF_16LE.decode(&bytes[2..]).0.into_owned(), ())
    } else if bytes.starts_with(&[0xFE, 0xFF]) {
        (UTF_16BE.decode(&bytes[2..]).0.into_owned(), ())
    } else if std::str::from_utf8(bytes).is_ok() {
        (String::from_utf8_lossy(bytes).into_owned(), ())
    } else {
        (GB18030.decode(bytes).0.into_owned(), ())
    };
    text
}

pub fn validate_relative_path(relative: &str) -> Result<(), AppError> {
    if relative.is_empty()
        || relative.contains('\0')
        || relative.contains(':')
        || relative.starts_with('/')
        || relative.starts_with('\\')
    {
        return Err(AppError::InvalidInput("非法引用相对路径".into()));
    }
    let path = Path::new(relative);
    if path
        .components()
        .any(|part| !matches!(part, Component::Normal(_)))
        || relative
            .split(['/', '\\'])
            .any(|part| part == ".." || part.is_empty())
    {
        return Err(AppError::InvalidInput("引用路径包含逃逸片段".into()));
    }
    Ok(())
}

fn reject_reparse_point(path: &Path) -> Result<(), AppError> {
    let metadata = fs::symlink_metadata(path).map_err(|err| map_read_error(path, err))?;
    if metadata.file_type().is_symlink() {
        return Err(AppError::InvalidInput("不读取符号链接引用".into()));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(AppError::InvalidInput("不读取 Windows 重解析点".into()));
        }
    }
    Ok(())
}

fn map_read_error(path: &Path, err: std::io::Error) -> AppError {
    if err.kind() == std::io::ErrorKind::PermissionDenied {
        AppError::Io(format!("无权限读取 {}: {err}", path.display()))
    } else {
        AppError::FileMissing(path.display().to_string())
    }
}
