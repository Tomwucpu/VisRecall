//! bge-small-zh-v1.5（ONNX int8，512 维）嵌入实现。
//!
//! 生命周期（design D3-05）：懒加载 + 启动后台预热；未就绪时 `ready()` 为假，
//! 检索侧据此跳过语义通道并置 `semantic_ready=false`。加载失败进入 `Failed` 终态，
//! 后续调用直接返回错误（不 panic、不无限重试）。
//!
//! 池化：取 `last_hidden_state[:,0,:]`（CLS）+ L2 归一化——bge 系列的官方用法。
//! 查询缓存：`Mutex<LruCache<String, Vec<f32>>>`，容量 256，命中不重算。

use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Mutex, OnceLock};

use lru::LruCache;
use ort::session::Session;
use ort::value::Tensor;
use tokenizers::Tokenizer;
use visrecall_domain::providers::EmbeddingProvider;
use visrecall_domain::AppError;

use crate::runtime::session_builder;

/// 写入 `settings['embedding.model_id']` 与向量表 `model_id`。
pub const MODEL_ID: &str = "bge-small-zh-v1.5-int8";
/// 向量维度。
pub const DIM: usize = 512;
/// 单条文本的 token 上限（bge-small 的 `max_position_embeddings`）。
pub const MAX_TOKENS: usize = 512;
/// 查询向量缓存容量。
pub const CACHE_CAPACITY: usize = 256;

/// 模型与分词器文件名（`models/manifest.json` 同名）。
pub const MODEL_FILE: &str = "bge-small-zh-v1.5-int8.onnx";
pub const TOKENIZER_FILE: &str = "tokenizer.json";

const STATE_NOT_READY: u8 = 0;
const STATE_LOADING: u8 = 1;
const STATE_READY: u8 = 2;
const STATE_FAILED: u8 = 3;

/// bge 嵌入 Provider。
pub struct BgeEmbedder {
    model_dir: PathBuf,
    state: AtomicU8,
    session: OnceLock<Mutex<Session>>,
    tokenizer: OnceLock<Tokenizer>,
    load_lock: Mutex<()>,
    cache: Mutex<LruCache<String, Vec<f32>>>,
}

impl std::fmt::Debug for BgeEmbedder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BgeEmbedder")
            .field("model_dir", &self.model_dir)
            .field("ready", &self.ready())
            .finish()
    }
}

impl BgeEmbedder {
    /// 构造但不加载模型（加载发生在首次 `embed` 或显式 `warm_up`）。
    pub fn new(model_dir: impl Into<PathBuf>) -> Self {
        Self {
            model_dir: model_dir.into(),
            state: AtomicU8::new(STATE_NOT_READY),
            session: OnceLock::new(),
            tokenizer: OnceLock::new(),
            load_lock: Mutex::new(()),
            cache: Mutex::new(LruCache::new(
                NonZeroUsize::new(CACHE_CAPACITY).expect("capacity > 0"),
            )),
        }
    }

    pub fn model_dir(&self) -> &Path {
        &self.model_dir
    }

    /// 预热：启动后台线程调用，失败只记状态（`ready()` 保持 false）。
    pub fn warm_up(&self) -> Result<(), AppError> {
        self.ensure_loaded()
    }

    /// 同步嵌入（单条 + 批量都走这里；批量在内部逐条推理）。
    pub fn embed_sync(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, AppError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        self.ensure_loaded()?;
        let mut out = Vec::with_capacity(texts.len());
        for text in texts {
            if let Some(hit) = self.cache_get(text) {
                out.push(hit);
                continue;
            }
            let vec = self.embed_one(text)?;
            self.cache_put(text, &vec);
            out.push(vec);
        }
        Ok(out)
    }

    /// 单条文本嵌入（不走缓存）。
    pub fn embed_one(&self, text: &str) -> Result<Vec<f32>, AppError> {
        self.ensure_loaded()?;
        let tokenizer = self
            .tokenizer
            .get()
            .ok_or_else(|| AppError::Internal("embedder: tokenizer not loaded".to_string()))?;
        let encoding = tokenizer
            .encode(text, true)
            .map_err(|e| AppError::Internal(format!("embedder: tokenize: {e}")))?;
        let ids_full = encoding.get_ids();
        let len = ids_full.len().clamp(1, MAX_TOKENS);
        let ids: Vec<i64> = ids_full[..len].iter().map(|&v| i64::from(v)).collect();
        let mask: Vec<i64> = vec![1; ids.len()];
        let types: Vec<i64> = vec![0; ids.len()];
        let seq = ids.len() as i64;

        let session = self
            .session
            .get()
            .ok_or_else(|| AppError::Internal("embedder: session not loaded".to_string()))?;
        let input_ids = Tensor::from_array((vec![1i64, seq], ids))
            .map_err(|e| AppError::Internal(format!("embedder: ids tensor: {e}")))?;
        let attention = Tensor::from_array((vec![1i64, seq], mask))
            .map_err(|e| AppError::Internal(format!("embedder: mask tensor: {e}")))?;
        let token_types = Tensor::from_array((vec![1i64, seq], types))
            .map_err(|e| AppError::Internal(format!("embedder: types tensor: {e}")))?;

        let mut session = session
            .lock()
            .map_err(|_| AppError::Internal("embedder: session poisoned".to_string()))?;
        let outputs = session
            .run(ort::inputs![
                "input_ids" => input_ids,
                "attention_mask" => attention,
                "token_type_ids" => token_types
            ])
            .map_err(|e| AppError::Internal(format!("embedder: run: {e}")))?;
        let (shape, data) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| AppError::Internal(format!("embedder: output: {e}")))?;
        let dim = *shape.get(2).unwrap_or(&0) as usize;
        if dim != DIM || data.len() < DIM {
            return Err(AppError::Internal(format!(
                "embedder: unexpected output shape {shape:?}"
            )));
        }
        let mut vec: Vec<f32> = data[..DIM].to_vec();
        l2_normalize(&mut vec);
        Ok(vec)
    }

    fn ensure_loaded(&self) -> Result<(), AppError> {
        match self.state.load(Ordering::Acquire) {
            STATE_READY => return Ok(()),
            STATE_FAILED => {
                return Err(AppError::Internal(
                    "embedder: model failed to load (see earlier warning)".to_string(),
                ))
            }
            _ => {}
        }
        let _guard = self
            .load_lock
            .lock()
            .map_err(|_| AppError::Internal("embedder: load lock poisoned".to_string()))?;
        match self.state.load(Ordering::Acquire) {
            STATE_READY => return Ok(()),
            STATE_FAILED => {
                return Err(AppError::Internal(
                    "embedder: model failed to load (see earlier warning)".to_string(),
                ))
            }
            _ => {}
        }
        self.state.store(STATE_LOADING, Ordering::Release);
        match self.load_model() {
            Ok(()) => {
                self.state.store(STATE_READY, Ordering::Release);
                Ok(())
            }
            Err(err) => {
                self.state.store(STATE_FAILED, Ordering::Release);
                Err(err)
            }
        }
    }

    fn load_model(&self) -> Result<(), AppError> {
        let model_path = self.model_dir.join(MODEL_FILE);
        let tokenizer_path = self.model_dir.join(TOKENIZER_FILE);
        for path in [&model_path, &tokenizer_path] {
            if !path.is_file() {
                return Err(AppError::FileMissing(path.display().to_string()));
            }
        }
        let tokenizer = Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| AppError::Internal(format!("embedder: tokenizer: {e}")))?;
        let session = session_builder()?
            .commit_from_file(&model_path)
            .map_err(|e| AppError::Internal(format!("embedder: session: {e}")))?;
        let _ = self.tokenizer.set(tokenizer);
        let _ = self.session.set(Mutex::new(session));
        Ok(())
    }

    fn cache_get(&self, text: &str) -> Option<Vec<f32>> {
        self.cache
            .lock()
            .ok()
            .and_then(|mut cache| cache.get(text).cloned())
    }

    fn cache_put(&self, text: &str, vec: &[f32]) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.put(text.to_string(), vec.to_vec());
        }
    }
}

#[async_trait::async_trait]
impl EmbeddingProvider for BgeEmbedder {
    fn id(&self) -> &str {
        MODEL_ID
    }

    fn dim(&self) -> usize {
        DIM
    }

    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, AppError> {
        self.embed_sync(texts)
    }

    fn ready(&self) -> bool {
        self.state.load(Ordering::Acquire) == STATE_READY
    }
}

fn l2_normalize(vec: &mut [f32]) {
    let norm = vec.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm > 1e-12 {
        for v in vec.iter_mut() {
            *v /= norm;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_ready_until_loaded() {
        let embedder = BgeEmbedder::new("Z:/definitely/missing");
        assert!(!embedder.ready());
    }

    #[test]
    fn missing_model_reports_error_and_marks_failed() {
        let embedder = BgeEmbedder::new("Z:/definitely/missing");
        let err = embedder.embed_one("你好").expect_err("模型缺失必须报错");
        assert!(
            matches!(err, AppError::FileMissing(_)),
            "应为 FileMissing，实际 {err:?}"
        );
        assert!(!embedder.ready());
        // 失败后再次调用仍然是错误，不 panic
        assert!(embedder.embed_one("你好").is_err());
    }

    #[test]
    fn empty_input_is_noop() {
        let embedder = BgeEmbedder::new("Z:/definitely/missing");
        assert!(embedder.embed_sync(&[]).expect("空批量应返回空").is_empty());
    }

    #[test]
    fn l2_normalize_gives_unit_norm() {
        let mut v = vec![3.0, 4.0];
        l2_normalize(&mut v);
        assert!((v[0] - 0.6).abs() < 1e-6);
        assert!((v[1] - 0.8).abs() < 1e-6);

        let mut zero = vec![0.0, 0.0];
        l2_normalize(&mut zero);
        assert_eq!(zero, vec![0.0, 0.0]);
    }
}
