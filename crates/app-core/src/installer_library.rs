//! 首次启动时验证并保存安装器传入的素材库路径。

use std::fs;
use std::path::{Component, Path, PathBuf, Prefix};

use visrecall_domain::AppError;
use visrecall_library::db::DB_FILE;
use visrecall_library::paths::{AppPaths, LIBRARY_META_FILE};
use visrecall_library::AppConfig;
use windows::core::PCWSTR;
use windows::Win32::Storage::FileSystem::GetDriveTypeW;
use windows::Win32::System::WindowsProgramming::{DRIVE_FIXED, DRIVE_REMOVABLE};

pub(super) fn prepare(
    paths: &AppPaths,
    install_dir: &Path,
    pending_path: impl FnOnce() -> Result<Option<PathBuf>, AppError>,
) -> Result<bool, AppError> {
    let pending = pending_path();
    if matches!(pending, Ok(None)) {
        return Ok(false);
    }
    // 在读取配置前检查文件是否存在，避免坏配置被备份后误用安装器的旧选择。
    if exists(&paths.config_file)?
        || exists(&paths.default_library_root.join(LIBRARY_META_FILE))?
        || exists(&paths.default_library_root.join(DB_FILE))?
    {
        return Ok(matches!(pending, Ok(Some(_))));
    }
    let Some(path) = pending? else {
        return Ok(false);
    };

    validate(&path, install_dir)?;
    AppConfig {
        library_path: path.clone(),
        ..AppConfig::default()
    }
    .save_atomic(&paths.config_file)
    .map_err(|err| AppError::Io(format!("无法保存素材库「{}」的配置：{err}", path.display())))?;
    Ok(true)
}

fn exists(path: &Path) -> Result<bool, AppError> {
    path.try_exists()
        .map_err(|err| AppError::Io(format!("无法检查「{}」：{err}", path.display())))
}

fn validate(path: &Path, install_dir: &Path) -> Result<(), AppError> {
    let invalid = |reason: &str| {
        AppError::InvalidInput(format!("安装时选择的素材库「{}」{reason}", path.display()))
    };
    let io = |err| {
        AppError::Io(format!(
            "无法访问安装时选择的素材库「{}」：{err}",
            path.display()
        ))
    };
    if !path.is_absolute() || drive_letter(path).is_none() {
        return Err(invalid("必须使用本地绝对路径"));
    }
    // 安装器已创建新目录；目录后来失效时由用户恢复，当前启动不能另建库。
    let resolved = fs::canonicalize(path).map_err(io)?;
    let Some(drive) = drive_letter(&resolved) else {
        return Err(invalid("必须位于本地磁盘"));
    };
    let root = [u16::from(drive), b':' as u16, b'\\' as u16, 0];
    // SAFETY: 盘符根路径以 NUL 结尾，并在调用期间有效。
    let drive_type = unsafe { GetDriveTypeW(PCWSTR(root.as_ptr())) };
    if !matches!(drive_type, DRIVE_FIXED | DRIVE_REMOVABLE) {
        return Err(invalid("必须位于本地磁盘"));
    }

    let install = fs::canonicalize(install_dir).map_err(|err| {
        AppError::Io(format!(
            "无法检查程序目录「{}」：{err}",
            install_dir.display()
        ))
    })?;
    if resolved.starts_with(&install) || install.starts_with(&resolved) {
        return Err(invalid("不能与程序目录重叠"));
    }

    let mut entries = fs::read_dir(&resolved).map_err(io)?;
    if entries.next().transpose().map_err(io)?.is_some() {
        return Err(invalid("必须是空目录"));
    }
    // 关闭临时文件时自动删除探针，库初始化前仍是空目录。
    tempfile::tempfile_in(&resolved).map_err(io)?;
    Ok(())
}

fn drive_letter(path: &Path) -> Option<u8> {
    match path.components().next()? {
        Component::Prefix(prefix) => match prefix.kind() {
            Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) => Some(drive),
            _ => None,
        },
        _ => None,
    }
}
