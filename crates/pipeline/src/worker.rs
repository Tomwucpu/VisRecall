//! jobs worker 框架（design §6.2）：claim / 退避 / stale 取消 / 优先级 / 水位事件。
//!
//! 并发模型：N 个线程（默认 2）共用一个 `Arc<Library>`（单写连接 → claim 天然串行）。
//! 每个线程循环「claim 一条 → 执行 handler → 写终态/退避」；空闲 500ms 轮询。
//!
//! 边界纪律（design §7）：claim 事务必须短，**推理绝不持库锁**——handler 内部自己决定
//! 何时开事务（本地推理在锁外做，写结果时用 `write_index_tx` 的短事务）。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use visrecall_domain::providers::{EmbeddingProvider, OcrProvider, TextProvider, VisionProvider};
use visrecall_domain::{now_millis, AppError};
use visrecall_library::{Job, Library};

/// 新捕获链路（`ocr`/`index`/`vlm`）。
pub const PRIORITY_NEW: i64 = 0;
/// 重试（`pending` 素材的补跑）。
pub const PRIORITY_RETRY: i64 = 1;
/// 全量重建/修复。
pub const PRIORITY_REBUILD: i64 = 2;

/// 默认 worker 线程数。
pub const DEFAULT_WORKERS: usize = 2;
/// 空闲轮询间隔。
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(500);
/// 默认退避（秒）：2/5/15。
pub const DEFAULT_BACKOFF_SECS: [u64; 3] = [2, 5, 15];
/// 单任务尝试上限。
pub const DEFAULT_MAX_ATTEMPTS: i64 = 3;
/// 水位事件合并窗口。
pub const DEFAULT_STATUS_DEBOUNCE: Duration = Duration::from_millis(100);

/// Provider 依赖（测试注入假实现，design §9.2）。
pub struct PipelineDeps {
    pub ocr: Arc<dyn OcrProvider>,
    pub embed: Arc<dyn EmbeddingProvider>,
    pub vision: Arc<dyn VisionProvider>,
    pub text: Arc<dyn TextProvider>,
    /// HTTP（VLM）在该 runtime 上 `block_on`；本地推理同步执行。
    pub runtime: tokio::runtime::Handle,
}

impl std::fmt::Debug for PipelineDeps {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PipelineDeps")
            .field("ocr", &self.ocr.id())
            .field("embed", &self.embed.id())
            .field("vision", &self.vision.id())
            .finish()
    }
}

/// 任务框架对外报告的最小事件面；由 `app-core` 适配成 `AppEvent`（避免反向依赖）。
pub trait PipelineEvents: Send + Sync {
    /// `pipeline_status`：队列水位。
    fn pipeline_status(&self, _queued: u64, _running: u64) {}
    /// 阶段一完成（素材已三路可检索）。
    fn item_indexed(&self, _item_id: &str) {}
    /// 阶段二完成（摘要/标签/属性已增强）。
    fn item_enriched(&self, _item_id: &str) {}
    /// 任务进入终态失败（`attempts` 用尽或配置错误）。
    fn job_failed(&self, _job: &Job, _error: &AppError) {}
    /// 素材进入 `pending`（阶段二终态失败，等待重试轮）：适配成 `item_updated`。
    fn item_pending(&self, _item_id: &str) {}
}

/// 丢弃所有事件（测试/一次性场景）。
#[derive(Debug, Default)]
pub struct NoopEvents;

impl PipelineEvents for NoopEvents {}

/// 任务执行上下文。
pub struct JobCtx<'a> {
    pub library: &'a Arc<Library>,
    pub deps: &'a PipelineDeps,
    pub events: &'a Arc<dyn PipelineEvents>,
    pub job: &'a Job,
    /// 单任务尝试上限（handler 据此判断「这次失败后就是终态」）。
    pub max_attempts: i64,
}

impl JobCtx<'_> {
    pub fn stage(&self) -> &str {
        &self.job.stage
    }

    pub fn item_id(&self) -> &str {
        &self.job.item_id
    }

    pub fn revision(&self) -> i64 {
        self.job.revision
    }

    /// 本任务已尝试次数（claim 时 +1，第一次执行即为 1）。
    pub fn attempts(&self) -> i64 {
        self.job.attempts
    }

    /// 本次失败后是否不再重试（`vlm` handler 据此把素材置 `pending`）。
    pub fn is_final_attempt(&self) -> bool {
        self.job.attempts >= self.max_attempts
    }
}

/// 单个阶段的执行器。
pub trait JobHandler: Send + Sync {
    fn stage(&self) -> &'static str;
    fn run(&self, ctx: &JobCtx<'_>) -> Result<(), AppError>;
}

/// worker 行为参数（测试用来压缩等待时间）。
#[derive(Debug, Clone)]
pub struct PipelineConfig {
    pub workers: usize,
    pub poll_interval: Duration,
    pub backoff_secs: Vec<u64>,
    pub max_attempts: i64,
    pub status_debounce: Duration,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            workers: DEFAULT_WORKERS,
            poll_interval: DEFAULT_POLL_INTERVAL,
            backoff_secs: DEFAULT_BACKOFF_SECS.to_vec(),
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            status_debounce: DEFAULT_STATUS_DEBOUNCE,
        }
    }
}

impl PipelineConfig {
    /// 退避时长：第 `attempts` 次失败后用 `backoff[attempts-1]`，超出取最后一档。
    pub fn backoff_for(&self, attempts: i64) -> Duration {
        if self.backoff_secs.is_empty() {
            return Duration::ZERO;
        }
        let index = (attempts.max(1) as usize - 1).min(self.backoff_secs.len() - 1);
        Duration::from_secs(self.backoff_secs[index])
    }
}

/// 运行中的 worker 组。
pub struct PipelineHandle {
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

impl PipelineHandle {
    /// 请求停止并等待线程退出（迁移闸门与测试用）。
    pub fn shutdown(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        for handle in self.threads.drain(..) {
            let _ = handle.join();
        }
    }

    /// 只请求停止、不 join：模拟进程崩溃（线程会跑完手上那条任务后退出）。
    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

impl Drop for PipelineHandle {
    fn drop(&mut self) {
        // 不 join：drop 可能发生在 handler 内部（自停止），join 会自锁。
        self.stop.store(true, Ordering::SeqCst);
        for handle in self.threads.drain(..) {
            let _ = handle.join();
        }
    }
}

/// 入队（revision 取当前 `items.revision`）。
pub fn enqueue(
    library: &Library,
    item_id: &str,
    stage: &str,
    priority: i64,
) -> Result<(), AppError> {
    library.enqueue_stage(item_id, stage, priority)?;
    Ok(())
}

/// worker 组的构造器。
pub struct Pipeline {
    library: Arc<Library>,
    deps: Arc<PipelineDeps>,
    events: Arc<dyn PipelineEvents>,
    handlers: HashMap<String, Arc<dyn JobHandler>>,
    config: PipelineConfig,
}

impl Pipeline {
    pub fn new(
        library: Arc<Library>,
        deps: Arc<PipelineDeps>,
        events: Arc<dyn PipelineEvents>,
    ) -> Self {
        Self {
            library,
            deps,
            events,
            handlers: HashMap::new(),
            config: PipelineConfig::default(),
        }
    }

    pub fn with_config(mut self, config: PipelineConfig) -> Self {
        self.config = config;
        self
    }

    pub fn register(mut self, handler: Arc<dyn JobHandler>) -> Self {
        self.handlers.insert(handler.stage().to_string(), handler);
        self
    }

    /// 启动：先把遗留 `running` 回退为 `queued`（崩溃恢复，attempts 保留），再起线程。
    pub fn spawn(self) -> Result<PipelineHandle, AppError> {
        let recovered = self.library.recover_jobs()?;
        if recovered > 0 {
            tracing::info!(count = recovered, "recovered interrupted jobs");
        }

        let shared = Arc::new(Shared {
            library: Arc::clone(&self.library),
            deps: self.deps,
            events: self.events,
            handlers: self.handlers,
            config: self.config,
            stop: Arc::new(AtomicBool::new(false)),
            throttle: StatusThrottle::default(),
        });

        let mut threads = Vec::with_capacity(shared.config.workers);
        for index in 0..shared.config.workers.max(1) {
            let shared = Arc::clone(&shared);
            let handle = thread::Builder::new()
                .name(format!("visrecall-pipeline-{index}"))
                .spawn(move || worker_loop(&shared))
                .map_err(|e| AppError::Internal(format!("spawn pipeline worker: {e}")))?;
            threads.push(handle);
        }

        shared.throttle.force_emit(&shared);
        Ok(PipelineHandle {
            stop: Arc::clone(&shared.stop),
            threads,
        })
    }
}

struct Shared {
    library: Arc<Library>,
    deps: Arc<PipelineDeps>,
    events: Arc<dyn PipelineEvents>,
    handlers: HashMap<String, Arc<dyn JobHandler>>,
    config: PipelineConfig,
    stop: Arc<AtomicBool>,
    throttle: StatusThrottle,
}

/// 水位事件节流：最多每 `status_debounce` 发一条；队列清空时强制发一条让 UI 归零。
#[derive(Debug, Default)]
struct StatusThrottle {
    last: Mutex<Option<Instant>>,
}

impl StatusThrottle {
    fn maybe_emit(&self, shared: &Shared) {
        let due = match self.last.lock() {
            Ok(guard) => guard
                .map(|last| last.elapsed() >= shared.config.status_debounce)
                .unwrap_or(true),
            Err(_) => true,
        };
        if due {
            self.emit(shared);
        }
    }

    fn force_emit(&self, shared: &Shared) {
        self.emit(shared);
    }

    fn emit(&self, shared: &Shared) {
        let counts = match shared.library.job_counts() {
            Ok(counts) => counts,
            Err(err) => {
                tracing::warn!(%err, "job_counts failed");
                return;
            }
        };
        if let Ok(mut guard) = self.last.lock() {
            *guard = Some(Instant::now());
        }
        shared.events.pipeline_status(counts.0, counts.1);
    }
}

fn worker_loop(shared: &Arc<Shared>) {
    loop {
        if shared.stop.load(Ordering::SeqCst) {
            break;
        }
        let claimed = shared.library.claim_next_job(now_millis());
        match claimed {
            Ok(Some(job)) => run_job(shared, job),
            Ok(None) => {
                shared.throttle.maybe_emit(shared);
                thread::sleep(shared.config.poll_interval);
            }
            Err(err) => {
                tracing::warn!(%err, "claim job failed");
                thread::sleep(shared.config.poll_interval);
            }
        }
    }
}

fn run_job(shared: &Arc<Shared>, job: Job) {
    // stale 取消：claim 后比对 revision（design §6.2）。素材已删则一并取消。
    match shared.library.item_revision(&job.item_id) {
        Ok(Some(current)) if current == job.revision => {}
        Ok(Some(current)) => {
            tracing::debug!(
                job = %job.id,
                job_revision = job.revision,
                current,
                "cancelling stale job"
            );
            if let Err(err) = shared.library.cancel_job(&job.id, "stale revision") {
                tracing::warn!(%err, "cancel stale job failed");
            }
            shared.throttle.maybe_emit(shared);
            return;
        }
        Ok(None) => {
            if let Err(err) = shared.library.cancel_job(&job.id, "item deleted") {
                tracing::warn!(%err, "cancel orphan job failed");
            }
            shared.throttle.maybe_emit(shared);
            return;
        }
        Err(err) => {
            tracing::warn!(%err, "read item revision failed");
            if let Err(err) =
                shared
                    .library
                    .retry_job(&job.id, "revision read failed", now_millis())
            {
                tracing::warn!(%err, "retry job failed");
            }
            return;
        }
    }

    let Some(handler) = shared.handlers.get(&job.stage).cloned() else {
        tracing::error!(stage = %job.stage, "no handler registered for stage");
        let _ = shared
            .library
            .finish_job_failed(&job.id, &format!("no handler for stage {}", job.stage));
        shared.throttle.maybe_emit(shared);
        return;
    };

    let ctx = JobCtx {
        library: &shared.library,
        deps: &shared.deps,
        events: &shared.events,
        job: &job,
        max_attempts: shared.config.max_attempts,
    };

    match handler.run(&ctx) {
        Ok(()) => {
            if let Err(err) = shared.library.finish_job_succeeded(&job.id) {
                tracing::warn!(%err, "finish job failed");
            }
        }
        Err(err) => {
            let message = err.to_string();
            let terminal = err.is_config_error() || job.attempts >= shared.config.max_attempts;
            if terminal {
                tracing::warn!(
                    job = %job.id,
                    stage = %job.stage,
                    attempts = job.attempts,
                    %message,
                    "job failed permanently"
                );
                if let Err(db_err) = shared.library.finish_job_failed(&job.id, &message) {
                    tracing::warn!(%db_err, "mark job failed");
                }
                shared.events.job_failed(&job, &err);
            } else {
                let backoff = shared.config.backoff_for(job.attempts);
                let next = now_millis() + i64::try_from(backoff.as_millis()).unwrap_or(2_000);
                tracing::info!(
                    job = %job.id,
                    stage = %job.stage,
                    attempts = job.attempts,
                    backoff_ms = backoff.as_millis(),
                    %message,
                    "job failed; scheduled retry"
                );
                if let Err(db_err) = shared.library.retry_job(&job.id, &message, next) {
                    tracing::warn!(%db_err, "schedule retry");
                }
            }
        }
    }

    shared.throttle.maybe_emit(shared);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_sequence_is_2_5_15() {
        let config = PipelineConfig::default();
        assert_eq!(config.backoff_for(1), Duration::from_secs(2));
        assert_eq!(config.backoff_for(2), Duration::from_secs(5));
        assert_eq!(config.backoff_for(3), Duration::from_secs(15));
        // 超出档位取最后一档（max_attempts 调大时不会 panic）
        assert_eq!(config.backoff_for(9), Duration::from_secs(15));
    }

    #[test]
    fn empty_backoff_is_zero() {
        let config = PipelineConfig {
            backoff_secs: Vec::new(),
            ..PipelineConfig::default()
        };
        assert_eq!(config.backoff_for(1), Duration::ZERO);
    }
}
