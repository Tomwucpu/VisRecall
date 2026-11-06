//! 库位置迁移：目标校验、复制与复制后校验（design §6.9）。
//!
//! 三条纪律：
//! 1. **只复制主库**：`checkpoint(TRUNCATE)` 之后 `visrecall.db` 已是完整快照，
//!    `-wal` / `-shm` 不复制（复制它们反而会把旧的 WAL 带回新库）；
//! 2. **连接纪律**：`checkpoint` 之前不新开任何连接；校验只对**目标库**开只读连接，
//!    源库的行数与 `library_id` 由调用方用现有写连接读出后传进来；
//! 3. **幂等复制**：目标已存在且大小相同就跳过，天然支持「中断后重试」。

use std::fs;
use std::path::{Component, Path, PathBuf};

use rusqlite::{Connection, OpenFlags};
use visrecall_domain::storage::RelocateReport;
use visrecall_domain::AppError;

use crate::db::DB_FILE;
use crate::files::free_space_bytes;
use crate::paths::LIBRARY_META_FILE;

/// 复制顺序：先落库元数据与主库，再搬内容目录。
const COPY_ORDER: [&str; 5] = ["library.json", DB_FILE, "originals", "texts", "thumbs"];

/// 目标卷需要的余量系数（源库大小 × 1.1）。
const SPACE_MARGIN: f64 = 1.1;

/// 复制进度回调：`(phase, done, total)`。
pub type ProgressFn<'a> = dyn Fn(&str, u64, u64) + 'a;

/// 校验迁移目标是否可用。
///
/// 拒绝条件（任一不满足即 `INVALID_INPUT`，并说明原因）：
/// - 目标必须是绝对路径；
/// - 目标不能等于、包含或被包含于当前库（用路径**组件**比较，`C:\lib` 与 `C:\lib2`
///   是不同目录）；
/// - 目标要么不存在，要么是空目录；
/// - 目标卷可用空间 ≥ `required_bytes × 1.1`。
pub fn validate_target(src: &Path, dst: &Path, required_bytes: u64) -> Result<(), AppError> {
    if !dst.is_absolute() {
        return Err(AppError::InvalidInput(
            "target path must be absolute".into(),
        ));
    }
    if is_within(dst, src) {
        return Err(AppError::InvalidInput(
            "target must not be inside the current library".into(),
        ));
    }
    if is_within(src, dst) {
        return Err(AppError::InvalidInput(
            "current library must not be inside the target".into(),
        ));
    }
    if paths_equal(src, dst) {
        return Err(AppError::InvalidInput(
            "target must differ from the current library".into(),
        ));
    }

    if dst.exists() {
        if !dst.is_dir() {
            return Err(AppError::InvalidInput(
                "target exists and is not a directory".into(),
            ));
        }
        let mut entries = fs::read_dir(dst)
            .map_err(|err| AppError::Io(format!("read {}: {err}", dst.display())))?;
        if entries.next().is_some() {
            return Err(AppError::InvalidInput(
                "target directory is not empty".into(),
            ));
        }
    }

    let required = (required_bytes as f64 * SPACE_MARGIN) as u64;
    let free = free_space_bytes(dst)?;
    if free < required {
        return Err(AppError::InvalidInput(format!(
            "target volume has {free} bytes free, {required} required"
        )));
    }
    Ok(())
}

/// 复制整个库；返回复制了多少文件、多少字节（跳过的文件不计入）。
pub fn copy_library(
    src: &Path,
    dst: &Path,
    progress: &ProgressFn<'_>,
) -> Result<RelocateReport, AppError> {
    progress("prepare", 0, 0);
    let plan = collect_copy_plan(src)?;
    let total = plan.len() as u64;
    let mut copied = 0u64;
    let mut bytes = 0u64;

    for (index, relative) in plan.iter().enumerate() {
        let from = src.join(relative);
        let to = dst.join(relative);
        let size = fs::metadata(&from).map(|meta| meta.len()).unwrap_or(0);

        // 已存在且大小一致 → 视为上次中断时已经复制过（天然续传）。
        let skip = fs::metadata(&to)
            .map(|meta| meta.len() == size)
            .unwrap_or(false);
        if !skip {
            copy_file(&from, &to)?;
            copied += 1;
            bytes += size;
        }

        // 每 64 个文件或最后一个发一次进度，避免刷爆事件通道。
        if index % 64 == 0 || index as u64 + 1 == total {
            progress("copy", index as u64 + 1, total);
        }
    }

    Ok(RelocateReport {
        new_root: dst.display().to_string(),
        files_copied: copied,
        bytes_copied: bytes,
    })
}

/// 校验目标库可用：能打开、`integrity_check = ok`、`items` 行数与 `library_id` 一致。
///
/// 只对目标库打开**只读**连接；源库的数据由调用方用现有连接读出后传进来。
pub fn verify(dst: &Path, expected_items: u64, expected_library_id: &str) -> Result<(), AppError> {
    let db = dst.join(DB_FILE);
    let conn = Connection::open_with_flags(
        &db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .map_err(|err| AppError::Database(format!("open {}: {err}", db.display())))?;

    let integrity: String = conn
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .map_err(|err| AppError::Database(format!("integrity_check: {err}")))?;
    if integrity != "ok" {
        return Err(AppError::Io(format!(
            "integrity check failed for {}: {integrity}",
            db.display()
        )));
    }

    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM items", [], |row| row.get(0))
        .map_err(|err| AppError::Database(format!("count items: {err}")))?;
    if count as u64 != expected_items {
        return Err(AppError::Io(format!(
            "item count mismatch: target has {count}, source has {expected_items}"
        )));
    }

    let meta_path = dst.join(LIBRARY_META_FILE);
    let text = fs::read_to_string(&meta_path)
        .map_err(|err| AppError::Io(format!("read {}: {err}", meta_path.display())))?;
    let meta: crate::paths::LibraryMeta = serde_json::from_str(&text)
        .map_err(|err| AppError::Io(format!("parse {}: {err}", meta_path.display())))?;
    if meta.library_id != expected_library_id {
        return Err(AppError::Io(format!(
            "library_id mismatch: target {} vs source {expected_library_id}",
            meta.library_id
        )));
    }
    Ok(())
}

/// `child` 是否位于 `parent` 之内（含相等）。
///
/// 按**路径组件**比较而不是字符串前缀：`C:\lib2` 不是 `C:\lib` 的子目录。
pub fn is_within(child: &Path, parent: &Path) -> bool {
    let child_parts = components(child);
    let parent_parts = components(parent);
    if child_parts.len() < parent_parts.len() {
        return false;
    }
    child_parts
        .iter()
        .zip(&parent_parts)
        .all(|(left, right)| left == right)
}

fn paths_equal(left: &Path, right: &Path) -> bool {
    components(left) == components(right)
}

/// 归一化后的路径组件（忽略 `.`、大小写不敏感、去掉末尾分隔符）。
fn components(path: &Path) -> Vec<String> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => {
                parts.push(prefix.as_os_str().to_string_lossy().to_lowercase())
            }
            Component::RootDir => parts.push("\\".to_string()),
            Component::Normal(part) => parts.push(part.to_string_lossy().to_lowercase()),
            Component::CurDir => {}
            Component::ParentDir => parts.push("..".to_string()),
        }
    }
    parts
}

/// 复制计划：按 [`COPY_ORDER`] 展开的相对路径列表（库元数据 → 主库 → 内容目录）。
fn collect_copy_plan(src: &Path) -> Result<Vec<PathBuf>, AppError> {
    let mut plan = Vec::new();
    for entry in COPY_ORDER {
        let path = src.join(entry);
        if path.is_dir() {
            for file in walk(&path) {
                if let Ok(relative) = file.strip_prefix(src) {
                    plan.push(relative.to_path_buf());
                }
            }
        } else if path.is_file() {
            plan.push(PathBuf::from(entry));
        }
    }
    Ok(plan)
}

/// 递归收集普通文件；跳过符号链接（不跟随、不复制）。
fn walk(base: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![base.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                stack.push(path);
            } else if file_type.is_file() {
                files.push(path);
            }
        }
    }
    files
}

/// 复制单个文件：先写 `<目标>.tmp` 再 rename，避免中断留下半个文件。
fn copy_file(from: &Path, to: &Path) -> Result<(), AppError> {
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| AppError::Io(format!("create dir {}: {err}", parent.display())))?;
    }
    let tmp = to.with_extension("tmp-copy");
    fs::copy(from, &tmp).map_err(|err| {
        AppError::Io(format!(
            "copy {} -> {}: {err}",
            from.display(),
            tmp.display()
        ))
    })?;
    fs::rename(&tmp, to).map_err(|err| {
        let _ = fs::remove_file(&tmp);
        AppError::Io(format!("rename {} -> {}: {err}", tmp.display(), to.display()))
    })
}
