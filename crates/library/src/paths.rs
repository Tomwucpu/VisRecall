//! 应用路径解析与库根目录（`library.json`）初始化。

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use visrecall_domain::{now_millis, AppError};

use crate::files;

/// 阶段 01 的库格式版本；更大值表示库由更新版本的应用创建。
pub const LIBRARY_FORMAT_VERSION: u32 = 1;

/// 库元数据文件名。
pub const LIBRARY_META_FILE: &str = "library.json";

const APP_DIR_NAME: &str = "VisRecall";

/// 运行期需要的路径（全部由环境变量推导，未知盘符）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppPaths {
    /// `%APPDATA%/VisRecall/config.json`
    pub config_file: PathBuf,
    /// `%USERPROFILE%/Documents/VisRecall`
    pub default_library_root: PathBuf,
}

impl AppPaths {
    /// 从 `APPDATA`/`USERPROFILE` 推导路径；任一环境变量缺失返回 [`AppError::ConfigMissing`]。
    pub fn resolve() -> Result<AppPaths, AppError> {
        let config_file = config_dir()?.join("config.json");
        Ok(AppPaths {
            config_file,
            default_library_root: default_library_root()?,
        })
    }

    /// `%APPDATA%/VisRecall`
    pub fn config_dir(&self) -> PathBuf {
        match self.config_file.parent() {
            Some(parent) => parent.to_path_buf(),
            None => PathBuf::from(APP_DIR_NAME),
        }
    }
}

/// `%APPDATA%/VisRecall`
pub fn config_dir() -> Result<PathBuf, AppError> {
    Ok(env_path("APPDATA")?.join(APP_DIR_NAME))
}

/// `%USERPROFILE%/Documents/VisRecall`
pub fn default_library_root() -> Result<PathBuf, AppError> {
    Ok(env_path("USERPROFILE")?
        .join("Documents")
        .join(APP_DIR_NAME))
}

/// 库根目录的元数据（`library.json`）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryMeta {
    pub library_id: String,
    pub format_version: u32,
    pub created_at: i64,
}

impl LibraryMeta {
    /// 新建库时生成的元数据。
    pub fn fresh() -> LibraryMeta {
        LibraryMeta {
            library_id: uuid::Uuid::new_v4().to_string(),
            format_version: LIBRARY_FORMAT_VERSION,
            created_at: now_millis(),
        }
    }
}

/// 确保库根目录结构存在，并读取/初始化 `library.json`。
///
/// - 目录：`root`、`originals/images`、`texts`、`thumbs`、`tmp`、`backups`。
/// - `library.json` 缺失时原子写入新元数据。
/// - `format_version > 1` 返回 [`AppError::InvalidInput`]（库由更新版本创建，不降级读写）。
/// - `library_id` 缺失/为空或 JSON 无法解析时视为损坏：备份为
///   `library.json.bad-<unix>` 后重建（阶段 01 只记录 `tracing::warn`）。
pub fn ensure_library_root(root: &Path) -> Result<LibraryMeta, AppError> {
    create_layout(root)?;

    let meta_path = root.join(LIBRARY_META_FILE);
    let text = match fs::read_to_string(&meta_path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            let meta = LibraryMeta::fresh();
            write_meta(&meta_path, &meta)?;
            return Ok(meta);
        }
        Err(err) => return Err(AppError::Io(format!("read {}: {err}", meta_path.display()))),
    };

    match inspect_meta(&text) {
        MetaState::Valid(meta) => Ok(meta),
        MetaState::TooNew => Err(AppError::InvalidInput(
            "library format newer than app".into(),
        )),
        MetaState::Corrupt(reason) => {
            tracing::warn!(
                path = %meta_path.display(),
                reason,
                "library metadata corrupt; rebuilding"
            );
            files::backup_corrupt(&meta_path);
            let meta = LibraryMeta::fresh();
            write_meta(&meta_path, &meta)?;
            Ok(meta)
        }
    }
}

enum MetaState {
    Valid(LibraryMeta),
    TooNew,
    Corrupt(&'static str),
}

fn inspect_meta(text: &str) -> MetaState {
    // 用宽松结构体区分“格式更新”与“文件损坏”。
    #[derive(Deserialize)]
    struct Probe {
        #[serde(default)]
        library_id: Option<String>,
        #[serde(default)]
        format_version: Option<u32>,
        #[serde(default)]
        created_at: Option<i64>,
    }

    let probe: Probe = match serde_json::from_str(text) {
        Ok(probe) => probe,
        Err(_) => return MetaState::Corrupt("invalid json"),
    };

    let format_version = probe.format_version.unwrap_or(LIBRARY_FORMAT_VERSION);
    if format_version > LIBRARY_FORMAT_VERSION {
        return MetaState::TooNew;
    }
    if format_version < 1 {
        return MetaState::Corrupt("invalid format_version");
    }

    match probe.library_id {
        Some(library_id) if !library_id.trim().is_empty() => MetaState::Valid(LibraryMeta {
            library_id,
            format_version,
            created_at: probe.created_at.unwrap_or_else(now_millis),
        }),
        _ => MetaState::Corrupt("library_id missing or empty"),
    }
}

fn write_meta(path: &Path, meta: &LibraryMeta) -> Result<(), AppError> {
    let bytes = serde_json::to_vec_pretty(meta)
        .map_err(|err| AppError::Internal(format!("serialize library meta: {err}")))?;
    files::write_atomic(path, &bytes)
}

fn create_layout(root: &Path) -> Result<(), AppError> {
    for dir in [
        root.to_path_buf(),
        root.join("originals").join("images"),
        root.join("texts"),
        root.join("thumbs"),
        root.join("tmp"),
        root.join("backups"),
    ] {
        fs::create_dir_all(&dir)
            .map_err(|err| AppError::Io(format!("create dir {}: {err}", dir.display())))?;
    }
    Ok(())
}

fn env_path(name: &str) -> Result<PathBuf, AppError> {
    match std::env::var_os(name) {
        Some(value) if !value.is_empty() => Ok(PathBuf::from(value)),
        _ => {
            tracing::warn!(variable = name, "environment variable missing");
            Err(AppError::ConfigMissing)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inspect_meta_accepts_valid_metadata() {
        let state = inspect_meta(r#"{"library_id":"abc","format_version":1,"created_at":42}"#);
        assert!(matches!(
            state,
            MetaState::Valid(LibraryMeta { library_id, format_version: 1, created_at: 42 }) if library_id == "abc"
        ));
    }

    #[test]
    fn inspect_meta_flags_newer_format() {
        assert!(matches!(
            inspect_meta(r#"{"library_id":"abc","format_version":2,"created_at":0}"#),
            MetaState::TooNew
        ));
    }

    #[test]
    fn inspect_meta_flags_missing_id_and_bad_json() {
        assert!(matches!(
            inspect_meta(r#"{"format_version":1,"created_at":0}"#),
            MetaState::Corrupt(_)
        ));
        assert!(matches!(inspect_meta("not json"), MetaState::Corrupt(_)));
    }

    #[test]
    fn inspect_meta_fills_missing_created_at() {
        let state = inspect_meta(r#"{"library_id":"abc"}"#);
        match state {
            MetaState::Valid(meta) => {
                assert_eq!(meta.format_version, LIBRARY_FORMAT_VERSION);
                assert!(meta.created_at > 0);
            }
            _ => panic!("expected valid metadata"),
        }
    }
}
