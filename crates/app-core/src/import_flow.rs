//! 拖放导入：把 [`DropContent`] 落盘并入库（design §5/§9.2/§10.2、P1-R09）。
//!
//! 线程模型（I3）：悬浮球线程只读取 OLE 数据并投递事件；app-core 的捕获循环收到
//! [`ShellEvent::Drop`] 后**不在循环线程上执行 [`ImportFlow::ingest`]**，而是交给独立的
//! 导入工作线程（见 `app::spawn_import`）。因此链接标题的 5s 超时、批量图片转码都不会让
//! 捕获请求/覆盖层事件排队。
//!
//! 逐项处理、部分失败不影响其他项：每个失败都会发 `capture_failed`，成功的素材由调用方
//! 统一发 `item_created`。
//!
//! 阶段 01 的边界：
//! - 图片（含 `ImageDib`）先以 `parsing` 入库；[`ImportFlow::emit_created`] 发完
//!   `item_created` 后把缩略图派发到后台线程，成功转 `ready`、失败转 `failed`；
//! - 文本/链接没有缩略图，写入后立即标 `ready`；
//! - 链接标题抓取是阻塞调用（5s 超时、响应体最多 512KB），失败时标题回退 URL。

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Once, OnceLock};
use std::time::{Duration, Instant};

use encoding_rs::{GB18030, UTF_16BE, UTF_16LE};
use image::{DynamicImage, ImageFormat, RgbaImage};
use sha2::{Digest, Sha256};
use tracing::{info, warn};
use visrecall_capture::WindowProbe;
use visrecall_domain::drop::DropContent;
use visrecall_domain::item::{Item, ItemKind, ItemStatus, NewItem};
use visrecall_domain::{new_item_id, now_millis, AppError};
use visrecall_library::Library;

use crate::capture_flow::{encode_rgba_png, image_target, write_bytes_atomic};
use crate::events::{AppEvent, CaptureFailedEvent, EventSink, ItemEvent};
use crate::thumbnail;

/// 一次拖放最多导入的文件数（P1-R09）。
pub const MAX_IMPORT_FILES: usize = 20;
/// 单张图片文件的大小上限。
pub const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;
/// 单份文本文件的大小上限。
pub const MAX_TEXT_BYTES: u64 = 500 * 1024;
/// 图片长边上限；超过时按比例缩小后入库（P1-R09）。
pub const MAX_IMAGE_EDGE: u32 = 8000;
/// 文本/HTML 片段的字符上限（`classify_drop` 已校验，这里再兜一次）。
pub const MAX_SNIPPET_CHARS: usize = 10_000;
/// 链接标题抓取超时（P1-R09）。
pub const URL_TIMEOUT: Duration = Duration::from_secs(5);
/// 链接标题抓取最多读取的响应体字节数（512KB）；标题在 `<head>` 里，够用即可。
pub const MAX_URL_BODY_BYTES: u64 = 512 * 1024;

/// 从拖放内容导入素材的入口。
pub struct ImportFlow {
    library: Arc<Library>,
    probe: Arc<dyn WindowProbe>,
    sink: Arc<dyn EventSink>,
    /// 复用阻塞 HTTP 客户端（自带 tokio 运行时，创建一次即可）。
    http: OnceLock<reqwest::blocking::Client>,
}

impl ImportFlow {
    pub fn new(
        library: Arc<Library>,
        probe: Arc<dyn WindowProbe>,
        sink: Arc<dyn EventSink>,
    ) -> Self {
        Self {
            library,
            probe,
            sink,
            http: OnceLock::new(),
        }
    }

    /// 导入一次拖放：返回成功入库的素材（调用方负责发 `item_created`）。
    ///
    /// 单项失败只发 `capture_failed` 并继续；全部失败时返回第一个错误。
    pub fn ingest(&self, content: DropContent) -> Result<Vec<Item>, AppError> {
        let started = Instant::now();
        let source = self.foreground_source();
        let result = match content {
            DropContent::Files(files) => self.import_files(&files, None, &source),
            DropContent::FilesWithNote { files, note } => {
                self.import_files(&files, Some(note.as_str()), &source)
            }
            DropContent::Text { plain, html } => {
                self.import_text_snippet(&plain, html.as_deref(), &source)
            }
            DropContent::Url { url } => self.import_link(&url, &source),
            DropContent::ImageDib {
                width,
                height,
                bgra,
            } => self.import_dib(width, height, bgra, &source),
        };
        match &result {
            Ok(items) => info!(
                count = items.len(),
                elapsed_ms = started.elapsed().as_millis() as u64,
                "drop imported"
            ),
            Err(err) => warn!(
                error = %err,
                elapsed_ms = started.elapsed().as_millis() as u64,
                "drop import failed"
            ),
        }
        result
    }

    /// 导入成功后由核心循环调用：为每个新素材发 `item_created`。
    ///
    /// 发完 `item_created` 再派发图片缩略图任务（有界 worker 队列，`thumbs/<id>.webp`），
    /// 保证前端先拿到素材、后收到 `item_updated`；文本/链接没有缩略图，跳过。
    /// 派发失败上报 `capture_failed { THUMBNAIL_FAILED }`，不让素材静默停在 `parsing`。
    pub fn emit_created(&self, items: &[Item]) {
        for item in items {
            self.sink.emit(AppEvent::ItemCreated(ItemEvent {
                id: item.id.clone(),
            }));
            // 阶段 03：图片入队 ocr、文本/链接入队 index（失败只 warn，自检会补）
            crate::pipeline_host::enqueue_initial_indexing(&self.library, item);
        }
        for item in items.iter().filter(|item| item.kind == ItemKind::Image) {
            if let Err(err) = thumbnail::spawn_thumbnail(
                Arc::clone(&self.library),
                Arc::clone(&self.sink),
                item.id.clone(),
                item.original_path.clone(),
            ) {
                warn!(id = %item.id, error = %err, "failed to queue the thumbnail job");
                self.emit_failed(
                    thumbnail::THUMBNAIL_FAILED_CODE,
                    &format!("failed to queue the thumbnail job: {err}"),
                );
            }
        }
    }

    /// 主窗口手动选择或弹窗拖放导入；来源不再猜测当前前台窗口。
    pub fn import_manual_file(&self, path: &Path) -> Result<Item, AppError> {
        let source = Source {
            app: Some("VisRecall".into()),
            title: Some("手动导入".into()),
        };
        match FileKind::of(path) {
            Ok(FileKind::Image) => self.import_image_file(path, &source, None),
            Ok(FileKind::Text) => self.import_text_file(path, &source),
            Err(failure) => Err(failure),
        }
        .map_err(|failure| failure.error)
    }

    /// 创建明确的手动文字素材，正文像 URL 时也保持文本类型。
    pub fn create_text_item(
        &self,
        title: Option<&str>,
        body: &str,
        tags: &[String],
    ) -> Result<Item, AppError> {
        let chars = body.chars().count();
        if body.trim().is_empty() {
            return Err(AppError::InvalidInput("正文不能为空".into()));
        }
        if chars > MAX_SNIPPET_CHARS {
            return Err(AppError::InvalidInput(format!(
                "文字最多 {MAX_SNIPPET_CHARS} 字符，当前为 {chars}"
            )));
        }
        let title = title
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| snippet_title(body));
        let source = Source {
            app: Some("VisRecall".into()),
            title: Some("新建文字".into()),
        };
        let item = self
            .persist_text(body, title, &source, None)
            .map_err(|failure| failure.error)?;
        for tag in tags {
            if let Err(err) = self.library.tag_add(&item.id, tag) {
                warn!(id = %item.id, error = %err, "手动文字标签保存失败");
            }
        }
        self.emit_created(std::slice::from_ref(&item));
        Ok(item)
    }

    pub fn emit_item_created(&self, item: &Item) {
        self.emit_created(std::slice::from_ref(item));
    }

    /// 悬浮球拒绝了一次拖放（无数据/不支持）：发 `capture_failed { DROP_REJECTED }`。
    pub fn emit_rejected(&self, message: &str) {
        self.emit_failed("DROP_REJECTED", message);
    }

    /// 上报一次导入失败（`code` 是稳定错误码，`message` 面向用户）。
    ///
    /// 除了 `ingest` 内部的逐项失败，导入工作线程起不来等基础设施问题也从这里上报。
    pub fn emit_failed(&self, code: &str, message: &str) {
        self.sink.emit(AppEvent::CaptureFailed(CaptureFailedEvent {
            code: code.to_string(),
            message: message.to_string(),
        }));
    }

    /// 来源应用：悬浮球是 `WS_EX_NOACTIVATE`，拖放期间前台仍是源应用。
    fn foreground_source(&self) -> Source {
        match self.probe.foreground() {
            Ok(info) => Source {
                app: non_empty(info.process_name),
                title: non_empty(info.title),
            },
            Err(err) => {
                warn!(error = %err, "foreground window unavailable for the drop source");
                Source::default()
            }
        }
    }

    fn import_files(
        &self,
        files: &[PathBuf],
        note: Option<&str>,
        source: &Source,
    ) -> Result<Vec<Item>, AppError> {
        if files.is_empty() {
            return Err(self.fail(ImportFailure::invalid("empty drop: no files")));
        }
        if files.len() > MAX_IMPORT_FILES {
            return Err(self.fail(ImportFailure::invalid(format!(
                "drop contains {} files; at most {MAX_IMPORT_FILES} are supported",
                files.len()
            ))));
        }

        let mut items = Vec::new();
        let mut first_error: Option<AppError> = None;
        for path in files {
            let outcome = match FileKind::of(path) {
                Ok(FileKind::Image) => self.import_image_file(path, source, note),
                Ok(FileKind::Text) => self.import_text_file(path, source),
                Err(failure) => Err(failure),
            };
            match outcome {
                Ok(item) => items.push(item),
                Err(failure) => {
                    warn!(path = %path.display(), error = %failure.error, "dropped file rejected");
                    self.emit_failure(&failure);
                    if first_error.is_none() {
                        first_error = Some(clone_error(&failure.error));
                    }
                }
            }
        }

        match (items.is_empty(), first_error) {
            (true, Some(err)) => Err(err),
            _ => Ok(items),
        }
    }

    fn import_image_file(
        &self,
        path: &Path,
        source: &Source,
        note: Option<&str>,
    ) -> Result<Item, ImportFailure> {
        let metadata = fs::metadata(path)
            .map_err(|err| ImportFailure::io(format!("read {}: {err}", path.display())))?;
        if metadata.len() > MAX_IMAGE_BYTES {
            return Err(ImportFailure::invalid(format!(
                "{} is larger than 20MB",
                file_name(path)
            )));
        }
        let bytes = fs::read(path)
            .map_err(|err| ImportFailure::io(format!("read {}: {err}", path.display())))?;
        let format = ImageFormat::from_path(path)
            .map_err(|err| ImportFailure::invalid(format!("{}: {err}", file_name(path))))?;
        let decoded = image::load_from_memory_with_format(&bytes, format)
            .map_err(|err| ImportFailure::invalid(format!("decode {}: {err}", file_name(path))))?;

        let image = limit_image_edge(decoded);
        let (width, height, png) = encode_image(&image)?;
        let title = Some(file_name(path));
        self.persist_image(width, height, &png, title, source, note)
    }

    fn import_text_file(&self, path: &Path, source: &Source) -> Result<Item, ImportFailure> {
        let metadata = fs::metadata(path)
            .map_err(|err| ImportFailure::io(format!("read {}: {err}", path.display())))?;
        if metadata.len() > MAX_TEXT_BYTES {
            return Err(ImportFailure::invalid(format!(
                "{} is larger than 500KB",
                file_name(path)
            )));
        }
        let bytes = fs::read(path)
            .map_err(|err| ImportFailure::io(format!("read {}: {err}", path.display())))?;
        let text = decode_text(&bytes);

        let title = markdown_title(&text).unwrap_or_else(|| {
            path.file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_else(|| file_name(path))
        });
        self.persist_text(&text, title, source, None)
    }

    /// 片段入库：正文写 `texts/<id>.txt`，富文本来源额外保存 HTML（复制时还原图片与格式）。
    fn import_text_snippet(
        &self,
        plain: &str,
        html: Option<&str>,
        source: &Source,
    ) -> Result<Vec<Item>, AppError> {
        let chars = plain.chars().count();
        if chars == 0 {
            return Err(self.fail(ImportFailure::invalid("empty drop text")));
        }
        if chars > MAX_SNIPPET_CHARS {
            return Err(self.fail(ImportFailure::invalid(format!(
                "drop text exceeds {MAX_SNIPPET_CHARS} characters (got {chars})"
            ))));
        }
        let title = snippet_title(plain);
        match self.persist_text(plain, title, source, html) {
            Ok(item) => Ok(vec![item]),
            Err(failure) => Err(self.fail(failure)),
        }
    }

    fn import_link(&self, url: &str, source: &Source) -> Result<Vec<Item>, AppError> {
        let title = self.link_title(url);
        match self.persist_link(url, title, source) {
            Ok(item) => Ok(vec![item]),
            Err(failure) => Err(self.fail(failure)),
        }
    }

    fn import_dib(
        &self,
        width: u32,
        height: u32,
        bgra: Vec<u8>,
        source: &Source,
    ) -> Result<Vec<Item>, AppError> {
        let expected = (width as usize)
            .checked_mul(height as usize)
            .and_then(|pixels| pixels.checked_mul(4));
        let Some(expected) = expected else {
            return Err(self.fail(ImportFailure::invalid(format!(
                "dropped image size overflow: {width}x{height}"
            ))));
        };
        if width == 0 || height == 0 || bgra.len() != expected {
            return Err(self.fail(ImportFailure::invalid(format!(
                "dropped image buffer mismatch: {} bytes for {width}x{height}",
                bgra.len()
            ))));
        }

        let mut rgba = Vec::with_capacity(bgra.len());
        for pixel in bgra.chunks_exact(4) {
            rgba.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
        }
        let Some(buffer) = RgbaImage::from_raw(width, height, rgba) else {
            return Err(self.fail(ImportFailure::invalid("dropped image buffer mismatch")));
        };
        let image = limit_image_edge(DynamicImage::ImageRgba8(buffer));
        let (width, height, png) = match encode_image(&image) {
            Ok(encoded) => encoded,
            Err(failure) => return Err(self.fail(failure)),
        };
        let title = source.title.clone();
        match self.persist_image(width, height, &png, title, source, None) {
            Ok(item) => Ok(vec![item]),
            Err(failure) => Err(self.fail(failure)),
        }
    }

    /// PNG 落盘 + 入库（`status = parsing`，缩略图由 Task 17 生成）。
    fn persist_image(
        &self,
        width: u32,
        height: u32,
        png: &[u8],
        source_title: Option<String>,
        source: &Source,
        note: Option<&str>,
    ) -> Result<Item, ImportFailure> {
        let id = new_item_id();
        let root = self.library.root();
        let (relative_path, target) = image_target(root, &id);
        write_bytes_atomic(root, &id, &target, png).map_err(ImportFailure::from_error)?;

        let item = self
            .library
            .insert_item_with_id(
                id,
                NewItem {
                    kind: ItemKind::Image,
                    captured_at: now_millis(),
                    source_app: source.app.clone(),
                    source_title,
                    source_url: None,
                    width: Some(width),
                    height: Some(height),
                    char_count: None,
                    original_path: relative_path,
                    content_hash: sha256_hex(png),
                },
            )
            .map_err(|err| {
                remove_orphan(&target);
                ImportFailure::from_error(err)
            })?;

        // 混合拖放（图片 + 文字）：备注写到图片素材上，不单独建文本条目。
        if let Some(note) = note {
            if let Err(err) = self.library.set_item_note(&item.id, note) {
                warn!(id = %item.id, error = %err, "failed to store the drop note");
            }
        }

        info!(id = %item.id, width, height, "dropped image imported");
        Ok(item)
    }

    /// 文本落盘 + 入库（无缩略图，直接 `ready`）。
    ///
    /// `html` 是富文本来源的片段（复制时写回 `CF_HTML`）；保存失败只 `warn`——
    /// 素材本身仍然可用，只是复制退化为纯文本。
    fn persist_text(
        &self,
        text: &str,
        title: String,
        source: &Source,
        html: Option<&str>,
    ) -> Result<Item, ImportFailure> {
        let id = new_item_id();
        let root = self.library.root();
        let relative_path = format!("texts/{id}.txt");
        let target = root.join(&relative_path);
        let bytes = text.as_bytes();
        write_bytes_atomic(root, &id, &target, bytes).map_err(ImportFailure::from_error)?;

        let item = self
            .library
            .insert_item_with_id(
                id,
                NewItem {
                    kind: ItemKind::Text,
                    captured_at: now_millis(),
                    source_app: source.app.clone(),
                    source_title: non_empty(title),
                    source_url: None,
                    width: None,
                    height: None,
                    char_count: Some(char_count(text)),
                    original_path: relative_path,
                    content_hash: sha256_hex(bytes),
                },
            )
            .map_err(|err| {
                remove_orphan(&target);
                ImportFailure::from_error(err)
            })?;

        if let Some(html) = html {
            if let Err(err) = self.library.set_snippet_html(&item.id, html) {
                warn!(id = %item.id, error = %err, "failed to store the snippet html");
            }
        }

        self.mark_ready(&item);
        info!(id = %item.id, chars = char_count(text), "dropped text imported");
        Ok(item)
    }

    /// 链接入库：标题由抓取结果决定（失败回退 URL），没有本地文件。
    fn persist_link(
        &self,
        url: &str,
        title: String,
        source: &Source,
    ) -> Result<Item, ImportFailure> {
        let item = self
            .library
            .insert_item(NewItem {
                kind: ItemKind::Link,
                captured_at: now_millis(),
                source_app: source.app.clone(),
                source_title: non_empty(title),
                source_url: Some(url.to_string()),
                width: None,
                height: None,
                char_count: None,
                // 链接没有本地文件：空路径表示「无原文件」。
                original_path: String::new(),
                content_hash: sha256_hex(url.as_bytes()),
            })
            .map_err(ImportFailure::from_error)?;

        self.mark_ready(&item);
        info!(id = %item.id, url, "dropped link imported");
        Ok(item)
    }

    /// 文本/链接素材没有缩略图：写盘成功后直接转 `ready`。
    fn mark_ready(&self, item: &Item) {
        if let Err(err) = self
            .library
            .update_status(&item.id, ItemStatus::Ready, None)
        {
            warn!(id = %item.id, error = %err, "failed to mark the dropped item ready");
        }
    }

    /// 抓取页面标题：`<title>` → 站点名 → URL（5s 超时，失败不阻断导入）。
    fn link_title(&self, url: &str) -> String {
        match self.fetch_title(url) {
            Ok(Some(title)) => title,
            Ok(None) => site_name(url).unwrap_or_else(|| url.to_string()),
            Err(err) => {
                warn!(url, error = %err, "failed to fetch the dropped link title");
                url.to_string()
            }
        }
    }

    fn fetch_title(&self, url: &str) -> Result<Option<String>, AppError> {
        let client = self.http_client()?;
        let response = client
            .get(url)
            .send()
            .map_err(|err| AppError::Io(format!("fetch {url}: {err}")))?;
        if !response.status().is_success() {
            return Err(AppError::Io(format!(
                "fetch {url}: HTTP {}",
                response.status().as_u16()
            )));
        }
        // 标题在 `<head>` 里，读前 512KB 足够；极端大的页面不会把整页读进内存。
        let mut reader = response.take(MAX_URL_BODY_BYTES);
        let mut body = Vec::new();
        reader
            .read_to_end(&mut body)
            .map_err(|err| AppError::Io(format!("read {url}: {err}")))?;
        // 页面编码不保证是 UTF-8：用有损解码只为了取标题。
        Ok(extract_html_title(&String::from_utf8_lossy(&body)))
    }

    fn http_client(&self) -> Result<&reqwest::blocking::Client, AppError> {
        if let Some(client) = self.http.get() {
            return Ok(client);
        }
        ensure_tls_provider();
        let client = reqwest::blocking::Client::builder()
            .timeout(URL_TIMEOUT)
            .build()
            .map_err(|err| AppError::Internal(format!("http client: {err}")))?;
        Ok(self.http.get_or_init(|| client))
    }

    fn emit_failure(&self, failure: &ImportFailure) {
        self.sink.emit(AppEvent::CaptureFailed(CaptureFailedEvent {
            code: failure.code.to_string(),
            message: failure.error.to_string(),
        }));
    }

    /// 记录失败事件并返回错误（供「整批拒绝」与单项导入使用）。
    fn fail(&self, failure: ImportFailure) -> AppError {
        self.emit_failure(&failure);
        failure.error
    }
}

/// 来源应用的元数据快照。
#[derive(Clone, Debug, Default)]
struct Source {
    app: Option<String>,
    title: Option<String>,
}

/// 单项导入失败：稳定错误码 + 领域错误。
struct ImportFailure {
    code: &'static str,
    error: AppError,
}

impl ImportFailure {
    fn rejected(message: impl Into<String>) -> Self {
        Self {
            code: "DROP_REJECTED",
            error: AppError::InvalidInput(message.into()),
        }
    }

    fn invalid(message: impl Into<String>) -> Self {
        Self {
            code: "INVALID_INPUT",
            error: AppError::InvalidInput(message.into()),
        }
    }

    fn io(message: impl Into<String>) -> Self {
        Self {
            code: "IO",
            error: AppError::Io(message.into()),
        }
    }

    fn from_error(err: AppError) -> Self {
        let code = match &err {
            AppError::InvalidInput(_) => "INVALID_INPUT",
            AppError::Io(_) => "IO",
            AppError::Database(_) => "DATABASE",
            AppError::StorageFull => "STORAGE_FULL",
            AppError::Internal(_) => "INTERNAL",
            _ => "DROP_REJECTED",
        };
        Self { code, error: err }
    }
}

/// 失败事件与返回值都要用同一条消息；`AppError` 不能 clone，按变体重建一份。
fn clone_error(err: &AppError) -> AppError {
    match err {
        AppError::ConfigMissing => AppError::ConfigMissing,
        AppError::InvalidInput(detail) => AppError::InvalidInput(detail.clone()),
        AppError::NotFound => AppError::NotFound,
        AppError::StorageFull => AppError::StorageFull,
        AppError::CaptureFailed(detail) => AppError::CaptureFailed(detail.clone()),
        AppError::CaptureCancelled => AppError::CaptureCancelled,
        AppError::DrmProtected => AppError::DrmProtected,
        AppError::FileMissing(detail) => AppError::FileMissing(detail.clone()),
        AppError::ClipboardBusy => AppError::ClipboardBusy,
        AppError::ClipboardFailed(detail) => AppError::ClipboardFailed(detail.clone()),
        AppError::Busy(detail) => AppError::Busy(detail.clone()),
        AppError::Io(detail) => AppError::Io(detail.clone()),
        AppError::Database(detail) => AppError::Database(detail.clone()),
        AppError::Internal(detail) => AppError::Internal(detail.clone()),
        AppError::ModelNotReady(detail) => AppError::ModelNotReady(detail.clone()),
        AppError::Provider(detail) => AppError::Provider(detail.clone()),
        AppError::ProviderConfig(detail) => AppError::ProviderConfig(detail.clone()),
        AppError::SchemaViolation(detail) => AppError::SchemaViolation(detail.clone()),
    }
}

/// 文件类型：只有图片与文本会被导入，其余（文件夹/快捷方式/压缩包/PDF/Office）拒绝。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FileKind {
    Image,
    Text,
}

impl FileKind {
    fn of(path: &Path) -> Result<FileKind, ImportFailure> {
        if path.is_dir() {
            return Err(ImportFailure::rejected(format!(
                "folders are not supported: {}",
                file_name(path)
            )));
        }
        match extension(path).as_deref() {
            Some("png" | "jpg" | "jpeg" | "webp" | "gif") => Ok(FileKind::Image),
            Some("txt" | "md") => Ok(FileKind::Text),
            _ => Err(ImportFailure::rejected(format!(
                "unsupported file type: {} (supported: png/jpg/jpeg/webp/gif, txt/md)",
                file_name(path)
            ))),
        }
    }
}

fn extension(path: &Path) -> Option<String> {
    path.extension()
        .map(|ext| ext.to_string_lossy().to_ascii_lowercase())
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// 文本解码：BOM 优先（UTF-16 LE/BE）→ UTF-8 → GB18030（P1-R09）。
///
/// Windows 记事本导出的「Unicode」文本是带 BOM 的 UTF-16LE（`FF FE`），按 UTF-8/GB18030
/// 解码会得到乱码，所以先按 BOM 判定；`encoding_rs` 的 `decode` 会吃掉 BOM 本身。
fn decode_text(bytes: &[u8]) -> String {
    match bytes {
        [0xFF, 0xFE, rest @ ..] => strip_bom(&UTF_16LE.decode(rest).0),
        [0xFE, 0xFF, rest @ ..] => strip_bom(&UTF_16BE.decode(rest).0),
        // UTF-8 BOM：直接跳过，避免文件开头出现 U+FEFF。
        [0xEF, 0xBB, 0xBF, _rest @ ..] => std::str::from_utf8(bytes)
            .map(|text| text.trim_start_matches('\u{feff}').to_string())
            .unwrap_or_else(|_| decode_without_bom(bytes)),
        _ => decode_without_bom(bytes),
    }
}

/// 目录引用建立元数据时复用同一套文本解码规则。
pub fn decode_imported_text(bytes: &[u8]) -> String {
    decode_text(bytes)
}

/// 去掉解码后残留的 U+FEFF（重复 BOM 或编码声明错误时可能出现）。
fn strip_bom(text: &str) -> String {
    text.trim_start_matches('\u{feff}').to_string()
}

/// 无 BOM：UTF-8 优先，失败回退 GB18030。
fn decode_without_bom(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_string(),
        Err(_) => GB18030.decode(bytes).0.into_owned(),
    }
}

/// 长边超过 [`MAX_IMAGE_EDGE`] 时按比例缩小（保持宽高比）。
fn limit_image_edge(image: DynamicImage) -> DynamicImage {
    let (width, height) = (image.width(), image.height());
    if width.max(height) <= MAX_IMAGE_EDGE {
        return image;
    }
    image.resize(
        MAX_IMAGE_EDGE,
        MAX_IMAGE_EDGE,
        image::imageops::FilterType::Lanczos3,
    )
}

fn encode_image(image: &DynamicImage) -> Result<(u32, u32, Vec<u8>), ImportFailure> {
    let rgba = image.to_rgba8();
    let (width, height) = rgba.dimensions();
    let png = encode_rgba_png(width, height, rgba.as_raw()).map_err(ImportFailure::from_error)?;
    Ok((width, height, png))
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn char_count(text: &str) -> u32 {
    u32::try_from(text.chars().count()).unwrap_or(u32::MAX)
}

fn non_empty(value: String) -> Option<String> {
    if value.trim().is_empty() {
        None
    } else {
        Some(value)
    }
}

/// 文本片段标题：首个非空行，超长截断（不跨行）。
fn snippet_title(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    truncate_chars(line, 60)
}

/// 文本文件标题：首个一级标题（`# `）；没有时由调用方取文件名。
fn markdown_title(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim_start)
        .find_map(|line| line.strip_prefix("# "))
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(|title| truncate_chars(title, 80))
}

fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut truncated: String = text.chars().take(max).collect();
    truncated.push('…');
    truncated
}

/// 链接页面的 `<title>`（大小写不敏感、空白折叠、最长 200 字符）。
fn extract_html_title(html: &str) -> Option<String> {
    let lowered = html.to_ascii_lowercase();
    let start = lowered.find("<title")?;
    let open = lowered[start..].find('>')? + start + 1;
    let end = lowered[open..].find("</title>")? + open;
    let title = html[open..end]
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if title.is_empty() {
        None
    } else {
        Some(truncate_chars(&title, 200))
    }
}

/// 站点名（URL 的 host）；用于页面没有 `<title>` 时的标题回退。
fn site_name(url: &str) -> Option<String> {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|parsed| parsed.host_str().map(str::to_string))
        .filter(|host| !host.is_empty())
}

/// rustls 单进程默认 provider：`rustls-no-provider` 要求构建客户端前安装。
fn ensure_tls_provider() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        if rustls::crypto::ring::default_provider()
            .install_default()
            .is_err()
        {
            warn!("rustls crypto provider was already installed");
        }
    });
}

/// 入库失败后删除已 `rename` 的目标文件，避免留下孤儿文件。
fn remove_orphan(target: &Path) {
    match fs::remove_file(target) {
        Ok(()) => warn!(path = %target.display(), "removed orphan drop file"),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            warn!(path = %target.display(), error = %err, "failed to remove orphan drop file")
        }
    }
}

#[cfg(test)]
mod tests {
    use visrecall_capture::WindowProbe;
    use visrecall_domain::capture::ForegroundInfo;
    use visrecall_domain::geometry::PhysicalRect;
    use visrecall_domain::WindowInfo;

    use super::*;
    use crate::events::RecordingSink;

    /// 不触碰真实桌面的窗口探针。
    struct FakeProbe;

    impl WindowProbe for FakeProbe {
        fn foreground(&self) -> Result<ForegroundInfo, AppError> {
            Ok(ForegroundInfo {
                hwnd: 0,
                pid: 42,
                process_name: "chrome.exe".into(),
                display_name: "Chrome".into(),
                title: "Example page".into(),
                bounds: PhysicalRect::new(0, 0, 100, 100),
            })
        }

        fn enumerate(&self) -> Result<Vec<WindowInfo>, AppError> {
            Ok(Vec::new())
        }

        fn hit_chain(
            &self,
            _point: visrecall_domain::PhysicalPoint,
        ) -> Result<Vec<WindowInfo>, AppError> {
            Ok(Vec::new())
        }

        fn hit_chain_excluding(
            &self,
            _point: visrecall_domain::PhysicalPoint,
            _excluded_pid: u32,
        ) -> Result<Vec<WindowInfo>, AppError> {
            Ok(Vec::new())
        }
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        flow: ImportFlow,
        library: Arc<Library>,
        sink: Arc<RecordingSink>,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let library = Arc::new(Library::open(dir.path().join("library")).unwrap());
        let sink = Arc::new(RecordingSink::default());
        let flow = ImportFlow::new(
            Arc::clone(&library),
            Arc::new(FakeProbe),
            Arc::clone(&sink) as Arc<dyn EventSink>,
        );
        Fixture {
            _dir: dir,
            flow,
            library,
            sink,
        }
    }

    fn failure_codes(sink: &RecordingSink) -> Vec<String> {
        sink.events()
            .into_iter()
            .filter_map(|event| match event {
                AppEvent::CaptureFailed(payload) => Some(payload.code),
                _ => None,
            })
            .collect()
    }

    fn write_image(path: &Path, width: u32, height: u32) {
        let image = RgbaImage::from_pixel(width, height, image::Rgba([10, 20, 30, 255]));
        let png = encode_rgba_png(width, height, image.as_raw()).unwrap();
        fs::write(path, png).unwrap();
    }

    #[test]
    fn text_snippet_is_stored_ready_with_char_count() {
        let fixture = fixture();
        let text = "第一行标题\n正文内容";
        let items = fixture
            .flow
            .ingest(DropContent::Text {
                plain: text.into(),
                html: None,
            })
            .unwrap();

        assert_eq!(items.len(), 1);
        let stored = fixture.library.get_item(&items[0].id).unwrap().unwrap();
        assert_eq!(stored.kind, ItemKind::Text);
        assert_eq!(stored.status, ItemStatus::Ready);
        assert_eq!(stored.source_title.as_deref(), Some("第一行标题"));
        assert_eq!(stored.source_app.as_deref(), Some("chrome.exe"));
        assert_eq!(stored.char_count, Some(text.chars().count() as u32));
        assert!(stored.original_path.starts_with("texts/"));
        assert_eq!(
            fs::read_to_string(fixture.library.root().join(&stored.original_path)).unwrap(),
            text
        );
        assert!(fixture.sink.events().is_empty());
    }

    /// 拖入文本的云端解析由「上传云端解析」开关控制（默认关闭）。
    #[test]
    fn dropped_text_only_queues_cloud_analysis_when_enabled() {
        let fixture = fixture();
        let off = fixture
            .flow
            .ingest(DropContent::Text {
                plain: "关闭时不上云".into(),
                html: None,
            })
            .unwrap();
        fixture.flow.emit_created(&off);
        assert!(
            fixture
                .library
                .jobs_for_stage(&off[0].id, visrecall_pipeline::STAGE_VLM)
                .unwrap()
                .is_empty(),
            "默认关闭：文本不得排队上云"
        );
        assert_eq!(
            fixture
                .library
                .jobs_for_stage(&off[0].id, visrecall_pipeline::STAGE_INDEX)
                .unwrap()
                .len(),
            1,
            "本地索引照常"
        );

        fixture.library.set_cloud_text_ai(true).unwrap();
        let on = fixture
            .flow
            .ingest(DropContent::Text {
                plain: "开启后上云".into(),
                html: None,
            })
            .unwrap();
        fixture.flow.emit_created(&on);
        assert_eq!(
            fixture
                .library
                .jobs_for_stage(&on[0].id, visrecall_pipeline::STAGE_VLM)
                .unwrap()
                .len(),
            1,
            "开启后文本要进阶段二"
        );
    }

    /// 富文本片段：正文照旧落 txt，HTML 单独存库供复制时写 `CF_HTML`。
    #[test]
    fn text_snippet_keeps_its_html_for_copy() {
        let fixture = fixture();
        let html = "<p>正文</p><img src=\"https://example.com/a.png\">";
        let items = fixture
            .flow
            .ingest(DropContent::Text {
                plain: "正文".into(),
                html: Some(html.into()),
            })
            .unwrap();

        assert_eq!(
            fixture.library.get_snippet_html(&items[0].id).unwrap().as_deref(),
            Some(html)
        );
        // 检索用的正文仍然是纯文本，HTML 不参与索引。
        let stored = fixture.library.get_item(&items[0].id).unwrap().unwrap();
        assert_eq!(
            fs::read_to_string(fixture.library.root().join(&stored.original_path)).unwrap(),
            "正文"
        );
    }

    /// 纯文本片段没有 HTML 记录：复制必须继续走纯文本分支。
    #[test]
    fn plain_text_snippet_has_no_html_record() {
        let fixture = fixture();
        let items = fixture
            .flow
            .ingest(DropContent::Text {
                plain: "没有格式".into(),
                html: None,
            })
            .unwrap();
        assert_eq!(fixture.library.get_snippet_html(&items[0].id).unwrap(), None);
    }

    #[test]
    fn oversized_snippet_is_rejected_without_rows() {
        let fixture = fixture();
        let big = "a".repeat(MAX_SNIPPET_CHARS + 1);
        let err = fixture
            .flow
            .ingest(DropContent::Text {
                plain: big,
                html: None,
            })
            .unwrap_err();
        assert!(format!("{err}").contains("10000"));
        assert!(fixture.library.list_items(None, 10).unwrap().is_empty());
        assert_eq!(failure_codes(&fixture.sink), vec!["INVALID_INPUT"]);
    }

    #[test]
    fn image_file_is_imported_as_parsing_with_png_on_disk() {
        let fixture = fixture();
        let source = fixture._dir.path().join("shot.png");
        write_image(&source, 4, 3);

        let items = fixture
            .flow
            .ingest(DropContent::Files(vec![source.clone()]))
            .unwrap();
        assert_eq!(items.len(), 1);

        let stored = fixture.library.get_item(&items[0].id).unwrap().unwrap();
        assert_eq!(stored.kind, ItemKind::Image);
        assert_eq!(stored.status, ItemStatus::Parsing);
        assert_eq!((stored.width, stored.height), (Some(4), Some(3)));
        assert_eq!(stored.source_title.as_deref(), Some("shot.png"));
        assert!(stored.original_path.starts_with("originals/images/"));
        let path = fixture.library.root().join(&stored.original_path);
        assert_eq!(
            fs::read(&path).unwrap(),
            fs::read(&source).unwrap().as_slice()
        );
    }

    /// Task 17：图片导入后 `emit_created` 派发缩略图，最终转 `ready` 并带 `thumbs/<id>.webp`。
    #[test]
    fn imported_image_gets_a_thumbnail_after_item_created() {
        let fixture = fixture();
        let source = fixture._dir.path().join("big.png");
        write_image(&source, 1000, 500);

        let items = fixture
            .flow
            .ingest(DropContent::Files(vec![source]))
            .unwrap();
        fixture.flow.emit_created(&items);
        // `item_created` 先于缩略图更新事件发出。
        assert_eq!(fixture.sink.events()[0].name(), "item_created");

        let id = items[0].id.clone();
        let thumb = format!("thumbs/{id}.webp");
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let stored = fixture.library.get_item(&id).unwrap().unwrap();
            if stored.status == ItemStatus::Ready {
                assert_eq!(stored.thumb_path.as_deref(), Some(thumb.as_str()));
                assert!(fixture.library.root().join(&thumb).exists());
                break;
            }
            assert!(
                Instant::now() < deadline,
                "imported image was not thumbnailed within 5s"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(fixture
            .sink
            .events()
            .iter()
            .any(|event| event.name() == "item_updated"));
    }

    /// 文本没有缩略图：`emit_created` 后不产生 `thumbs/` 文件，状态保持 `ready`。
    #[test]
    fn imported_text_has_no_thumbnail() {
        let fixture = fixture();
        let items = fixture
            .flow
            .ingest(DropContent::Text {
                plain: "没有缩略图".into(),
                html: None,
            })
            .unwrap();
        fixture.flow.emit_created(&items);

        let stored = fixture.library.get_item(&items[0].id).unwrap().unwrap();
        assert_eq!(stored.status, ItemStatus::Ready);
        assert_eq!(stored.thumb_path, None);
        let thumbs = fs::read_dir(fixture.library.root().join("thumbs"))
            .unwrap()
            .count();
        assert_eq!(thumbs, 0);
        assert_eq!(fixture.sink.events().len(), 1);
    }

    #[test]
    fn text_file_prefers_markdown_heading() {
        let fixture = fixture();
        let source = fixture._dir.path().join("notes.md");
        fs::write(&source, "# 设计笔记\n\n正文").unwrap();

        let items = fixture
            .flow
            .ingest(DropContent::Files(vec![source]))
            .unwrap();
        let stored = fixture.library.get_item(&items[0].id).unwrap().unwrap();
        assert_eq!(stored.source_title.as_deref(), Some("设计笔记"));
        assert_eq!(stored.status, ItemStatus::Ready);
    }

    #[test]
    fn files_with_note_store_the_note_on_images() {
        let fixture = fixture();
        let source = fixture._dir.path().join("shot.jpg");
        let image = image::RgbImage::from_pixel(2, 2, image::Rgb([1, 2, 3]));
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new(&mut jpeg)
            .encode(image.as_raw(), 2, 2, image::ExtendedColorType::Rgb8)
            .unwrap();
        fs::write(&source, jpeg).unwrap();

        let items = fixture
            .flow
            .ingest(DropContent::FilesWithNote {
                files: vec![source],
                note: "来自浏览器".into(),
            })
            .unwrap();
        assert_eq!(items.len(), 1);
        let stored = fixture.library.get_item(&items[0].id).unwrap().unwrap();
        assert_eq!(stored.note, "来自浏览器");
    }

    #[test]
    fn unsupported_file_is_rejected_but_others_still_import() {
        let fixture = fixture();
        let pdf = fixture._dir.path().join("doc.pdf");
        fs::write(&pdf, b"%PDF-1.4").unwrap();
        let text = fixture._dir.path().join("a.txt");
        fs::write(&text, "hello").unwrap();

        let items = fixture
            .flow
            .ingest(DropContent::Files(vec![pdf, text]))
            .unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].kind, ItemKind::Text);
        assert_eq!(failure_codes(&fixture.sink), vec!["DROP_REJECTED"]);
        assert_eq!(fixture.library.list_items(None, 10).unwrap().len(), 1);
    }

    #[test]
    fn too_many_files_reject_the_whole_batch() {
        let fixture = fixture();
        let files: Vec<PathBuf> = (0..=MAX_IMPORT_FILES)
            .map(|index| PathBuf::from(format!("a{index}.png")))
            .collect();
        let err = fixture.flow.ingest(DropContent::Files(files)).unwrap_err();
        assert!(format!("{err}").contains("20"));
        assert!(fixture.library.list_items(None, 10).unwrap().is_empty());
        assert_eq!(failure_codes(&fixture.sink), vec!["INVALID_INPUT"]);
    }

    #[test]
    fn oversized_image_file_is_rejected() {
        let fixture = fixture();
        let source = fixture._dir.path().join("big.png");
        let file = fs::File::create(&source).unwrap();
        file.set_len(MAX_IMAGE_BYTES + 1).unwrap();
        drop(file);

        let err = fixture
            .flow
            .ingest(DropContent::Files(vec![source]))
            .unwrap_err();
        assert!(format!("{err}").contains("20MB"));
        assert!(fixture.library.list_items(None, 10).unwrap().is_empty());
        assert_eq!(failure_codes(&fixture.sink), vec!["INVALID_INPUT"]);
    }

    #[test]
    fn dib_image_is_stored_as_png() {
        let fixture = fixture();
        // 1×1 红色，BGRA。
        let items = fixture
            .flow
            .ingest(DropContent::ImageDib {
                width: 1,
                height: 1,
                bgra: vec![0, 0, 255, 255],
            })
            .unwrap();
        let stored = fixture.library.get_item(&items[0].id).unwrap().unwrap();
        assert_eq!(stored.kind, ItemKind::Image);
        assert_eq!(stored.status, ItemStatus::Parsing);
        assert_eq!((stored.width, stored.height), (Some(1), Some(1)));
        assert!(fixture.library.root().join(&stored.original_path).exists());
    }

    #[test]
    fn unreachable_link_falls_back_to_the_url_title() {
        let fixture = fixture();
        let url = "http://127.0.0.1:1/never";
        let items = fixture
            .flow
            .ingest(DropContent::Url { url: url.into() })
            .unwrap();
        let stored = fixture.library.get_item(&items[0].id).unwrap().unwrap();
        assert_eq!(stored.kind, ItemKind::Link);
        assert_eq!(stored.status, ItemStatus::Ready);
        assert_eq!(stored.source_url.as_deref(), Some(url));
        assert_eq!(stored.source_title.as_deref(), Some(url));
        assert_eq!(stored.original_path, "");
    }

    #[test]
    fn garbled_text_file_falls_back_to_gb18030() {
        let fixture = fixture();
        let source = fixture._dir.path().join("legacy.txt");
        // GB18030 的「中文测试」。
        fs::write(&source, [0xD6, 0xD0, 0xCE, 0xC4, 0xB2, 0xE2, 0xCA, 0xD4]).unwrap();

        let items = fixture
            .flow
            .ingest(DropContent::Files(vec![source]))
            .unwrap();
        let stored = fixture.library.get_item(&items[0].id).unwrap().unwrap();
        let text = fs::read_to_string(fixture.library.root().join(&stored.original_path)).unwrap();
        assert_eq!(text, "中文测试");
    }

    #[test]
    fn html_title_and_site_name_helpers() {
        assert_eq!(
            extract_html_title("<html><head><Title>  Hello \n World </Title>"),
            Some("Hello World".into())
        );
        assert_eq!(extract_html_title("<html>no title</html>"), None);
        assert_eq!(
            site_name("https://example.com/path?q=1"),
            Some("example.com".into())
        );
        assert_eq!(site_name("not a url"), None);
    }

    /// 文本解码顺序：UTF-16 BOM → UTF-8（含 BOM）→ GB18030。
    #[test]
    fn decode_text_handles_utf16_and_utf8_boms() {
        // 「中文」：U+4E2D U+6587。
        assert_eq!(decode_text(&[0xFF, 0xFE, 0x2D, 0x4E, 0x87, 0x65]), "中文");
        assert_eq!(decode_text(&[0xFE, 0xFF, 0x4E, 0x2D, 0x65, 0x87]), "中文");
        let mut utf8_bom = vec![0xEF, 0xBB, 0xBF];
        utf8_bom.extend_from_slice("中文".as_bytes());
        assert_eq!(decode_text(&utf8_bom), "中文");
        assert_eq!(decode_text("中文".as_bytes()), "中文");
        // 无 BOM 的 GB18030 仍走回退。
        assert_eq!(decode_text(&[0xD6, 0xD0, 0xCE, 0xC4]), "中文");
    }

    /// 记事本导出的「Unicode」文本文件（UTF-16LE + BOM）导入后不能是乱码。
    #[test]
    fn utf16_text_file_is_decoded_before_gb18030() {
        let fixture = fixture();
        let source = fixture._dir.path().join("utf16.txt");
        let mut bytes = vec![0xFF, 0xFE];
        for unit in "# UTF-16 标题\n正文".encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        fs::write(&source, bytes).unwrap();

        let items = fixture
            .flow
            .ingest(DropContent::Files(vec![source]))
            .unwrap();
        let stored = fixture.library.get_item(&items[0].id).unwrap().unwrap();
        assert_eq!(stored.source_title.as_deref(), Some("UTF-16 标题"));
        let text = fs::read_to_string(fixture.library.root().join(&stored.original_path)).unwrap();
        assert_eq!(text, "# UTF-16 标题\n正文");
    }

    /// 标题抓取最多读 `MAX_URL_BODY_BYTES`：`<title>` 落在限制之后时不解析（也不读整页）。
    #[test]
    fn fetched_title_is_not_read_beyond_the_body_limit() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                // 读掉请求头（内容无关）；随后写回一个大响应体。
                let mut scratch = [0u8; 2048];
                let _ = stream.read(&mut scratch);
                let body = format!(
                    "<html><head>{}</head><title>too late</title></html>",
                    "x".repeat(600 * 1024)
                );
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\
                     Connection: close\r\n\r\n{body}",
                    body.len()
                );
                // 客户端会在 512KB 处断开：写入失败是预期的。
                let _ = stream.write_all(response.as_bytes());
            }
        });

        let fixture = fixture();
        let title = fixture
            .flow
            .fetch_title(&format!("http://127.0.0.1:{port}/"))
            .unwrap();
        assert_eq!(
            title, None,
            "a title beyond MAX_URL_BODY_BYTES must not be read"
        );
        server.join().unwrap();
    }

    #[test]
    fn oversized_image_is_downscaled() {
        let large = DynamicImage::ImageRgba8(RgbaImage::from_pixel(
            MAX_IMAGE_EDGE + 10,
            10,
            image::Rgba([0, 0, 0, 255]),
        ));
        let limited = limit_image_edge(large);
        assert_eq!(limited.width(), MAX_IMAGE_EDGE);
        assert_eq!(limited.height(), 10);
    }
}
