//! 缩略图（design §10.2）：长边 480px 的有损 WebP `thumbs/<id>.webp`，捕获/导入提交后异步生成。
//!
//! 线程模型（review I1）：一次拖放最多 20 张、每张都要 `image::open` 全量解码，逐张起线程
//! 会让峰值内存随批量线性增长（20 张 8000px 可到数 GB）。这里改为**有界并发**：全局队列
//! （[`OnceLock`]）后面固定 [`THUMB_WORKERS`] 个命名 worker 线程，捕获循环与导入线程都只
//! 负责 [`spawn_thumbnail`] 派发。队列无界不丢任务，同时在跑的解码最多两个。
//!
//! 失败语义（design §12）：缩略图失败**不删除原图**，只把素材转 `failed` 并发
//! `item_updated`，卡片保留占位（重试入口属于阶段 02）。worker 内每张图用 `catch_unwind`
//! 兜住 panic：标 `failed`、记录 error；连状态都写不进去时再发 `capture_failed`，
//! 不让素材永久静默停在 `parsing`。

use std::fs;
use std::panic::AssertUnwindSafe;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;

use crossbeam_channel::{Receiver, Sender};
use image::imageops::FilterType;
use tracing::{error, info, warn};
use visrecall_domain::item::{ItemKind, ItemStatus};
use visrecall_domain::AppError;
use visrecall_library::items::LegacyThumb;
use visrecall_library::Library;
use webp::Encoder;

use crate::capture_flow::write_bytes_atomic_in;
use crate::events::{AppEvent, CaptureFailedEvent, EventSink, ItemUpdatedEvent};

/// 缩略图长边上限（design §10.2）。
pub const THUMB_MAX_EDGE: u32 = 480;

/// 缩略图编码质量（有损 WebP，0–100）。
///
/// q85 在 228px 卡片上文字伪影不可见；本机实测体积约为同尺寸 PNG 的 12%。
pub const THUMB_WEBP_QUALITY: f32 = 85.0;

/// 缩略图目录（相对库根）；`Library` 打开时已建好。
pub const THUMB_DIR: &str = "thumbs";

/// 原图目录（相对库根）。
pub const ORIGINAL_DIR: &str = "originals";

/// 缩略图 worker 线程数（有界并发上限）。
///
/// 捕获一次只有一张；导入一批最多 20 张，两个 worker 让峰值解码内存止步于两张原图，
/// 同时保留「一张慢慢解码时另一张还能推进」的并行度。
pub const THUMB_WORKERS: usize = 2;

/// 缩略图失败的稳定错误码（`capture_failed.code`，与 IPC 错误码同一套命名）。
pub const THUMBNAIL_FAILED_CODE: &str = "THUMBNAIL_FAILED";

/// 素材缩略图的库内相对路径。
pub fn thumb_relative_path(item_id: &str) -> String {
    format!("{THUMB_DIR}/{item_id}.webp")
}

/// `thumb_path` 是否指向 `thumbs/` 下的文件（纵深防御：库被改坏也不读 `texts/` 等）。
fn is_thumb_relative(thumb: &str) -> bool {
    is_under_dir(thumb, THUMB_DIR)
}

/// 相对路径是否位于 `dir` 目录下（`dir/...`），且不含反斜杠。
fn is_under_dir(path: &str, dir: &str) -> bool {
    path.strip_prefix(dir)
        .and_then(|rest| rest.strip_prefix('/'))
        .is_some_and(|name| !name.is_empty() && !name.contains('\\'))
}

/// 把 RGBA 像素编码为有损 WebP；全不透明图会自动省略 alpha 通道。
fn encode_webp_rgba(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, AppError> {
    let encoder = Encoder::from_rgba(rgba, width, height);
    Ok(encoder.encode(THUMB_WEBP_QUALITY).to_vec())
}

/// 把 `src` 缩到长边 `max_edge` 并保存为 WebP 到 `dst`。
///
/// - 保持宽高比；原图长边不超过 `max_edge` 时不放大（缩略图只缩小）。
/// - 建父目录；先写同目录临时文件再 `rename`，失败清理临时文件（design §10.2）。
/// - 读图/编码/写盘失败统一返回 [`AppError::Io`]。
pub fn generate_thumbnail(src: &Path, dst: &Path, max_edge: u32) -> Result<(), AppError> {
    if max_edge == 0 {
        return Err(AppError::InvalidInput(
            "thumbnail max edge must be positive".into(),
        ));
    }

    let image =
        image::open(src).map_err(|err| AppError::Io(format!("open {}: {err}", src.display())))?;
    let (width, height) = (image.width(), image.height());
    if width == 0 || height == 0 {
        return Err(AppError::Io(format!(
            "image {} has an empty size: {width}x{height}",
            src.display()
        )));
    }
    let scaled = if width.max(height) > max_edge {
        image.resize(max_edge, max_edge, FilterType::Lanczos3)
    } else {
        image
    };

    let rgba = scaled.to_rgba8();
    let (width, height) = rgba.dimensions();
    let webp = encode_webp_rgba(width, height, rgba.as_raw())?;

    // 临时文件与目标同目录：缩略图不参与 `tmp/` 的启动清理策略。
    let parent = dst.parent().unwrap_or_else(|| Path::new("."));
    let tmp_id = dst
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "thumbnail".to_string());
    write_bytes_atomic_in(parent, &tmp_id, dst, &webp)
}

/// 读取素材缩略图的字节与 MIME（资源协议用）。
///
/// MIME 按 `thumb_path` 扩展名推导：新缩略图是 `.webp`，旧库里的 `.png` 继续可用。
/// 素材不存在、没有 `thumb_path`、或缩略图文件已丢失都返回 [`AppError::NotFound`]；
/// `thumb_path` 不在 `thumbs/` 下（库被手工改坏指向 `texts/` 等库内文件）或不是库内
/// 相对路径，返回 [`AppError::InvalidInput`]，从不读取库根目录之外的文件。
pub fn load_thumb_bytes(
    library: &Library,
    item_id: &str,
) -> Result<(Vec<u8>, &'static str), AppError> {
    let item = library.get_item(item_id)?.ok_or(AppError::NotFound)?;
    let thumb = item.thumb_path.ok_or(AppError::NotFound)?;

    // 纵深防御：协议只服务缩略图，库里的路径再坏也不能读到其他库内文件。
    if !is_thumb_relative(&thumb) {
        return Err(AppError::InvalidInput(format!(
            "thumb path is not under {THUMB_DIR}/: {thumb}"
        )));
    }

    let path = library.relative_to_root(&thumb).map_err(|err| match err {
        AppError::InvalidInput(detail) => AppError::InvalidInput(detail),
        // 路径解析失败（文件被删/目录被移走）对调用方等价于「没有缩略图」。
        _ => AppError::NotFound,
    })?;

    let bytes = fs::read(&path).map_err(|err| match err.kind() {
        std::io::ErrorKind::NotFound => AppError::NotFound,
        _ => AppError::Io(format!("read {thumb}: {err}")),
    })?;
    Ok((bytes, image_mime(&thumb)))
}

/// 读取素材原图的字节与 MIME（资源协议 `visrecall://original/<id>` 用）。
///
/// 语义与 [`load_thumb_bytes`] 同构：
/// - 素材不存在 → `NOT_FOUND`；
/// - 非图片素材（文本/链接）→ `INVALID_INPUT`（协议层 400，不读出文本内容）；
/// - `original_path` 不在 `originals/` 下 → `INVALID_INPUT`（纵深防御）；
/// - 文件已丢失 → `NOT_FOUND`（协议层 404，前端显示「原图文件缺失」占位）。
///
/// 只返回原尺寸字节：不实现 `?max=` 缩放参数（D2-08），预览由 WebView 按容器缩放。
pub fn load_original_bytes(
    library: &Library,
    item_id: &str,
) -> Result<(Vec<u8>, &'static str), AppError> {
    let item = library.get_item(item_id)?.ok_or(AppError::NotFound)?;
    if item.kind != ItemKind::Image {
        return Err(AppError::InvalidInput(format!(
            "item {item_id} is not an image"
        )));
    }

    if let Some(external) = library.external_asset_for_item(item_id)? {
        let asset = visrecall_library::AssetReader::new(library).resolve(item_id)?;
        let bytes = fs::read(&asset.path)
            .map_err(|err| AppError::Io(format!("读取引用原图失败：{err}")))?;
        return Ok((bytes, image_mime(&external.relative_path)));
    }

    // 纵深防御：协议只服务原图，库里的路径再坏也不能读到 `texts/` 等其他库内文件。
    if !is_under_dir(&item.original_path, ORIGINAL_DIR) {
        return Err(AppError::InvalidInput(format!(
            "original path is not under {ORIGINAL_DIR}/: {}",
            item.original_path
        )));
    }

    let path = library
        .relative_to_root(&item.original_path)
        .map_err(|err| match err {
            AppError::InvalidInput(detail) => AppError::InvalidInput(detail),
            _ => AppError::NotFound,
        })?;
    let bytes = fs::read(&path).map_err(|err| match err.kind() {
        std::io::ErrorKind::NotFound => AppError::NotFound,
        _ => AppError::Io(format!("read {}: {err}", item.original_path)),
    })?;

    Ok((bytes, image_mime(&item.original_path)))
}

/// 按扩展名给出 `Content-Type`；未知扩展名按二进制流返回。
fn image_mime(path: &str) -> &'static str {
    let extension = Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        _ => "application/octet-stream",
    }
}

/// 一张缩略图的生成任务：库、事件发送端、素材 id 与库内相对原图路径。
///
/// 原图路径随任务携带，worker 不必再查一次库（批量导入时省掉 20 次读）。
struct ThumbnailJob {
    library: Arc<Library>,
    sink: Arc<dyn EventSink>,
    item_id: String,
    original_path: String,
    revision: i64,
}

/// 全局任务队列；首次派发时创建 worker（见 [`queue`]）。
static QUEUE: OnceLock<Sender<ThumbnailJob>> = OnceLock::new();

/// 取全局队列，必要时先起 [`THUMB_WORKERS`] 个 worker。
///
/// 初始化在互斥锁内完成，避免两个线程（捕获循环 + 导入线程）各起一批 worker。
/// 任一线程起不来就返回错误并**保持队列未初始化**：已起的 worker 因通道断开自行退出，
/// 下一次派发会重试，不会留下「只有一半 worker 且永远不补」的半成品。
fn queue() -> Result<&'static Sender<ThumbnailJob>, AppError> {
    static INIT: Mutex<()> = Mutex::new(());

    let _guard = INIT.lock().unwrap_or_else(|poison| poison.into_inner());
    if let Some(queue) = QUEUE.get() {
        return Ok(queue);
    }

    let (tx, rx) = crossbeam_channel::unbounded();
    for index in 0..THUMB_WORKERS {
        let worker_rx = rx.clone();
        let spawned = thread::Builder::new()
            .name(format!("visrecall-thumb-{index}"))
            .spawn(move || worker_loop(worker_rx));
        if let Err(err) = spawned {
            // 丢弃 tx/rx：已起的 worker 收到通道断开后退出，队列留待下次重试。
            return Err(AppError::Internal(format!(
                "spawn thumbnail worker {index}: {err}"
            )));
        }
    }
    drop(rx);

    QUEUE
        .set(tx)
        .map_err(|_| AppError::Internal("thumbnail queue was already initialized".into()))?;
    QUEUE
        .get()
        .ok_or_else(|| AppError::Internal("thumbnail queue is unavailable".into()))
}

/// 异步生成缩略图：把任务放进全局队列，立即返回（捕获/导入线程不等待解码）。
///
/// 返回 `Err` 表示队列/worker 起不来——调用方必须把它报出去（`capture_failed`），
/// 否则该素材会静默停在 `parsing`。
pub fn spawn_thumbnail(
    library: Arc<Library>,
    sink: Arc<dyn EventSink>,
    item_id: String,
    original_path: String,
) -> Result<(), AppError> {
    let revision = library
        .get_item(&item_id)?
        .ok_or(AppError::NotFound)?
        .revision;
    let job = ThumbnailJob {
        library,
        sink,
        item_id,
        original_path,
        revision,
    };
    queue()?
        .send(job)
        .map_err(|_| AppError::Internal("thumbnail queue is closed".into()))
}

/// worker 线程体：串行取任务，直到队列发送端全部丢弃。
fn worker_loop(rx: Receiver<ThumbnailJob>) {
    for job in rx {
        run_job(&job);
    }
}

/// 执行一张缩略图任务，`catch_unwind` 兜住 panic。
///
/// panic（例如某张图触发解码器内部断言）不能让 worker 线程死掉，否则队列会永久少一个
/// worker；这里记录 error 后尽力把素材标 `failed`，恢复动作本身 panic 也只记录。
fn run_job(job: &ThumbnailJob) {
    #[cfg(test)]
    let _active = ActiveGuard::new();

    let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
        run_task(
            &job.library,
            job.sink.as_ref(),
            &job.item_id,
            &job.original_path,
            job.revision,
        )
    }));

    if let Err(payload) = outcome {
        error!(
            id = %job.item_id,
            panic = %panic_text(payload.as_ref()),
            "thumbnail worker panicked; marking the item failed"
        );
        let recovered = std::panic::catch_unwind(AssertUnwindSafe(|| {
            mark_failed(&job.library, job.sink.as_ref(), &job.item_id)
        }));
        if recovered.is_err() {
            error!(
                id = %job.item_id,
                "failed to record the panicked thumbnail job; the item may stay parsing"
            );
        }
    }
}

/// 同步生成一张缩略图并写回状态（测试与排障直接调用；生产走 [`spawn_thumbnail`]）。
///
/// - 成功：`thumbs/<id>.webp` + `update_status(Ready, Some(相对路径))` +
///   `item_updated { fields: ["status", "thumbUrl"] }`。
/// - 失败：`update_status(Failed, None)` + 同样字段的 `item_updated`；原图保留，卡片显示占位。
pub fn generate_for_item(library: &Library, sink: &dyn EventSink, item_id: &str) {
    match library.get_item(item_id) {
        Ok(Some(item)) if item.kind == ItemKind::Image => {
            run_task(library, sink, item_id, &item.original_path, item.revision)
        }
        Ok(Some(_)) => warn!(id = item_id, "item is not an image; no thumbnail"),
        Ok(None) => warn!(id = item_id, "item disappeared before thumbnailing"),
        Err(err) => {
            error!(id = item_id, error = %err, "failed to load the item for thumbnailing");
            emit_failed_event(sink, item_id, &err.to_string());
        }
    }
}

/// 一张图的完整处理：读原图 → 写缩略图 → 写回状态 → 发事件。
fn run_task(
    library: &Library,
    sink: &dyn EventSink,
    item_id: &str,
    original_path: &str,
    revision: i64,
) {
    // 刷新目录时同一素材会递增 revision，旧任务不得覆盖新内容的状态和预览。
    if !revision_is_current(library, item_id, revision) {
        return;
    }
    match build_thumbnail(library, item_id, original_path) {
        Ok(relative) if revision_is_current(library, item_id, revision) => {
            match library.update_status(item_id, ItemStatus::Ready, Some(&relative)) {
                Ok(()) => {
                    info!(id = item_id, path = %relative, "thumbnail generated");
                    emit_updated(sink, item_id);
                }
                Err(err) => report_status_failure(sink, item_id, "store the thumbnail path", &err),
            }
        }
        Ok(_) => {}
        Err(err) if revision_is_current(library, item_id, revision) => {
            warn!(id = item_id, error = %err, "thumbnail generation failed; keeping the original");
            mark_failed(library, sink, item_id);
        }
        Err(_) => {}
    }
}

fn revision_is_current(library: &Library, item_id: &str, revision: i64) -> bool {
    library
        .get_item(item_id)
        .ok()
        .flatten()
        .is_some_and(|item| item.revision == revision)
}

/// 把素材标 `failed` 并发 `item_updated`（失败不删原图）。
fn mark_failed(library: &Library, sink: &dyn EventSink, item_id: &str) {
    match library.update_status(item_id, ItemStatus::Failed, None) {
        Ok(()) => emit_updated(sink, item_id),
        Err(err) => report_status_failure(sink, item_id, "mark the item failed", &err),
    }
}

/// `update_status` 失败：素材会停在 `parsing`，不能只 warn。
///
/// 记录 error + 发 `capture_failed`（用户可见）+ 发 `item_updated`（前端重新拉取当前状态），
/// 两条事件都不依赖数据库写成功。
fn report_status_failure(sink: &dyn EventSink, item_id: &str, action: &str, err: &AppError) {
    error!(
        id = item_id,
        error = %err,
        action,
        "thumbnail status update failed; the item stays parsing"
    );
    emit_failed_event(sink, item_id, &format!("failed to {action}: {err}"));
    emit_updated(sink, item_id);
}

/// 读原图 → 写 `thumbs/<id>.webp`，返回缩略图的库内相对路径。
fn build_thumbnail(
    library: &Library,
    item_id: &str,
    original_path: &str,
) -> Result<String, AppError> {
    let source = if original_path.is_empty() {
        visrecall_library::AssetReader::new(library)
            .resolve(item_id)?
            .path
    } else {
        library.relative_to_root(original_path)?
    };
    let relative = thumb_relative_path(item_id);
    let target = library.root().join(&relative);
    generate_thumbnail(&source, &target, THUMB_MAX_EDGE)?;
    Ok(relative)
}

/// 起一个后台线程，把旧 PNG 缩略图迁移为 WebP；处理完即退出。
///
/// 失败只返回线程创建错误（调用方记录 warn），迁移条目本身的失败在内部计数。
pub fn spawn_legacy_thumb_migration(library: Arc<Library>) -> Result<(), AppError> {
    thread::Builder::new()
        .name("visrecall-thumb-migrate".into())
        .spawn(move || migrate_legacy_thumbs(&library))
        .map(|_| ())
        .map_err(|err| AppError::Internal(format!("spawn thumbnail migration thread: {err}")))
}

/// 同步迁移全部旧 PNG 缩略图（测试与排障直接调用；生产走 [`spawn_legacy_thumb_migration`]）。
pub(crate) fn migrate_legacy_thumbs(library: &Library) {
    let entries = match library.legacy_png_thumbs() {
        Ok(entries) => entries,
        Err(err) => {
            warn!(error = %err, "failed to list legacy png thumbs");
            return;
        }
    };
    if entries.is_empty() {
        return;
    }
    let total = entries.len();
    let mut failed = 0;
    for entry in &entries {
        if let Err(err) = migrate_legacy_thumb(library, entry) {
            failed += 1;
            warn!(
                id = %entry.id,
                path = %entry.thumb_path,
                error = %err,
                "legacy thumb migration failed"
            );
        }
    }
    info!(total, failed, "legacy png thumb migration finished");
}

/// 迁移一条：解码 PNG → 编码 WebP → 原子写 → 换库路径 → 删旧文件。
///
/// `Ok(())` 包含「已迁移」与「安全跳过」（素材已变、旧文件缺失）。
/// 已存在 `.webp`（上次崩在写库之前）时跳过重编码，直接补写库路径。
pub(crate) fn migrate_legacy_thumb(
    library: &Library,
    entry: &LegacyThumb,
) -> Result<(), AppError> {
    // 复核：迁移期间素材可能被刷新/重新链接，旧任务不得覆盖新状态。
    let Some(item) = library.get_item(&entry.id)? else {
        return Ok(());
    };
    if item.revision != entry.revision
        || item.thumb_path.as_deref() != Some(entry.thumb_path.as_str())
    {
        return Ok(());
    }

    let png_path = library.relative_to_root(&entry.thumb_path)?;
    if !png_path.is_file() {
        return Ok(());
    }

    let webp_relative = thumb_relative_path(&entry.id);
    let webp_path = library.root().join(&webp_relative);
    if !webp_path.is_file() {
        let image = image::open(&png_path)
            .map_err(|err| AppError::Io(format!("open {}: {err}", png_path.display())))?;
        let rgba = image.to_rgba8();
        let (width, height) = rgba.dimensions();
        let webp = encode_webp_rgba(width, height, rgba.as_raw())?;
        let parent = webp_path.parent().unwrap_or_else(|| Path::new("."));
        write_bytes_atomic_in(parent, &entry.id, &webp_path, &webp)?;
    }

    // 保留原状态（迁移不是修复失败素材的时机）。
    library.update_status(&entry.id, item.status, Some(&webp_relative))?;

    // 库已指向新文件；删除失败只留下一个孤儿 png，不影响正确性。
    if let Err(err) = fs::remove_file(&png_path) {
        warn!(path = %entry.thumb_path, error = %err, "failed to remove migrated png thumb");
    }
    Ok(())
}

/// 发 `item_updated`；字段名与前端 DTO 一致（`status` / `thumbUrl`）。
fn emit_updated(sink: &dyn EventSink, item_id: &str) {
    sink.emit(AppEvent::ItemUpdated(ItemUpdatedEvent {
        id: item_id.to_string(),
        fields: vec!["status".to_string(), "thumbUrl".to_string()],
    }));
}

/// 发 `capture_failed { THUMBNAIL_FAILED }`：缩略图链路出错且状态写不进去时的兜底可见性。
fn emit_failed_event(sink: &dyn EventSink, item_id: &str, detail: &str) {
    sink.emit(AppEvent::CaptureFailed(CaptureFailedEvent {
        code: THUMBNAIL_FAILED_CODE.to_string(),
        message: format!("thumbnail for {item_id} failed: {detail}"),
    }));
}

/// 从 `catch_unwind` 的载荷里取可读消息（`&str` / `String` / 其他）。
fn panic_text(payload: &(dyn std::any::Any + Send)) -> &str {
    if let Some(message) = payload.downcast_ref::<&str>() {
        message
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.as_str()
    } else {
        "non-string panic payload"
    }
}

/// 测试用并发观测：记录同时执行 `run_job` 的数量与峰值。
///
/// 所有任务共用一个全局队列，因此峰值就是 worker 数——用于锁住「有界并发」这条不变量。
#[cfg(test)]
mod test_metrics {
    use std::sync::atomic::{AtomicUsize, Ordering};

    static ACTIVE: AtomicUsize = AtomicUsize::new(0);
    static PEAK: AtomicUsize = AtomicUsize::new(0);

    pub(super) fn enter() {
        let active = ACTIVE.fetch_add(1, Ordering::SeqCst) + 1;
        PEAK.fetch_max(active, Ordering::SeqCst);
    }

    pub(super) fn leave() {
        ACTIVE.fetch_sub(1, Ordering::SeqCst);
    }

    pub(super) fn peak() -> usize {
        PEAK.load(Ordering::SeqCst)
    }
}

/// `run_job` 期间的活跃计数守卫（panic 展开时也会 `leave`）。
#[cfg(test)]
struct ActiveGuard;

#[cfg(test)]
impl ActiveGuard {
    fn new() -> Self {
        test_metrics::enter();
        Self
    }
}

#[cfg(test)]
impl Drop for ActiveGuard {
    fn drop(&mut self) {
        test_metrics::leave();
    }
}

#[cfg(test)]
mod tests {
    use std::ptr;
    use std::time::{Duration, Instant};

    use image::{Rgba, RgbaImage};
    use visrecall_domain::item::NewItem;

    use super::*;
    use crate::events::RecordingSink;

    /// 并发峰值（[`test_metrics::peak`]）是**进程级**的静态量：`run_job` 除了被 worker
    /// 线程调用，`panicking_sink_still_marks_the_item_failed` 也会在测试线程上直接调用。
    /// 两个用例并行时峰值会变成 3，与 worker 池的容量断言无关。互斥执行后，峰值只反映
    /// worker 池本身。
    static METRIC_LOCK: Mutex<()> = Mutex::new(());

    fn metric_guard() -> std::sync::MutexGuard<'static, ()> {
        METRIC_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    fn write_png(path: &Path, width: u32, height: u32) {
        RgbaImage::from_pixel(width, height, Rgba([10, 20, 30, 255]))
            .save(path)
            .unwrap();
    }

    /// 把原图放进库里 `relative` 对应的位置（按需建 `originals/images/YYYY/MM`）。
    fn place_original(library: &Library, source: &Path, relative: &str) {
        let target = library.root().join(relative);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::copy(source, target).unwrap();
    }

    fn insert_image(library: &Library, id: &str, relative: &str) {
        library
            .insert_item_with_id(
                id.to_string(),
                NewItem {
                    kind: ItemKind::Image,
                    captured_at: 1,
                    source_app: None,
                    source_title: None,
                    source_url: None,
                    width: Some(10),
                    height: Some(10),
                    char_count: None,
                    original_path: relative.to_string(),
                    content_hash: "hash".into(),
                },
            )
            .unwrap();
    }

    fn updated_events(sink: &RecordingSink) -> Vec<ItemUpdatedEvent> {
        sink.events()
            .into_iter()
            .filter_map(|event| match event {
                AppEvent::ItemUpdated(payload) => Some(payload),
                _ => None,
            })
            .collect()
    }

    /// 同步路径的完整链路：缩略图落盘 → `ready` + `thumb_path` → `item_updated`。
    #[test]
    fn generate_for_item_marks_the_item_ready_with_a_thumb() {
        let dir = tempfile::tempdir().unwrap();
        let library = Library::open(dir.path().join("library")).unwrap();
        let original = dir.path().join("original.png");
        write_png(&original, 1000, 500);

        let id = "item-1";
        let relative = "originals/images/2026/01/item-1.png";
        insert_image(&library, id, relative);
        place_original(&library, &original, relative);
        let sink = RecordingSink::default();

        generate_for_item(&library, &sink, id);

        let stored = library.get_item(id).unwrap().unwrap();
        assert_eq!(stored.status, ItemStatus::Ready);
        assert_eq!(stored.thumb_path.as_deref(), Some("thumbs/item-1.webp"));
        assert!(library.root().join("thumbs/item-1.webp").exists());
        assert!(!library.root().join("thumbs/item-1.tmp").exists());

        let updates = updated_events(&sink);
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].id, id);
        assert_eq!(updates[0].fields, vec!["status", "thumbUrl"]);
    }

    /// 缩略图失败（原图丢失）：素材转 `failed`，仍然只发 `item_updated`。
    #[test]
    fn generate_for_item_marks_failed_when_the_original_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let library = Library::open(dir.path().join("library")).unwrap();
        let id = "item-broken";
        insert_image(&library, id, "originals/images/2026/01/missing.png");
        let sink = RecordingSink::default();

        generate_for_item(&library, &sink, id);

        let stored = library.get_item(id).unwrap().unwrap();
        assert_eq!(stored.status, ItemStatus::Failed);
        assert_eq!(stored.thumb_path, None);

        let updates = updated_events(&sink);
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].fields, vec!["status", "thumbUrl"]);
    }

    /// 小图不放大：长边小于上限时保持原尺寸。
    #[test]
    fn small_images_are_not_upscaled() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("small.png");
        let dst = dir.path().join("thumbs/small.webp");
        write_png(&src, 40, 20);

        generate_thumbnail(&src, &dst, THUMB_MAX_EDGE).unwrap();

        let out = image::open(&dst).unwrap();
        assert_eq!((out.width(), out.height()), (40, 20));
    }

    /// `spawn_thumbnail` 派发后 worker 完成生成（调用方立即返回）。
    #[test]
    fn spawn_thumbnail_generates_in_the_background() {
        let dir = tempfile::tempdir().unwrap();
        let library = Arc::new(Library::open(dir.path().join("library")).unwrap());
        let original = dir.path().join("original.png");
        write_png(&original, 1000, 500);
        let id = "item-bg";
        let relative = "originals/images/2026/01/item-bg.png";
        insert_image(&library, id, relative);
        place_original(&library, &original, relative);
        let sink = Arc::new(RecordingSink::default());

        spawn_thumbnail(
            Arc::clone(&library),
            Arc::clone(&sink) as Arc<dyn EventSink>,
            id.to_string(),
            relative.to_string(),
        )
        .expect("queue thumbnail job");

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let stored = library.get_item(id).unwrap().unwrap();
            if stored.status == ItemStatus::Ready {
                assert_eq!(stored.thumb_path.as_deref(), Some("thumbs/item-bg.webp"));
                break;
            }
            assert!(
                Instant::now() < deadline,
                "thumbnail was not generated within 5s"
            );
            thread::sleep(Duration::from_millis(10));
        }
        assert!(!updated_events(&sink).is_empty());
    }

    /// 队列只初始化一次：所有派发共用一个发送端（也就共用同一批 worker）。
    #[test]
    fn queue_is_initialized_once_and_shared() {
        let first = queue().expect("initialize queue");
        let second = queue().expect("reuse queue");
        assert!(
            ptr::eq(first, second),
            "the thumbnail queue must be a single global channel"
        );
        // 有界并发的上限由 worker 数量决定（结构性约束）。
        assert_eq!(THUMB_WORKERS, 2);
    }

    /// 批量派发仍受 [`THUMB_WORKERS`] 约束：同时执行的绘图不超过 worker 数，且全部完成。
    #[test]
    fn concurrent_jobs_never_exceed_the_worker_count() {
        let _serial = metric_guard();
        let dir = tempfile::tempdir().unwrap();
        let library = Arc::new(Library::open(dir.path().join("library")).unwrap());
        let original = dir.path().join("batch.png");
        write_png(&original, 600, 400);
        let sink = Arc::new(RecordingSink::default());

        let ids: Vec<String> = (0..8).map(|index| format!("batch-{index}")).collect();
        for id in &ids {
            let relative = format!("originals/images/2026/01/{id}.png");
            insert_image(&library, id, &relative);
            place_original(&library, &original, &relative);
            spawn_thumbnail(
                Arc::clone(&library),
                Arc::clone(&sink) as Arc<dyn EventSink>,
                id.clone(),
                relative,
            )
            .expect("queue thumbnail job");
        }

        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let ready = ids
                .iter()
                .filter(|id| {
                    library
                        .get_item(id)
                        .unwrap()
                        .is_some_and(|item| item.status == ItemStatus::Ready)
                })
                .count();
            if ready == ids.len() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "only {ready}/{} thumbnails were generated within 30s",
                ids.len()
            );
            thread::sleep(Duration::from_millis(20));
        }

        let peak = test_metrics::peak();
        assert!(
            peak <= THUMB_WORKERS,
            "concurrency peak {peak} exceeded the {} workers",
            THUMB_WORKERS
        );
        assert!(peak >= 1, "workers never ran a job");
    }

    /// 事件发送端 panic（例如序列化/发送失败）：worker 兜住 panic 并把素材标 `failed`，
    /// 不会带死线程。
    #[test]
    fn panicking_sink_still_marks_the_item_failed() {
        struct PanickingSink;

        impl EventSink for PanickingSink {
            fn emit(&self, _event: AppEvent) {
                panic!("sink exploded");
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let library = Arc::new(Library::open(dir.path().join("library")).unwrap());
        let original = dir.path().join("original.png");
        write_png(&original, 100, 50);
        let id = "item-panic";
        let relative = "originals/images/2026/01/item-panic.png";
        insert_image(&library, id, relative);
        place_original(&library, &original, relative);

        let job = ThumbnailJob {
            library: Arc::clone(&library),
            sink: Arc::new(PanickingSink),
            item_id: id.to_string(),
            original_path: relative.to_string(),
            revision: 0,
        };
        // `catch_unwind` 会打印 panic 信息；测试只关心兜底结果。
        // 与并发峰值断言互斥：这条 `run_job` 不走 worker 池，会让进程级峰值偏大。
        let _serial = metric_guard();
        run_job(&job);

        let stored = library.get_item(id).unwrap().unwrap();
        assert_eq!(stored.status, ItemStatus::Failed);
    }

    /// 旧 PNG 缩略图迁移为 WebP：换库路径、删旧文件、输出可解码。
    #[test]
    fn migrates_legacy_png_thumbs_to_webp() {
        let dir = tempfile::tempdir().unwrap();
        let library = Library::open(dir.path().join("library")).unwrap();
        let id = "legacy-1";
        insert_image(&library, id, "originals/images/2026/01/legacy-1.png");
        let png_rel = format!("thumbs/{id}.png");
        let png_path = library.root().join(&png_rel);
        fs::create_dir_all(png_path.parent().unwrap()).unwrap();
        write_png(&png_path, 100, 50);
        library
            .update_status(id, ItemStatus::Ready, Some(&png_rel))
            .unwrap();

        migrate_legacy_thumbs(&library);

        let stored = library.get_item(id).unwrap().unwrap();
        assert_eq!(stored.thumb_path.as_deref(), Some("thumbs/legacy-1.webp"));
        assert!(!png_path.exists());
        let bytes = fs::read(library.root().join("thumbs/legacy-1.webp")).unwrap();
        assert_eq!(
            image::guess_format(&bytes).unwrap(),
            image::ImageFormat::WebP
        );
        assert!(library.legacy_png_thumbs().unwrap().is_empty());
    }

    /// 迁移不得把 `failed` 素材改成 `ready`。
    #[test]
    fn migration_keeps_failed_status() {
        let dir = tempfile::tempdir().unwrap();
        let library = Library::open(dir.path().join("library")).unwrap();
        let id = "legacy-failed";
        insert_image(&library, id, "originals/images/2026/01/legacy-failed.png");
        let png_rel = format!("thumbs/{id}.png");
        let png_path = library.root().join(&png_rel);
        fs::create_dir_all(png_path.parent().unwrap()).unwrap();
        write_png(&png_path, 100, 50);
        library
            .update_status(id, ItemStatus::Failed, Some(&png_rel))
            .unwrap();

        migrate_legacy_thumbs(&library);

        let stored = library.get_item(id).unwrap().unwrap();
        assert_eq!(stored.status, ItemStatus::Failed);
        assert_eq!(
            stored.thumb_path.as_deref(),
            Some("thumbs/legacy-failed.webp")
        );
    }

    /// 旧文件缺失：跳过，不改库，也不产生新文件。
    #[test]
    fn migration_skips_missing_legacy_files() {
        let dir = tempfile::tempdir().unwrap();
        let library = Library::open(dir.path().join("library")).unwrap();
        let id = "legacy-missing";
        insert_image(&library, id, "originals/images/2026/01/legacy-missing.png");
        let png_rel = format!("thumbs/{id}.png");
        library
            .update_status(id, ItemStatus::Ready, Some(&png_rel))
            .unwrap();

        migrate_legacy_thumbs(&library);

        let stored = library.get_item(id).unwrap().unwrap();
        assert_eq!(stored.thumb_path.as_deref(), Some(png_rel.as_str()));
        assert!(!library.root().join("thumbs/legacy-missing.webp").exists());
    }

    /// 扫描后、逐条处理前素材被更新（thumb_path 变了）：该条跳过，旧文件保留。
    #[test]
    fn migration_skips_entries_whose_thumb_changed_after_scan() {
        let dir = tempfile::tempdir().unwrap();
        let library = Library::open(dir.path().join("library")).unwrap();
        let id = "legacy-changed";
        insert_image(&library, id, "originals/images/2026/01/legacy-changed.png");
        let png_rel = format!("thumbs/{id}.png");
        let png_path = library.root().join(&png_rel);
        fs::create_dir_all(png_path.parent().unwrap()).unwrap();
        write_png(&png_path, 100, 50);
        library
            .update_status(id, ItemStatus::Ready, Some(&png_rel))
            .unwrap();
        let entries = library.legacy_png_thumbs().unwrap();
        assert_eq!(entries.len(), 1);

        library
            .update_status(id, ItemStatus::Ready, Some("thumbs/other.webp"))
            .unwrap();
        migrate_legacy_thumb(&library, &entries[0]).unwrap();

        let stored = library.get_item(id).unwrap().unwrap();
        assert_eq!(stored.thumb_path.as_deref(), Some("thumbs/other.webp"));
        assert!(png_path.exists());
    }
}
