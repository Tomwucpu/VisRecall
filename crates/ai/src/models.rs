//! 模型文件定位与 sha256 清单校验（design D3-04）。
//!
//! 解析顺序：`VISRECALL_MODEL_DIR` 覆盖 → 可执行文件同级 `models/`（Tauri resource dir
//! 在 Windows 上等于 exe 目录）→ 可执行文件上级目录逐层 `models/`（dev：`target/debug`
//! 向上即仓库根）→ 当前工作目录 `models/`。不写死任何绝对路径。
//!
//! 校验只在启动时做一次（`verify_manifest`），定位本身不读文件内容——否则每次加载模型
//! 都要重算 10–20MB 的 sha256。

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use visrecall_domain::AppError;

/// 清单文件名。
pub const MANIFEST_FILE: &str = "manifest.json";
/// 显式指定模型目录的环境变量（T0 spike、测试与阶段 04 打包使用）。
pub const MODEL_DIR_ENV: &str = "VISRECALL_MODEL_DIR";
/// 向上回溯查找 `models/` 的层数上限。
const ANCESTOR_DEPTH: usize = 4;

/// `models/manifest.json` 的结构：文件名 → sha256，另附来源与许可备注（人工维护）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelManifest {
    #[serde(default)]
    pub files: BTreeMap<String, String>,
    /// 文件名 → 下载来源 URL（取证与复现用，不参与校验）。
    #[serde(default)]
    pub sources: BTreeMap<String, String>,
    /// 文件名 → 许可备注（不参与校验）。
    #[serde(default)]
    pub licenses: BTreeMap<String, String>,
}

impl ModelManifest {
    pub fn file_names(&self) -> impl Iterator<Item = &String> {
        self.files.keys()
    }
}

/// 按约定顺序返回候选目录（去重、保序）。
pub fn candidate_model_dirs() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let mut push = |p: PathBuf| {
        if !out.contains(&p) {
            out.push(p);
        }
    };

    if let Some(dir) = std::env::var_os(MODEL_DIR_ENV) {
        if !dir.is_empty() {
            push(PathBuf::from(dir));
        }
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            push(exe_dir.join("models"));
            for ancestor in exe_dir.ancestors().skip(1).take(ANCESTOR_DEPTH) {
                push(ancestor.join("models"));
            }
        }
    }

    if let Ok(cwd) = std::env::current_dir() {
        push(cwd.join("models"));
    }

    out
}

/// 在给定候选目录中找出第一个含 `manifest.json` 的目录。
pub fn locate_model_dir(candidates: &[PathBuf]) -> Option<PathBuf> {
    candidates
        .iter()
        .find(|dir| dir.join(MANIFEST_FILE).is_file())
        .cloned()
}

/// 定位模型目录（只判断清单是否存在，不校验内容）。
pub fn resolve_model_dir() -> Result<PathBuf, AppError> {
    locate_model_dir(&candidate_model_dirs()).ok_or(AppError::ConfigMissing)
}

/// 定位并校验模型目录：清单存在且所有文件 sha256 一致。
///
/// 供启动自检使用：失败时调用方记 `warn` 并走兜底路径（Windows.Media.Ocr / FTS-only）。
pub fn resolve_verified_model_dir() -> Result<(PathBuf, ModelManifest), AppError> {
    let dir = resolve_model_dir()?;
    let manifest = verify_manifest(&dir)?;
    Ok((dir, manifest))
}

/// 清单文件路径。
pub fn manifest_path(dir: &Path) -> PathBuf {
    dir.join(MANIFEST_FILE)
}

/// 模型文件路径。
pub fn model_file(dir: &Path, name: &str) -> PathBuf {
    dir.join(name)
}

/// 读取清单（不校验）。
pub fn load_manifest(dir: &Path) -> Result<ModelManifest, AppError> {
    let path = manifest_path(dir);
    let raw = fs::read(&path).map_err(|_| AppError::FileMissing(path.display().to_string()))?;
    serde_json::from_slice(&raw)
        .map_err(|e| AppError::InvalidInput(format!("models/manifest.json: {e}")))
}

/// 逐文件校验：清单里列出的文件必须存在且 sha256 一致；未列出的文件忽略。
pub fn verify_manifest(dir: &Path) -> Result<ModelManifest, AppError> {
    let manifest = load_manifest(dir)?;
    if manifest.files.is_empty() {
        return Err(AppError::InvalidInput(
            "models/manifest.json: no files listed".to_string(),
        ));
    }
    for (name, expected) in &manifest.files {
        let path = model_file(dir, name);
        let actual = sha256_file(&path)?;
        if !actual.eq_ignore_ascii_case(expected) {
            return Err(AppError::Internal(format!(
                "model checksum mismatch: {name} (expected {expected}, got {actual})"
            )));
        }
    }
    Ok(manifest)
}

/// 文件 sha256（小写十六进制）。
pub fn sha256_file(path: &Path) -> Result<String, AppError> {
    let bytes = fs::read(path).map_err(|_| AppError::FileMissing(path.display().to_string()))?;
    Ok(sha256_hex(&bytes))
}

/// 字节串 sha256（小写十六进制）。
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_known_vector() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn locate_returns_first_dir_with_manifest() {
        let root = std::env::temp_dir().join(format!(
            "visrecall-models-locate-{}",
            visrecall_domain::new_item_id()
        ));
        let a = root.join("a");
        let b = root.join("b");
        fs::create_dir_all(&a).expect("mkdir a");
        fs::create_dir_all(&b).expect("mkdir b");
        fs::write(b.join(MANIFEST_FILE), br#"{"files":{}}"#).expect("write manifest");

        assert_eq!(locate_model_dir(&[a.clone(), b.clone()]), Some(b.clone()));
        assert_eq!(locate_model_dir(&[a]), None);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn candidate_dirs_are_unique_and_ordered() {
        let dirs = candidate_model_dirs();
        let mut seen = std::collections::BTreeSet::new();
        for dir in &dirs {
            assert!(
                seen.insert(dir.clone()),
                "候选目录不应重复：{}",
                dir.display()
            );
        }
    }
}
