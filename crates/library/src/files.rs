//! 可复用的文件写入工具：临时文件 + `sync_all` + `rename` 原子替换。
//!
//! 配置与 `library.json` 都走这里；图片/文本落盘（阶段 01 Task 14）也会复用。
//! 另外提供磁盘可用空间查询（[`free_space_bytes`]），供捕获前的最低空间门禁使用。

use std::fs;
use std::io::Write;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use visrecall_domain::{now_millis, AppError};
use windows::core::PCWSTR;
use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

fn io_error(operation: &str, path: &Path, err: impl std::fmt::Display) -> AppError {
    AppError::Io(format!("{operation} {}: {err}", path.display()))
}

/// 查询 `path` 所在卷对当前用户可用的空闲字节数（`GetDiskFreeSpaceExW`）。
///
/// `path` 不必已存在：系统按所在卷解析。路径为空、卷不可用或查询失败都返回
/// [`AppError::Io`]，由调用方决定是否阻止捕获（design §5/§12：<2GB 拒绝新捕获）。
pub fn free_space_bytes(path: &Path) -> Result<u64, AppError> {
    if path.as_os_str().is_empty() {
        return Err(AppError::Io("free space: empty path".into()));
    }

    let mut wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // 根目录形式（`C:`）以反斜杠补齐，避免被解释成「当前目录所在卷」。
    if wide.len() == 3 && wide[1] == b':' as u16 {
        wide.insert(2, b'\\' as u16);
    }

    let mut free_bytes: u64 = 0;
    // SAFETY: 字符串以 NUL 结尾且在调用期间存活；其余输出指针是可写的栈上变量。
    unsafe { GetDiskFreeSpaceExW(PCWSTR(wide.as_ptr()), Some(&mut free_bytes), None, None) }
        .map_err(|err| io_error("free space", path, err))?;
    Ok(free_bytes)
}

/// 与 `path` 同目录的临时文件路径（`<文件名>.tmp`）。
pub fn temp_path(path: &Path) -> PathBuf {
    let name = match path.file_name() {
        Some(name) => {
            let mut name = name.to_os_string();
            name.push(".tmp");
            name
        }
        None => std::ffi::OsString::from(".tmp"),
    };
    path.with_file_name(name)
}

/// 把 `bytes` 原子写入 `path`：创建父目录 → 写 `<文件名>.tmp` → `sync_all` → `rename`。
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), AppError> {
    create_parent_dir(path)?;

    let tmp = temp_path(path);
    if let Err(err) = write_tmp(&tmp, bytes) {
        let _ = fs::remove_file(&tmp);
        return Err(err);
    }

    fs::rename(&tmp, path).map_err(|err| {
        let _ = fs::remove_file(&tmp);
        io_error("rename", path, err)
    })
}

/// 把无法解析的文件改名成 `<文件名>.bad-<unix 秒>`，失败只记录日志。
///
/// 返回备份路径；备份失败返回 `None`（调用方不应因此中断恢复流程）。
pub fn backup_corrupt(path: &Path) -> Option<PathBuf> {
    let base = path.file_name()?.to_string_lossy().into_owned();
    let stamp = now_millis() / 1000;

    let mut target = path.with_file_name(format!("{base}.bad-{stamp}"));
    let mut attempt = 1;
    while target.exists() && attempt <= 100 {
        target = path.with_file_name(format!("{base}.bad-{stamp}-{attempt}"));
        attempt += 1;
    }

    match fs::rename(path, &target) {
        Ok(()) => {
            tracing::warn!(from = %path.display(), to = %target.display(), "moved corrupt file aside");
            Some(target)
        }
        Err(err) => {
            tracing::warn!(path = %path.display(), %err, "failed to move corrupt file aside");
            None
        }
    }
}

fn create_parent_dir(path: &Path) -> Result<(), AppError> {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => {
            fs::create_dir_all(parent).map_err(|err| io_error("create dir", parent, err))
        }
        _ => Ok(()),
    }
}

fn write_tmp(tmp: &Path, bytes: &[u8]) -> Result<(), AppError> {
    let mut file = fs::File::create(tmp).map_err(|err| io_error("create", tmp, err))?;
    file.write_all(bytes)
        .map_err(|err| io_error("write", tmp, err))?;
    file.sync_all().map_err(|err| io_error("sync", tmp, err))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_path_appends_suffix_in_same_directory() {
        let path = Path::new("data").join("library.json");
        assert_eq!(temp_path(&path), Path::new("data").join("library.json.tmp"));
    }

    #[test]
    fn write_atomic_replaces_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.json");
        write_atomic(&path, b"one").unwrap();
        write_atomic(&path, b"two").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        assert!(!temp_path(&path).exists());
    }

    #[test]
    fn backup_corrupt_moves_file_aside() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("library.json");
        fs::write(&path, b"broken").unwrap();
        let backup = backup_corrupt(&path).unwrap();
        assert!(!path.exists());
        assert!(backup.exists());
        assert!(backup
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("library.json.bad-"));
    }

    #[test]
    fn free_space_bytes_reports_a_positive_amount() {
        let dir = tempfile::tempdir().unwrap();
        let free = free_space_bytes(dir.path()).unwrap();
        assert!(free > 0, "free space should be positive, got {free}");
    }

    #[test]
    fn free_space_bytes_rejects_empty_path() {
        assert!(matches!(
            free_space_bytes(Path::new("")),
            Err(AppError::Io(_))
        ));
    }
}
