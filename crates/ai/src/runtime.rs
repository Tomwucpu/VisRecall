//! ONNX Runtime 动态加载（`ort` 的 `load-dynamic`）。
//!
//! 为什么不是静态链接：`ort-sys` 预编译的 onnxruntime 静态库用较新的 MSVC STL 构建
//! （依赖 `__std_*_vectorized` 等符号），本机 VS 2022 17.7（MSVC 14.37）的 STL 里没有
//! 这些符号，链接期报 `LNK2019`。改为运行期加载 `onnxruntime.dll` 后既不受本机 MSVC 版本
//! 影响，也让安装包只需附带一个 dll（阶段 04 打包项）。
//!
//! dll 与模型文件一起放在 `models/`（manifest.json 记录 sha256）；解析顺序：
//! `ORT_DYLIB_PATH` 环境变量 → exe 同级目录 → 模型目录候选（`models::candidate_model_dirs`）。

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use ort::environment::Environment;
use ort::session::builder::SessionBuilder;
use visrecall_domain::AppError;

use crate::models::candidate_model_dirs;

/// 动态库文件名（Windows x64）。
pub const ORT_DYLIB: &str = "onnxruntime.dll";
/// 显式指定动态库路径的环境变量（`ort` 自身也识别它）。
pub const ORT_DYLIB_ENV: &str = "ORT_DYLIB_PATH";
/// 每个 session 的 intra-op 线程数（内存与延迟的折中，见 `session_builder`）。
pub const INTRA_THREADS: usize = 2;
/// 每个 session 的 inter-op 线程数。
pub const INTER_THREADS: usize = 1;

static ENV: OnceLock<Environment> = OnceLock::new();
static INIT_LOCK: Mutex<()> = Mutex::new(());

/// 按约定顺序查找 `onnxruntime.dll`。
pub fn resolve_ort_dylib() -> Result<PathBuf, AppError> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(path) = std::env::var_os(ORT_DYLIB_ENV) {
        if !path.is_empty() {
            candidates.push(PathBuf::from(path));
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join(ORT_DYLIB));
            for ancestor in dir.ancestors().skip(1).take(4) {
                candidates.push(ancestor.join(ORT_DYLIB));
            }
        }
    }
    for dir in candidate_model_dirs() {
        candidates.push(dir.join(ORT_DYLIB));
    }
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join(ORT_DYLIB));
    }

    candidates
        .into_iter()
        .find(|path| path.is_file())
        .ok_or_else(|| AppError::FileMissing(format!("{ORT_DYLIB} (models/ or exe dir)")))
}

/// 返回进程级 ONNX Runtime 环境（首次调用时加载 dll，失败不缓存）。
pub fn environment() -> Result<&'static Environment, AppError> {
    if let Some(env) = ENV.get() {
        return Ok(env);
    }
    let _guard = INIT_LOCK
        .lock()
        .map_err(|_| AppError::Internal("ort init lock poisoned".to_string()))?;
    if let Some(env) = ENV.get() {
        return Ok(env);
    }
    let dylib = resolve_ort_dylib()?;
    ENV.set(load(&dylib)?)
        .map_err(|_| AppError::Internal("ort environment already initialized".to_string()))?;
    Ok(ENV.get().expect("environment set above"))
}

/// 统一 session 构造入口：限制线程数 + 关闭内存模式。
///
/// 默认线程池按核数扩张时，ORT 会为每个线程保留 arena，实测 5 张截图后工作集从 52MB
/// 涨到 271MB——远超托盘 150MB 硬上限。2 线程 + 关闭 memory pattern 后内存可控，
/// 延迟仍在门禁内（见 `docs/architecture.md` §5.1）。
pub fn session_builder() -> Result<SessionBuilder, AppError> {
    let env = environment()?;
    let builder = ort::session::Session::builder(env)
        .map_err(|e| AppError::Internal(format!("ort: session builder: {e}")))?;
    builder
        .with_intra_threads(INTRA_THREADS)
        .and_then(|b| b.with_inter_threads(INTER_THREADS))
        .and_then(|b| b.with_memory_pattern(false))
        .map_err(|e| AppError::Internal(format!("ort: session options: {e}")))
}

fn load(dylib: &Path) -> Result<Environment, AppError> {
    let builder = ort::init_from(dylib)
        .map_err(|e| AppError::Internal(format!("ort: failed to load {}: {e}", dylib.display())))?;
    builder
        .with_name("visrecall")
        .build()
        .map_err(|e| AppError::Internal(format!("ort: failed to create environment: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dylib_resolves_from_repo_models_dir() {
        // dev/test 运行时 exe 在 target/debug/deps 下，向上回溯能找到 <repo>/models。
        let path = resolve_ort_dylib().expect("models/onnxruntime.dll 应可定位");
        assert!(path.is_file(), "{} 不是文件", path.display());
        assert_eq!(path.file_name().and_then(|s| s.to_str()), Some(ORT_DYLIB));
    }
}
