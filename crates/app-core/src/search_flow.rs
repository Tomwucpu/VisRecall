//! 查询侧编排的基础设施：查询嵌入、语义就绪状态、降级标注（design D3-05）。
//!
//! 分工：向量化本身在 `domain::providers::EmbeddingProvider`（实现是 `ai::BgeEmbedder`，
//! 自带 LRU 256 查询缓存）；本模块负责「就绪判定 + 事件驱动的降级标注 + 预热」，
//! 以及给 `search::execute` 提供查询向量（未就绪时返回 `None`，语义通道跳过）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use visrecall_domain::providers::EmbeddingProvider;

use crate::events::{AppEvent, EventSink, SemanticStatusEvent};

/// 预热用的固定文本（内容无关，只为触发模型加载）。
const WARMUP_TEXT: &str = "预热";

/// 查询嵌入与语义就绪状态。
pub struct SearchFlow {
    embed: Arc<dyn EmbeddingProvider>,
    /// `None` = 异步运行时创建失败：语义通道永久降级（不 panic）。
    runtime: Option<tokio::runtime::Handle>,
    sink: Arc<dyn EventSink>,
    ready: Arc<AtomicBool>,
    announced_degraded: AtomicBool,
}

impl SearchFlow {
    pub fn new(
        embed: Arc<dyn EmbeddingProvider>,
        runtime: Option<tokio::runtime::Handle>,
        sink: Arc<dyn EventSink>,
    ) -> Arc<Self> {
        Arc::new(Self {
            embed,
            runtime,
            sink,
            ready: Arc::new(AtomicBool::new(false)),
            announced_degraded: AtomicBool::new(false),
        })
    }

    pub fn provider(&self) -> Arc<dyn EmbeddingProvider> {
        Arc::clone(&self.embed)
    }

    /// 语义通道是否可用（`semantic_ready` 的来源）。
    pub fn semantic_ready(&self) -> bool {
        self.ready.load(Ordering::SeqCst) && self.embed.ready()
    }

    /// 后台预热：加载模型 → 发 `semantic_status{ready}`（失败则保持降级，只记日志）。
    pub fn warm_up(self: &Arc<Self>) {
        let this = Arc::clone(self);
        let spawned = std::thread::Builder::new()
            .name("visrecall-embed-warmup".to_string())
            .spawn(move || {
                let Some(runtime) = this.runtime.clone() else {
                    tracing::warn!("async runtime unavailable; semantic channel stays degraded");
                    return;
                };
                let started = std::time::Instant::now();
                let result = runtime.block_on(this.embed.embed(&[WARMUP_TEXT.to_string()]));
                let ready = this.embed.ready() && result.is_ok();
                this.ready.store(ready, Ordering::SeqCst);
                match result {
                    Ok(_) => tracing::info!(
                        elapsed_ms = started.elapsed().as_millis(),
                        ready,
                        "embedding model warmed up"
                    ),
                    Err(err) => {
                        tracing::warn!(%err, "embedding warm-up failed; semantic channel degraded")
                    }
                }
                this.sink
                    .emit(AppEvent::SemanticStatus(SemanticStatusEvent { ready }));
            });
        if let Err(err) = spawned {
            tracing::warn!(%err, "failed to spawn embedding warm-up thread");
        }
    }

    /// 执行一次检索（三路召回 + RRF + 解释；`limit` 已由调用方 clamp）。
    pub fn run(
        &self,
        library: &visrecall_library::Library,
        vectors: &dyn visrecall_domain::providers::VectorIndex,
        raw: &str,
        limit: u32,
        filters: &visrecall_domain::ItemFilters,
    ) -> Result<visrecall_search::SearchOutcome, visrecall_domain::AppError> {
        visrecall_search::execute::search(
            visrecall_search::SearchDeps {
                library,
                vectors,
                embed: Some(self.embed.as_ref()),
                runtime: self.runtime.as_ref(),
                rrf: visrecall_search::RrfConfig::from_settings(library),
            },
            visrecall_search::SearchRequest::new(raw, limit).with_filters(filters.clone()),
        )
    }

    /// 查询向量：未就绪返回 `None`（并确保降级事件已发过一次）。
    pub fn embed_query(&self, text: &str) -> Option<Vec<f32>> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return None;
        }
        if !self.embed.ready() {
            if !self.announced_degraded.swap(true, Ordering::SeqCst) {
                self.sink
                    .emit(AppEvent::SemanticStatus(SemanticStatusEvent {
                        ready: false,
                    }));
            }
            return None;
        }
        let runtime = self.runtime.clone()?;
        self.ready.store(true, Ordering::SeqCst);
        match runtime.block_on(self.embed.embed(&[trimmed.to_string()])) {
            Ok(mut vectors) => vectors.pop(),
            Err(err) => {
                tracing::warn!(%err, "query embedding failed; semantic channel skipped");
                None
            }
        }
    }
}
