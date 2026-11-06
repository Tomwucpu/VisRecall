//! 启动清理（design §3.3 启动第 4 步、§10.2）：清空 `tmp/`，扫描孤儿文件。
//!
//! 原子写残留的 `tmp/*.tmp` 不会被数据库引用（只有 rename 成功后才入库），
//! 进程被强杀后可直接删除，满足 P1-R06 的强杀恢复。
//! `originals/` 与 `texts/` 中的孤儿文件只计数并记录日志，不自动删除
//! （阶段 02 提供清理入口，P1-R10）。

use std::collections::HashSet;
use std::fs;
use std::path::{Component, Path};

use visrecall_domain::AppError;

use crate::db::{db_error, Library};

/// 启动清理结果。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StartupCleanup {
    /// 从 `tmp/` 删除的文件数。
    pub tmp_removed: u64,
    /// `originals/`、`texts/` 中未被数据库引用（未删除）的文件数。
    pub orphans: u64,
}

/// 扫描孤儿文件的目录（相对库根目录）。
///
/// 白名单是硬约束：孤儿扫描只认这两个目录，绝不遍历库根目录，也不触碰
/// `visrecall.db` / `-wal` / `-shm`、`library.json`、`backups/`、`tmp/`。
pub(crate) const ORPHAN_SCAN_DIRS: [&str; 2] = ["originals", "texts"];

/// 存放素材内容的目录（全库清空时清空其下文件，但保留目录本身）。
pub(crate) const CONTENT_DIRS: [&str; 3] = ["originals", "texts", "thumbs"];

/// `name` 是否是内容目录之一。
pub(crate) fn is_content_dir(name: &str) -> bool {
    CONTENT_DIRS.contains(&name)
}

impl Library {
    /// 启动清理：清空 `tmp/` 下的普通文件，再统计孤儿文件。
    ///
    /// - `tmp/` 不存在时跳过（删除计数为 0）；单个文件删除失败只记录 warn 并继续。
    /// - 孤儿文件只计数 + 记录日志，不删除。
    /// - 数据库不可读时返回错误（调用方 `Library::open` 只记录 warn，不影响打开库）。
    pub fn cleanup_startup(&self) -> Result<StartupCleanup, AppError> {
        let tmp_removed = clear_tmp(&self.root().join("tmp"));

        let referenced = self.referenced_original_paths()?;
        let mut orphans = 0;
        for dir in ORPHAN_SCAN_DIRS {
            let base = self.root().join(dir);
            for path in walk_files(&base) {
                let Some(relative) = relative_slash_path(self.root(), &path) else {
                    continue;
                };
                if !referenced.contains(&relative) {
                    orphans += 1;
                    tracing::warn!(path = %relative, "orphan file not referenced by library");
                }
            }
        }

        Ok(StartupCleanup {
            tmp_removed,
            orphans,
        })
    }

    /// `items.original_path` 的集合，统一成 `/` 分隔的相对路径。
    fn referenced_original_paths(&self) -> Result<HashSet<String>, AppError> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare("SELECT original_path FROM items")
            .map_err(db_error)?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(db_error)?;

        let mut paths = HashSet::new();
        for row in rows {
            paths.insert(normalize_db_path(&row.map_err(db_error)?));
        }
        Ok(paths)
    }
}

/// 删除 `tmp_dir` 下所有普通文件并返回删除计数；目录不存在返回 0。
fn clear_tmp(tmp_dir: &Path) -> u64 {
    let mut removed = 0;
    for path in walk_files(tmp_dir) {
        match fs::remove_file(&path) {
            Ok(()) => removed += 1,
            Err(err) => {
                tracing::warn!(path = %path.display(), %err, "failed to remove tmp file");
            }
        }
    }
    removed
}

/// 递归收集 `base` 下的普通文件；目录不存在或不可读时返回空列表（只记录 warn）。
///
/// 与 `storage` 的用量统计、`purge` 的清空共用：跳过符号链接，既不跟随也不删除，
/// 避免越出库根目录。
pub(crate) fn walk_files(base: &Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![base.to_path_buf()];

    while let Some(dir) = stack.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => {
                tracing::warn!(path = %dir.display(), %err, "failed to read directory");
                continue;
            }
        };

        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(err) => {
                    tracing::warn!(path = %dir.display(), %err, "failed to read directory entry");
                    continue;
                }
            };
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(err) => {
                    tracing::warn!(path = %entry.path().display(), %err, "failed to stat entry");
                    continue;
                }
            };
            // 符号链接既不跟随也不删除，避免越出库根目录。
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                stack.push(entry.path());
            } else if file_type.is_file() {
                files.push(entry.path());
            }
        }
    }

    files
}

/// `path` 相对 `root` 的路径，分隔符统一为 `/`；无法表示时返回 `None`。
pub(crate) fn relative_slash_path(root: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(root).ok()?;
    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
            _ => return None,
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// 数据库里的相对路径统一成 `/` 分隔、去掉开头的 `./`。
pub(crate) fn normalize_db_path(raw: &str) -> String {
    raw.replace('\\', "/").trim_start_matches("./").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_db_path_uses_forward_slashes() {
        assert_eq!(
            normalize_db_path("originals\\images\\2026\\11\\a.png"),
            "originals/images/2026/11/a.png"
        );
        assert_eq!(normalize_db_path("./texts/a.txt"), "texts/a.txt");
    }

    #[test]
    fn relative_slash_path_rejects_foreign_paths() {
        let root = Path::new("C:\\library");
        assert_eq!(
            relative_slash_path(root, &root.join("texts").join("a.txt")).as_deref(),
            Some("texts/a.txt")
        );
        assert!(relative_slash_path(root, Path::new("C:\\other\\a.txt")).is_none());
    }

    #[test]
    fn clear_tmp_ignores_missing_directory() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(clear_tmp(&dir.path().join("tmp")), 0);
    }
}
