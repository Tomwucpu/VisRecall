//! OLE 拖放目标（design §9.2、P1-R09）：`IDropTarget` 实现 + 载荷分类 + 拖入视觉状态。
//!
//! 线程与所有权：目标对象由悬浮球线程的 STA apartment 创建并 `RegisterDragDrop`，
//! 回调也只在该线程派发。回调里**不落盘、不联网**：读完数据后投递
//! [`ShellEvent::Drop`] / [`ShellEvent::DropRejected`]，由 app-core 的导入流程处理。
//!
//! 视觉反馈：回调写入共享的 [`DropVisual`] 并 `PostMessageW` 通知球窗口，由窗口过程在
//! 悬浮球线程内放大球体并绘制动作文案；回调不直接触碰球窗口的 `RefCell` 状态。
//!
//! 支持的剪贴板格式：`CF_HDROP`、`CF_UNICODETEXT`、`CF_HTML`（注册格式
//! `HTML Format`）、`CF_DIB`/`TYMED_GDI`、`CF_BITMAP`（GDI 位图），以及拖拽源注册的
//! `PNG`。位图优先级 PNG → `CF_DIB` → `CF_BITMAP`（design §9.2）。
//!
//! `CF_BITMAP` 必须单独探测：浏览器拖网页图片通常只给这一种载体，而 OLE 只会在剪贴板
//! 场景把 `CF_BITMAP` 合成为 `CF_DIB`，拖拽的 `IDataObject` 不会。
//!
//! 只有 `HTML Format`（没有 `CF_UNICODETEXT`）的源也算文本：`DragEnter` 判定与 `Drop`
//! 读取都会回退到 HTML 片段剥离结果。`CF_DIB` 在交给 GDI 之前校验尺寸/位深，
//! 畸形头部直接拒绝（不越界读）。图文混选（位图 + 非 URL 文本 + HTML）存为**片段**，
//! 图片保留为 HTML 里的 URL，避免文字被位图吞掉。

use std::ffi::c_void;
use std::mem::size_of;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use crossbeam_channel::Sender;
use tracing::{debug, warn};
use visrecall_domain::AppError;
use windows::core::{implement, ComObject, Ref, PCWSTR};
use windows::Win32::Foundation::{HGLOBAL, HWND, LPARAM, POINTL, S_OK, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateDIBitmap, DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC, BITMAP, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, CBM_INIT, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ,
};
use windows::Win32::System::Com::{
    IDataObject, DVASPECT_CONTENT, FORMATETC, STGMEDIUM, TYMED, TYMED_GDI, TYMED_HGLOBAL,
};
use windows::Win32::System::DataExchange::RegisterClipboardFormatW;
use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::System::Ole::{
    IDropTarget, IDropTarget_Impl, RegisterDragDrop, ReleaseStgMedium, RevokeDragDrop, CF_BITMAP,
    CF_DIB, CF_HDROP, CF_UNICODETEXT, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_NONE,
};
use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::html_snippet::{fragment_of, sanitize};
use crate::ShellEvent;

pub use visrecall_domain::drop::DropContent;

/// 一次拖放最多接受的文件数；超出整批拒绝（P1-R09）。
pub const MAX_DROP_FILES: usize = 20;
/// 文本/HTML 片段的最大字符数；超出明确拒绝、不截断（P1-R09）。
pub const MAX_TEXT_CHARS: usize = 10_000;
/// 单个拖放位图的像素上限（8000×8000，与导入流程的长边上限一致）。
///
/// 超过上限直接拒绝：避免按畸形/超大位图分配巨量 BGRA 缓冲（I2 的健壮性要求）。
const MAX_DIB_PIXELS: usize = 8000 * 8000;

/// 通知悬浮球窗口重新应用拖入视觉（`WM_APP + 2`）。
pub(crate) const WM_BALL_DROP_VISUAL: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 2;

/// 拖拽源提供的格式（只做 `QueryGetData`，不读取数据）。
///
/// 位图有三种常见载体：注册格式 `PNG`（HGLOBAL）、`CF_DIB`（HGLOBAL/GDI）与
/// `CF_BITMAP`（GDI）。浏览器拖网页图片时通常只给最后一种——OLE 只在剪贴板场景
/// 把 `CF_BITMAP` 合成为 `CF_DIB`，拖拽的 `IDataObject` 不会，所以必须单独探测。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DropFormats {
    pub hdrop: bool,
    pub unicode_text: bool,
    pub html: bool,
    pub dib: bool,
    /// `CF_BITMAP`：浏览器/Office 拖图片时最常提供的 DDB 位图（OLE 不会把它合成 `CF_DIB`）。
    pub bitmap: bool,
}

/// 悬浮球窗口在拖拽中的视觉状态。
///
/// `accepted == false` 时球体显示为灰色并给出拒绝原因（如「不支持」「最多 20 个文件」）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DropVisual {
    pub active: bool,
    pub label: String,
    pub accepted: bool,
}

impl Default for DropVisual {
    fn default() -> Self {
        Self {
            active: false,
            label: String::new(),
            accepted: true,
        }
    }
}

/// 根据「可用格式 + 纯文本 + 文件路径」决定拖放载荷（纯逻辑，便于单测）。
///
/// - 有文件：返回 [`DropContent::Files`]；若拖拽源同时提供了 **HTML** 片段与非空文字，
///   返回 [`DropContent::FilesWithNote`]（浏览器「图片 + 选中文字」）。只带纯文本时
///   不视为备注：资源管理器/桌面拖文件总会附带文件名文本，那只是剪贴板回退。
/// - 无文件：单个 `http`/`https` URL（无空白）→ [`DropContent::Url`]；
///   超过 [`MAX_TEXT_CHARS`] 字符 → 错误（消息含 `10000`）；其余 → [`DropContent::Text`]。
///
/// DIB/PNG 位图不经过这里：`Drop` 处理器先解出像素再构造 [`DropContent::ImageDib`]。
pub fn classify_drop(
    formats: &DropFormats,
    plain: Option<&str>,
    files: &[PathBuf],
) -> Result<DropContent, AppError> {
    if !files.is_empty() {
        let note = plain.map(str::trim).filter(|text| !text.is_empty());
        if formats.html {
            if let Some(note) = note {
                if note.chars().count() > MAX_TEXT_CHARS {
                    return Err(text_too_long(note.chars().count()));
                }
                return Ok(DropContent::FilesWithNote {
                    files: files.to_vec(),
                    note: note.to_string(),
                });
            }
        }
        return Ok(DropContent::Files(files.to_vec()));
    }

    let Some(plain) = plain else {
        return Err(AppError::InvalidInput("empty drop content".into()));
    };
    let trimmed = plain.trim();
    if trimmed.is_empty() {
        return Err(AppError::InvalidInput("empty drop content".into()));
    }

    let chars = plain.chars().count();
    if chars > MAX_TEXT_CHARS {
        return Err(text_too_long(chars));
    }

    if let Some(url) = single_http_url(trimmed) {
        return Ok(DropContent::Url { url });
    }
    Ok(DropContent::Text {
        plain: plain.to_string(),
        html: None,
    })
}

fn text_too_long(chars: usize) -> AppError {
    AppError::InvalidInput(format!(
        "drop text exceeds {MAX_TEXT_CHARS} characters (got {chars})"
    ))
}

/// 去掉首尾空白后是单个 `http`/`https` URL（不含任何空白字符）时返回它。
fn single_http_url(text: &str) -> Option<String> {
    if text.chars().any(char::is_whitespace) {
        return None;
    }
    let parsed = url::Url::parse(text).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return None;
    }
    Some(text.to_string())
}

/// 注册拖放目标；返回的守卫在 drop 时 `RevokeDragDrop`（窗口销毁前必须释放）。
pub struct DropRegistration {
    hwnd: isize,
    _target: IDropTarget,
}

impl DropRegistration {
    /// 在已创建的悬浮球窗口上注册 OLE 拖放目标。
    ///
    /// 失败只返回错误（调用方记日志继续）：拖放不可用不应影响单击捕获。
    pub fn register(
        hwnd: HWND,
        events: Sender<ShellEvent>,
        visual: Arc<Mutex<DropVisual>>,
    ) -> Result<Self, AppError> {
        let target = BallDropTarget {
            hwnd: hwnd.0 as isize,
            events,
            visual,
            last: Mutex::new(DragDecision::rejected_static()),
        };
        let object = ComObject::new(target);
        let interface: IDropTarget = object.to_interface::<IDropTarget>();
        // SAFETY: `hwnd` 属于当前 STA 线程；`interface` 在守卫 drop 之前一直存活。
        unsafe { RegisterDragDrop(hwnd, &interface) }
            .map_err(|err| AppError::Internal(format!("RegisterDragDrop failed: {err}")))?;
        Ok(Self {
            hwnd: hwnd.0 as isize,
            _target: interface,
        })
    }
}

impl Drop for DropRegistration {
    fn drop(&mut self) {
        // SAFETY: 与 `RegisterDragDrop` 成对调用；窗口已销毁时返回错误，忽略即可。
        if let Err(err) = unsafe { RevokeDragDrop(HWND(self.hwnd as *mut c_void)) } {
            debug!(error = %err, "RevokeDragDrop failed");
        }
    }
}

/// DragEnter 算出的结果：给 OLE 的 effect + 球体提示文案。
#[derive(Clone, Debug)]
struct DragDecision {
    effect: DROPEFFECT,
    label: String,
    accepted: bool,
}

impl DragDecision {
    fn accepted(label: &str) -> Self {
        Self {
            effect: DROPEFFECT_COPY,
            label: label.to_string(),
            accepted: true,
        }
    }

    fn rejected(label: &str) -> Self {
        Self {
            effect: DROPEFFECT_NONE,
            label: label.to_string(),
            accepted: false,
        }
    }

    fn rejected_static() -> Self {
        Self::rejected("不支持")
    }

    fn visual(&self, active: bool) -> DropVisual {
        DropVisual {
            active,
            label: self.label.clone(),
            accepted: self.accepted,
        }
    }
}

#[implement(IDropTarget)]
struct BallDropTarget {
    hwnd: isize,
    events: Sender<ShellEvent>,
    visual: Arc<Mutex<DropVisual>>,
    /// DragEnter 的判定；DragOver 没有数据对象，沿用这次结果。
    last: Mutex<DragDecision>,
}

impl BallDropTarget {
    /// 更新共享视觉状态并让球窗口在悬浮球线程上重新渲染。
    fn publish_visual(&self, visual: DropVisual) {
        if let Ok(mut slot) = self.visual.lock() {
            *slot = visual;
        }
        // SAFETY: `hwnd` 是悬浮球窗口（同一 STA 线程）；PostMessageW 只是排队。
        if let Err(err) = unsafe {
            PostMessageW(
                Some(HWND(self.hwnd as *mut c_void)),
                WM_BALL_DROP_VISUAL,
                WPARAM(0),
                LPARAM(0),
            )
        } {
            debug!(error = %err, "failed to notify the floating ball about drop state");
        }
    }

    fn reset_visual(&self) {
        self.publish_visual(DropVisual::default());
    }

    fn send(&self, event: ShellEvent) {
        if self.events.send(event).is_err() {
            debug!("shell event channel closed; drop event dropped");
        }
    }
}

impl IDropTarget_Impl for BallDropTarget_Impl {
    fn DragEnter(
        &self,
        pdataobj: Ref<IDataObject>,
        _grfkeystate: MODIFIERKEYS_FLAGS,
        _pt: &POINTL,
        pdweffect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        let decision = match pdataobj.ok() {
            Ok(data) => decision_for(data),
            Err(err) => {
                debug!(error = %err, "drop without a data object");
                DragDecision::rejected_static()
            }
        };
        if let Ok(mut last) = self.last.lock() {
            *last = decision.clone();
        }
        self.publish_visual(decision.visual(true));
        // SAFETY: OLE 传入的 `pdweffect` 在本次调用内有效。
        unsafe { *pdweffect = decision.effect };
        Ok(())
    }

    fn DragOver(
        &self,
        _grfkeystate: MODIFIERKEYS_FLAGS,
        _pt: &POINTL,
        pdweffect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        let effect = self
            .last
            .lock()
            .map(|last| last.effect)
            .unwrap_or(DROPEFFECT_NONE);
        // SAFETY: 同 `DragEnter`。
        unsafe { *pdweffect = effect };
        Ok(())
    }

    fn DragLeave(&self) -> windows::core::Result<()> {
        self.reset_visual();
        Ok(())
    }

    fn Drop(
        &self,
        pdataobj: Ref<IDataObject>,
        _grfkeystate: MODIFIERKEYS_FLAGS,
        _pt: &POINTL,
        pdweffect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        let result = match pdataobj.ok() {
            Ok(data) => read_content(data),
            Err(err) => Err(AppError::InvalidInput(format!("drop without data: {err}"))),
        };
        let accepted = result.is_ok();
        match result {
            Ok(content) => {
                debug!(content = %content_summary(&content), "drop content captured");
                self.send(ShellEvent::Drop(content));
            }
            Err(err) => {
                warn!(error = %err, "drop rejected");
                self.send(ShellEvent::DropRejected(err.to_string()));
            }
        }
        self.reset_visual();
        // SAFETY: 同 `DragEnter`；读取/校验失败时不能让拖拽源以为放成功了（不报 COPY）。
        unsafe {
            *pdweffect = if accepted {
                DROPEFFECT_COPY
            } else {
                DROPEFFECT_NONE
            };
        }
        Ok(())
    }
}

/// 拖放载荷的日志摘要：只记录类型/计数/尺寸，**永不**打印文本或像素（P1-R11）。
///
/// `Debug` 打印 [`DropContent`] 会把 `Text.plain`、`FilesWithNote.note` 与
/// `ImageDib.bgra` 全量展开——debug 级别下一张 4K 位图就是几十 MB，拖放回调在
/// 拖拽源的 STA 线程上执行，会造成明显卡顿。URL 只记长度与主机名，避免把
/// 可能敏感的查询串写进日志。
fn content_summary(content: &DropContent) -> String {
    match content {
        DropContent::Files(files) => format!("Files(files={})", files.len()),
        DropContent::FilesWithNote { files, note } => format!(
            "FilesWithNote(files={}, note_chars={})",
            files.len(),
            note.chars().count()
        ),
        DropContent::Text { plain, html } => format!(
            "Text(plain_chars={}, html={})",
            plain.chars().count(),
            if html.is_some() { "some" } else { "none" }
        ),
        DropContent::Url { url } => match url::Url::parse(url) {
            Ok(parsed) => format!(
                "Url(len={}, host={})",
                url.len(),
                parsed.host_str().unwrap_or("unknown")
            ),
            Err(_) => format!("Url(len={})", url.len()),
        },
        DropContent::ImageDib {
            width,
            height,
            bgra,
        } => format!("ImageDib({width}x{height}, bgra_bytes={})", bgra.len()),
    }
}

/// DragEnter 的轻量判定：只读文件列表与文本（不解码位图）。
fn decision_for(data: &IDataObject) -> DragDecision {
    let formats = available_formats(data);
    let files = match read_optional_files(data, &formats) {
        Ok(files) => files,
        Err(err) => {
            debug!(error = %err, "drop file list is unreadable");
            return DragDecision::rejected("无法读取文件列表");
        }
    };
    let plain = plain_text_for_decision(data, &formats, &files);

    if !files.is_empty() {
        if files.len() > MAX_DROP_FILES {
            return DragDecision::rejected(&format!("最多 {MAX_DROP_FILES} 个文件"));
        }
        if files.iter().all(|path| is_image_path(path)) {
            return DragDecision::accepted("存为图片");
        }
        if files.iter().all(|path| is_text_path(path)) {
            return DragDecision::accepted("存为片段");
        }
        return DragDecision::accepted(&format!("导入 {} 个文件", files.len()));
    }

    // 图文混选（浏览器里连文字带图片一起拖）：HTML 片段与非 URL 文本都在时按片段处理，
    // 图片保留为 HTML 里的 URL；只有「图片 + 图片地址」才是纯粹的图片拖拽。
    let prose = snippet_text(plain.as_deref());
    if image_available(data, &formats) && !(formats.html && prose.is_some()) {
        return DragDecision::accepted("存为图片");
    }

    match plain {
        Some(text) => {
            if single_http_url(text.trim()).is_some() {
                DragDecision::accepted("存为链接")
            } else if text.chars().count() > MAX_TEXT_CHARS {
                DragDecision::rejected("超过 10000 字")
            } else {
                DragDecision::accepted("存为片段")
            }
        }
        None => DragDecision::rejected_static(),
    }
}

/// 拖拽源是否提供了任何可解码的位图载体。
fn image_available(data: &IDataObject, formats: &DropFormats) -> bool {
    formats.dib || formats.bitmap || png_format(data).is_some()
}

/// 非 URL、非空白的文本：只有这种文本才说明用户拖的是「文字 + 图片」而不是一张图。
fn snippet_text(plain: Option<&str>) -> Option<&str> {
    plain
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .filter(|text| single_http_url(text).is_none())
}

/// 读取 `CF_HDROP`；没有该格式时返回空列表，格式存在但读不出来时返回错误（不静默当空）。
fn read_optional_files(
    data: &IDataObject,
    formats: &DropFormats,
) -> Result<Vec<PathBuf>, AppError> {
    if !formats.hdrop {
        return Ok(Vec::new());
    }
    read_files(data)
}

/// DragEnter 用的纯文本：优先 `CF_UNICODETEXT`，没有时回退到 HTML 片段剥离结果
/// （只有 `HTML Format` 的源也能被接受，与 `read_content` 的回退一致）。
fn plain_text_for_decision(
    data: &IDataObject,
    formats: &DropFormats,
    files: &[PathBuf],
) -> Option<String> {
    let plain = if formats.unicode_text {
        read_utf16_text(data, CF_UNICODETEXT.0)
    } else {
        None
    }
    .filter(|text| !text.trim().is_empty());
    if plain.is_some() || !formats.html || !files.is_empty() {
        return plain;
    }
    read_html(data)
        .map(|raw| strip_html(fragment_of(&raw)))
        .filter(|text| !text.trim().is_empty())
}

/// 读取完整拖放载荷；文件 → 位图 → 文本的优先级与 design §9.2 一致。
///
/// 唯一例外是浏览器「文字 + 图片」混选：位图与 HTML 片段同时存在、且文本不是单个 URL 时，
/// 按片段入库（图片保留为 HTML 里的 URL），否则文字会被位图整块吞掉。
fn read_content(data: &IDataObject) -> Result<DropContent, AppError> {
    let formats = available_formats(data);
    let files = read_optional_files(data, &formats)?;

    let raw_html = if formats.html { read_html(data) } else { None };
    // 片段：清洗后入库、复制时写回 CF_HTML；HTML 剥离结果只用作纯文本回退。
    let snippet = raw_html.as_deref().and_then(sanitize);
    let html_text = raw_html
        .as_deref()
        .map(|raw| strip_html(fragment_of(raw)))
        .filter(|text| !text.trim().is_empty());
    let plain = if formats.unicode_text {
        read_utf16_text(data, CF_UNICODETEXT.0)
    } else {
        None
    }
    .or(html_text)
    .filter(|text| !text.trim().is_empty());

    if !files.is_empty() {
        return classify_drop(&formats, plain.as_deref(), &files);
    }

    if image_available(data, &formats) {
        if let Some(image) = read_image(data) {
            if let (Some(text), Some(html)) = (snippet_text(plain.as_deref()), snippet.as_deref()) {
                return Ok(DropContent::Text {
                    plain: text.to_string(),
                    html: Some(html.to_string()),
                });
            }
            return Ok(image);
        }
        debug!("bitmap drop could not be decoded; falling back to text/files");
    }

    let mut content = classify_drop(&formats, plain.as_deref(), &files)?;
    if let DropContent::Text { html: slot, .. } = &mut content {
        *slot = snippet;
    }
    Ok(content)
}

fn is_image_path(path: &Path) -> bool {
    matches!(
        extension(path).as_deref(),
        Some("png" | "jpg" | "jpeg" | "webp" | "gif")
    )
}

fn is_text_path(path: &Path) -> bool {
    matches!(extension(path).as_deref(), Some("txt" | "md"))
}

fn extension(path: &Path) -> Option<String> {
    path.extension()
        .map(|ext| ext.to_string_lossy().to_ascii_lowercase())
}

// ---------------------------------------------------------------------------
// 剪贴板格式读取
// ---------------------------------------------------------------------------

/// `STGMEDIUM` 守卫：离开作用域时 `ReleaseStgMedium`，避免泄漏全局内存/位图。
struct OwnedMedium(STGMEDIUM);

impl Drop for OwnedMedium {
    fn drop(&mut self) {
        // SAFETY: `STGMEDIUM` 由 `IDataObject::GetData` 分配，只在这里释放一次。
        unsafe { ReleaseStgMedium(&mut self.0) };
    }
}

fn format_etc(cf: u16, tymed: u32) -> FORMATETC {
    FORMATETC {
        cfFormat: cf,
        ptd: std::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed,
    }
}

fn has_format(data: &IDataObject, cf: u16, tymed: TYMED) -> bool {
    if cf == 0 {
        return false;
    }
    let format = format_etc(cf, tymed.0 as u32);
    // SAFETY: `format` 在调用期间存活；`QueryGetData` 只读。
    unsafe { data.QueryGetData(&format) == S_OK }
}

fn available_formats(data: &IDataObject) -> DropFormats {
    DropFormats {
        hdrop: has_format(data, CF_HDROP.0, TYMED_HGLOBAL),
        unicode_text: has_format(data, CF_UNICODETEXT.0, TYMED_HGLOBAL),
        html: html_format(data).is_some(),
        dib: has_format(data, CF_DIB.0, TYMED_HGLOBAL) || has_format(data, CF_DIB.0, TYMED_GDI),
        bitmap: has_format(data, CF_BITMAP.0, TYMED_GDI),
    }
}

fn register_format(name: &str) -> u16 {
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: 名称以 NUL 结尾且在调用期间存活；失败返回 0。
    unsafe { RegisterClipboardFormatW(PCWSTR(wide.as_ptr())) as u16 }
}

fn html_format_id() -> u16 {
    static ID: OnceLock<u16> = OnceLock::new();
    *ID.get_or_init(|| register_format("HTML Format"))
}

fn png_format_id() -> u16 {
    static ID: OnceLock<u16> = OnceLock::new();
    *ID.get_or_init(|| register_format("PNG"))
}

fn html_format(data: &IDataObject) -> Option<u16> {
    let id = html_format_id();
    has_format(data, id, TYMED_HGLOBAL).then_some(id)
}

/// 注册的 `PNG` 格式可用时返回其格式 id（需要能按 HGLOBAL 读取）。
fn png_format(data: &IDataObject) -> Option<u16> {
    let id = png_format_id();
    has_format(data, id, TYMED_HGLOBAL).then_some(id)
}

fn get_medium(data: &IDataObject, cf: u16, tymed: TYMED) -> Option<OwnedMedium> {
    let format = format_etc(cf, tymed.0 as u32);
    // SAFETY: `format` 在调用期间存活；返回的 `STGMEDIUM` 由守卫释放。
    let medium = unsafe { data.GetData(&format) }.ok()?;
    Some(OwnedMedium(medium))
}

fn global_handle(medium: &OwnedMedium) -> Option<HGLOBAL> {
    if medium.0.tymed & TYMED_HGLOBAL.0 as u32 == 0 {
        return None;
    }
    // SAFETY: tymed 表明联合体里是 `hGlobal`。
    let handle = unsafe { medium.0.u.hGlobal };
    (!handle.is_invalid()).then_some(handle)
}

/// 拷贝 HGLOBAL 的全部字节（调用方在 `medium` 释放前使用）。
fn copy_global(handle: HGLOBAL) -> Option<Vec<u8>> {
    let size = unsafe { GlobalSize(handle) };
    if size == 0 {
        return None;
    }
    // SAFETY: `handle` 来自 `STGMEDIUM` 且未被释放；锁定的内存在 `GlobalUnlock` 前有效。
    unsafe {
        let pointer = GlobalLock(handle) as *const u8;
        if pointer.is_null() {
            return None;
        }
        let bytes = std::slice::from_raw_parts(pointer, size).to_vec();
        let _ = GlobalUnlock(handle);
        Some(bytes)
    }
}

fn read_global_bytes(data: &IDataObject, cf: u16) -> Option<Vec<u8>> {
    let medium = get_medium(data, cf, TYMED_HGLOBAL)?;
    let handle = global_handle(&medium)?;
    copy_global(handle)
}

fn read_utf16_text(data: &IDataObject, cf: u16) -> Option<String> {
    let bytes = read_global_bytes(data, cf)?;
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .take_while(|unit| *unit != 0)
        .collect();
    if units.is_empty() {
        return None;
    }
    Some(String::from_utf16_lossy(&units))
}

fn read_html(data: &IDataObject) -> Option<String> {
    let id = html_format(data)?;
    let bytes = read_global_bytes(data, id)?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// 读取 `CF_HDROP` 里的文件列表。
///
/// 读不出来时返回错误：调用方不能把「读取失败」当成「没有文件」——那会把紧跟着读取的
/// 文本（资源管理器总会附带文件名文本）当成用户文本片段导入。列表为空同样视为错误。
fn read_files(data: &IDataObject) -> Result<Vec<PathBuf>, AppError> {
    let medium = get_medium(data, CF_HDROP.0, TYMED_HGLOBAL)
        .ok_or_else(|| AppError::InvalidInput("drop file list is unavailable".into()))?;
    let handle = global_handle(&medium)
        .ok_or_else(|| AppError::InvalidInput("drop file list is not a global handle".into()))?;
    let hdrop = HDROP(handle.0);

    // SAFETY: `hdrop` 是有效的 `CF_HDROP` 句柄，`medium` 在函数返回前一直存活。
    let count = unsafe { DragQueryFileW(hdrop, u32::MAX, None) };
    let mut files = Vec::with_capacity(count as usize);
    for index in 0..count {
        // SAFETY: 同上；长度为 0 的索引会被跳过。
        let length = unsafe { DragQueryFileW(hdrop, index, None) };
        if length == 0 {
            continue;
        }
        let mut buffer = vec![0u16; length as usize + 1];
        // SAFETY: 缓冲区长度包含结尾 NUL，`DragQueryFileW` 最多写 `length + 1` 个 UTF-16。
        let copied = unsafe { DragQueryFileW(hdrop, index, Some(&mut buffer)) };
        if copied == 0 {
            continue;
        }
        buffer.truncate(copied as usize);
        files.push(PathBuf::from(String::from_utf16_lossy(&buffer)));
    }

    // 释放 `HDROP` 的全局内存（不要用 `DragFinish`：内存归 OLE 所有）。
    drop(medium);
    if files.is_empty() {
        return Err(AppError::InvalidInput("drop file list is empty".into()));
    }
    Ok(files)
}

// ---------------------------------------------------------------------------
// 位图：注册 PNG 优先，失败回退 CF_DIB，最后回退 CF_BITMAP（GetDIBits → top-down BGRA）
// ---------------------------------------------------------------------------

fn read_image(data: &IDataObject) -> Option<DropContent> {
    if let Some(id) = png_format(data) {
        if let Some(bytes) = read_global_bytes(data, id) {
            match decode_png(&bytes) {
                Some(image) => return Some(image),
                None => debug!("registered PNG format failed to decode; falling back to CF_DIB"),
            }
        }
    }
    read_dib(data).or_else(|| read_bitmap(data))
}

fn decode_png(bytes: &[u8]) -> Option<DropContent> {
    let image = image::load_from_memory_with_format(bytes, image::ImageFormat::Png).ok()?;
    let rgba = image.to_rgba8();
    let (width, height) = rgba.dimensions();
    if width == 0 || height == 0 {
        return None;
    }
    let mut bgra = Vec::with_capacity(rgba.len());
    for pixel in rgba.pixels() {
        let [red, green, blue, alpha] = pixel.0;
        bgra.extend_from_slice(&[blue, green, red, alpha]);
    }
    Some(DropContent::ImageDib {
        width,
        height,
        bgra,
    })
}

fn read_dib(data: &IDataObject) -> Option<DropContent> {
    read_bitmap_medium(data, CF_DIB.0, TYMED_GDI)
        .or_else(|| read_bitmap_medium(data, CF_DIB.0, TYMED_HGLOBAL))
}

/// `CF_BITMAP` 只有 GDI 载体（DDB）；转码与 `CF_DIB` 的 GDI 分支完全一致。
fn read_bitmap(data: &IDataObject) -> Option<DropContent> {
    read_bitmap_medium(data, CF_BITMAP.0, TYMED_GDI)
}

fn read_bitmap_medium(data: &IDataObject, cf: u16, tymed: TYMED) -> Option<DropContent> {
    let medium = get_medium(data, cf, tymed)?;

    // SAFETY: `GetDC(None)` 返回屏幕 DC，失败时为空句柄。
    let screen = unsafe { GetDC(None) };
    if screen.is_invalid() {
        return None;
    }
    let result = convert_medium_to_bgra(&medium, screen);
    // SAFETY: 与 `GetDC` 成对。
    unsafe { ReleaseDC(None, screen) };
    result
}

fn convert_medium_to_bgra(medium: &OwnedMedium, screen: HDC) -> Option<DropContent> {
    // `CF_DIB` 可能以 GDI 位图（TYMED_GDI）或打包的 DIB（TYMED_HGLOBAL）提供。
    // TYMED_GDI 的基本校验：句柄不能为空（`is_invalid`），尺寸由 `bitmap_to_bgra` 里的
    // `GetObjectW` 复核（`bmWidth/bmHeight > 0`，像素数有上限）；句柄归 `medium` 所有。
    let (bitmap, owned) = if medium.0.tymed & TYMED_GDI.0 as u32 != 0 {
        // SAFETY: tymed 表明联合体里是 `hBitmap`；所有权仍在 `medium`。
        let bitmap = unsafe { medium.0.u.hBitmap };
        (bitmap, false)
    } else {
        let handle = global_handle(medium)?;
        let bitmap = bitmap_from_dib_global(screen, handle)?;
        (bitmap, true)
    };
    if bitmap.is_invalid() {
        return None;
    }

    let converted = bitmap_to_bgra(screen, bitmap);
    if owned {
        // SAFETY: 位图由 `bitmap_from_dib_global` 创建，只在本地使用。
        unsafe {
            let _ = DeleteObject(HGDIOBJ(bitmap.0));
        }
    }
    converted
}

/// 从打包的 `CF_DIB`（`BITMAPINFOHEADER` + 调色板 + 像素）创建 GDI 位图。
///
/// I2：`CreateDIBitmap` 会按头部读像素，因此调用前必须用 [`dib_layout`] 校验
/// 「头部 + 调色板 + `stride × |biHeight|` 全部落在 `GlobalSize` 之内」；校验不过直接
/// 放弃这次拖放（返回 `None` → 上层报 `DROP_REJECTED`），绝不越界读。
fn bitmap_from_dib_global(screen: HDC, handle: HGLOBAL) -> Option<HBITMAP> {
    let size = unsafe { GlobalSize(handle) };
    if size < size_of::<BITMAPINFOHEADER>() {
        return None;
    }

    // SAFETY: 锁定的内存在 `GlobalUnlock` 之前有效；先校验长度再读取头部。
    unsafe {
        let base = GlobalLock(handle) as *const u8;
        if base.is_null() {
            return None;
        }
        let header = &*(base as *const BITMAPINFOHEADER);
        let Some((pixel_offset, pixel_bytes)) = dib_layout(header, size) else {
            warn!(
                width = header.biWidth,
                height = header.biHeight,
                bit_count = header.biBitCount,
                global_size = size,
                "malformed CF_DIB rejected"
            );
            let _ = GlobalUnlock(handle);
            return None;
        };
        debug!(
            width = header.biWidth,
            height = header.biHeight,
            pixel_offset,
            pixel_bytes,
            "validated packed CF_DIB"
        );

        let bitmap = CreateDIBitmap(
            screen,
            Some(base as *const BITMAPINFOHEADER),
            CBM_INIT as u32,
            Some(base.add(pixel_offset) as *const c_void),
            Some(base as *const BITMAPINFO),
            DIB_RGB_COLORS,
        );
        let _ = GlobalUnlock(handle);
        (!bitmap.is_invalid()).then_some(bitmap)
    }
}

/// 校验 `BITMAPINFOHEADER` 并算出 `(像素偏移, 像素字节数)`（纯函数，便于畸形输入测试）。
///
/// 拒绝的情形（I2）：
/// - `biSize` 小于 `BITMAPINFOHEADER` 或大于整块数据；
/// - `biWidth <= 0`、`biHeight == 0`、`biPlanes != 1`；
/// - `biBitCount` 不在 `{1,4,8,16,24,32}`；
/// - `offset + stride × |biHeight| > total`，其中
///   `stride = ((biWidth × biBitCount + 31) / 32) × 4`、`offset = biSize + palette × 4`。
///
/// 所有算术都用 `checked_*`：畸形头部（超大宽高、超大 `biClrUsed`）不会溢出，只会被拒绝。
fn dib_layout(header: &BITMAPINFOHEADER, total: usize) -> Option<(usize, usize)> {
    let header_size = usize::try_from(header.biSize).ok()?;
    if header_size < size_of::<BITMAPINFOHEADER>() || header_size > total {
        return None;
    }
    if header.biWidth <= 0 || header.biHeight == 0 || header.biPlanes != 1 {
        return None;
    }
    if !matches!(header.biBitCount, 1 | 4 | 8 | 16 | 24 | 32) {
        return None;
    }

    let palette_entries = if header.biClrUsed > 0 {
        usize::try_from(header.biClrUsed).ok()?
    } else if header.biBitCount <= 8 {
        1usize << header.biBitCount
    } else {
        0
    };
    let offset = header_size.checked_add(palette_entries.checked_mul(size_of::<u32>())?)?;

    let width = usize::try_from(header.biWidth).ok()?;
    let bit_count = usize::from(header.biBitCount);
    let stride = width
        .checked_mul(bit_count)?
        .checked_add(31)?
        .checked_div(32)?
        .checked_mul(4)?;
    let height = usize::try_from(header.biHeight.unsigned_abs()).ok()?;
    let pixel_bytes = stride.checked_mul(height)?;

    if offset > total || offset.checked_add(pixel_bytes)? > total {
        return None;
    }
    Some((offset, pixel_bytes))
}

/// `GetDIBits` → top-down BGRA8；源位图没有 alpha 通道时补成不透明。
fn bitmap_to_bgra(screen: HDC, bitmap: HBITMAP) -> Option<DropContent> {
    let mut info = BITMAP::default();
    // SAFETY: `info` 是栈上输出缓冲；`bitmap` 有效。
    let got = unsafe {
        GetObjectW(
            HGDIOBJ(bitmap.0),
            size_of::<BITMAP>() as i32,
            Some(&mut info as *mut BITMAP as *mut c_void),
        )
    };
    if got == 0 || info.bmWidth <= 0 || info.bmHeight <= 0 {
        return None;
    }
    let width = info.bmWidth as u32;
    let height = info.bmHeight as u32;
    // 像素数上限：畸形/超大位图（例如 60000×60000）直接拒绝，不分配巨量缓冲。
    if (width as usize).saturating_mul(height as usize) > MAX_DIB_PIXELS {
        warn!(
            width,
            height, "drop bitmap exceeds the pixel limit; rejected"
        );
        return None;
    }
    let image_bytes = (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(4))?;

    let mut bitmap_info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width as i32,
            // 负高度 = top-down，与 design §4 的 `bgra` 约定一致。
            biHeight: -(height as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            biSizeImage: image_bytes as u32,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut pixels = vec![0u8; image_bytes];
    // SAFETY: 缓冲区按 `width * height * 4` 分配；`bitmap_info` 描述 32bpp BI_RGB 输出。
    let lines = unsafe {
        GetDIBits(
            screen,
            bitmap,
            0,
            height,
            Some(pixels.as_mut_ptr().cast()),
            &mut bitmap_info,
            DIB_RGB_COLORS,
        )
    };
    if lines == 0 {
        return None;
    }

    // 多数 `CF_DIB`（尤其来自 Office/GDI）没有 alpha 字节：透明黑会变成全透明，补不透明。
    if pixels.chunks_exact(4).all(|pixel| pixel[3] == 0) {
        for pixel in pixels.chunks_exact_mut(4) {
            pixel[3] = 255;
        }
    }

    Some(DropContent::ImageDib {
        width,
        height,
        bgra: pixels,
    })
}

// ---------------------------------------------------------------------------
// HTML → 纯文本
// ---------------------------------------------------------------------------

/// 简单标签剥离 + 实体反转义 + 空白折叠（design §9.2）。
pub(crate) fn strip_html(html: &str) -> String {
    let lowered = html.to_ascii_lowercase();
    let mut text = String::with_capacity(html.len());
    let mut index = 0;
    while index < html.len() {
        if lowered[index..].starts_with("<script") {
            if let Some(end) = lowered[index..].find("</script>") {
                index += end + "</script>".len();
                continue;
            }
        }
        if lowered[index..].starts_with("<style") {
            if let Some(end) = lowered[index..].find("</style>") {
                index += end + "</style>".len();
                continue;
            }
        }
        let byte = html.as_bytes()[index];
        if byte == b'<' {
            match html[index..].find('>') {
                Some(end) => {
                    // 块级标签换成空格，避免 `<p>a</p><p>b</p>` 粘成 `ab`；
                    // 行内标签（`<b>`、`<span>`）不插空格，保留原词的连续文本。
                    if is_block_tag(&html[index + 1..index + end]) {
                        text.push(' ');
                    }
                    index += end + 1;
                    continue;
                }
                None => break,
            }
        }
        let Some(ch) = html[index..].chars().next() else {
            break;
        };
        text.push(ch);
        index += ch.len_utf8();
    }

    let decoded = decode_entities(&text);
    collapsed_whitespace(&decoded)
}

/// 标签体（`<` 与 `>` 之间）是否属于块级/换行标签；结尾的空白折叠由调用方负责。
fn is_block_tag(tag: &str) -> bool {
    let name = tag
        .trim_start_matches('/')
        .split(|c: char| c.is_whitespace() || c == '/')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    matches!(
        name.as_str(),
        "address"
            | "article"
            | "aside"
            | "blockquote"
            | "br"
            | "dd"
            | "details"
            | "div"
            | "dl"
            | "dt"
            | "fieldset"
            | "figcaption"
            | "figure"
            | "footer"
            | "form"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "header"
            | "hr"
            | "li"
            | "main"
            | "nav"
            | "ol"
            | "p"
            | "pre"
            | "section"
            | "summary"
            | "table"
            | "tbody"
            | "td"
            | "tfoot"
            | "th"
            | "thead"
            | "tr"
            | "ul"
    )
}

fn decode_entities(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
}

fn collapsed_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use std::mem::ManuallyDrop;

    use windows::core::{implement, ComObject, BOOL};
    use windows::Win32::Foundation::{DV_E_FORMATETC, E_FAIL, E_NOTIMPL, POINT};
    use windows::Win32::System::Com::{IDataObject_Impl, STGMEDIUM_0};
    use windows::Win32::System::Memory::{GlobalAlloc, GMEM_MOVEABLE};
    use windows::Win32::UI::Shell::DROPFILES;

    use image::ImageEncoder;

    use super::*;

    #[test]
    fn strip_html_removes_tags_scripts_and_entities() {
        let html = "<style>p{color:red}</style><p>你好 &amp; <b>世界</b></p>\
                    <script>alert(1)</script>";
        assert_eq!(strip_html(html), "你好 & 世界");
    }

    #[test]
    fn image_and_text_paths_are_recognized_case_insensitively() {
        assert!(is_image_path(Path::new("a.PNG")));
        assert!(is_image_path(Path::new("a.JpEg")));
        assert!(!is_image_path(Path::new("a.txt")));
        assert!(is_text_path(Path::new("notes.MD")));
    }

    #[test]
    fn visual_defaults_to_inactive() {
        let visual = DropVisual::default();
        assert!(!visual.active);
        assert!(visual.accepted);
    }

    /// P1-R11：日志摘要只含类型/计数/尺寸，不泄漏文本、备注或像素。
    #[test]
    fn content_summary_never_leaks_text_or_pixels() {
        let files = vec![PathBuf::from("C:\\tmp\\secret.png")];
        assert_eq!(
            content_summary(&DropContent::Files(files.clone())),
            "Files(files=1)"
        );

        let with_note = DropContent::FilesWithNote {
            files,
            note: "机密备注".into(),
        };
        assert_eq!(
            content_summary(&with_note),
            "FilesWithNote(files=1, note_chars=4)"
        );

        let text = DropContent::Text {
            plain: "机密正文".into(),
            html: Some("<p>机密正文</p>".into()),
        };
        assert_eq!(content_summary(&text), "Text(plain_chars=4, html=some)");
        assert_eq!(
            content_summary(&DropContent::Text {
                plain: "x".into(),
                html: None
            }),
            "Text(plain_chars=1, html=none)"
        );

        // URL 只记长度与主机：查询串（可能是 token）不进日志。
        let url = DropContent::Url {
            url: "https://example.com/path?token=secret".into(),
        };
        let summary = content_summary(&url);
        assert!(summary.starts_with("Url(len="), "got {summary}");
        assert!(summary.contains("host=example.com"), "got {summary}");
        assert!(
            !summary.contains("token"),
            "query must not be logged: {summary}"
        );

        let image = DropContent::ImageDib {
            width: 2,
            height: 1,
            bgra: vec![1, 2, 3, 4, 5, 6, 7, 8],
        };
        assert_eq!(content_summary(&image), "ImageDib(2x1, bgra_bytes=8)");

        // 所有摘要都不应包含正文/备注内容或像素字节。
        for summary in [content_summary(&text), content_summary(&with_note)] {
            assert!(!summary.contains("机密"), "content leaked: {summary}");
        }
        assert!(!content_summary(&image).contains("255"));
    }

    // -----------------------------------------------------------------------
    // 用真实的 OLE 媒介（HGLOBAL/HDROP/BITMAPINFO）验证格式读取
    // -----------------------------------------------------------------------

    /// 假数据对象：按剪贴板格式返回 HGLOBAL 载荷，覆盖 `QueryGetData`/`GetData`。
    ///
    /// `gdi` 里的条目按 `TYMED_GDI` 返回位图句柄（所有权交给 `ReleaseStgMedium`）。
    #[implement(IDataObject)]
    struct TestDataObject {
        entries: Vec<(u16, Vec<u8>)>,
        gdi: Vec<(u16, HBITMAP)>,
    }

    impl IDataObject_Impl for TestDataObject_Impl {
        fn GetData(&self, format: *const FORMATETC) -> windows::core::Result<STGMEDIUM> {
            // SAFETY: OLE/测试代码传入的 `FORMATETC` 在调用期间有效。
            let format = unsafe { &*format };
            if let Some((_, bitmap)) = self.gdi.iter().find(|(cf, _)| *cf == format.cfFormat) {
                return Ok(STGMEDIUM {
                    tymed: TYMED_GDI.0 as u32,
                    u: STGMEDIUM_0 { hBitmap: *bitmap },
                    pUnkForRelease: ManuallyDrop::new(None),
                });
            }
            let Some((_, bytes)) = self.entries.iter().find(|(cf, _)| *cf == format.cfFormat)
            else {
                return Err(windows::core::Error::from_hresult(DV_E_FORMATETC));
            };
            let handle = global_from_bytes(bytes)?;
            Ok(STGMEDIUM {
                tymed: TYMED_HGLOBAL.0 as u32,
                u: STGMEDIUM_0 { hGlobal: handle },
                pUnkForRelease: ManuallyDrop::new(None),
            })
        }

        fn GetDataHere(
            &self,
            _format: *const FORMATETC,
            _medium: *mut STGMEDIUM,
        ) -> windows::core::Result<()> {
            Err(windows::core::Error::from_hresult(E_NOTIMPL))
        }

        fn QueryGetData(&self, format: *const FORMATETC) -> windows::core::HRESULT {
            // SAFETY: 同 `GetData`。
            let format = unsafe { &*format };
            if self.entries.iter().any(|(cf, _)| *cf == format.cfFormat)
                || self.gdi.iter().any(|(cf, _)| *cf == format.cfFormat)
            {
                S_OK
            } else {
                DV_E_FORMATETC
            }
        }

        fn GetCanonicalFormatEtc(
            &self,
            _in_format: *const FORMATETC,
            _out_format: *mut FORMATETC,
        ) -> windows::core::HRESULT {
            E_NOTIMPL
        }

        fn SetData(
            &self,
            _format: *const FORMATETC,
            _medium: *const STGMEDIUM,
            _release: BOOL,
        ) -> windows::core::Result<()> {
            Err(windows::core::Error::from_hresult(E_FAIL))
        }

        fn EnumFormatEtc(
            &self,
            _direction: u32,
        ) -> windows::core::Result<windows::Win32::System::Com::IEnumFORMATETC> {
            Err(windows::core::Error::from_hresult(E_NOTIMPL))
        }

        fn DAdvise(
            &self,
            _format: *const FORMATETC,
            _flags: u32,
            _sink: Ref<windows::Win32::System::Com::IAdviseSink>,
        ) -> windows::core::Result<u32> {
            Err(windows::core::Error::from_hresult(E_NOTIMPL))
        }

        fn DUnadvise(&self, _connection: u32) -> windows::core::Result<()> {
            Err(windows::core::Error::from_hresult(E_NOTIMPL))
        }

        fn EnumDAdvise(&self) -> windows::core::Result<windows::Win32::System::Com::IEnumSTATDATA> {
            Err(windows::core::Error::from_hresult(E_NOTIMPL))
        }
    }

    fn global_from_bytes(bytes: &[u8]) -> windows::core::Result<HGLOBAL> {
        // SAFETY: 新分配的全局内存足以容纳 `bytes`；写入后立即解锁。
        unsafe {
            let handle = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1))?;
            let pointer = GlobalLock(handle) as *mut u8;
            if pointer.is_null() {
                return Err(windows::core::Error::from_hresult(E_FAIL));
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), pointer, bytes.len());
            let _ = GlobalUnlock(handle);
            Ok(handle)
        }
    }

    fn data_object(entries: Vec<(u16, Vec<u8>)>) -> IDataObject {
        data_object_with(entries, Vec::new())
    }

    fn gdi_data_object(cf: u16, bitmap: HBITMAP) -> IDataObject {
        data_object_with(Vec::new(), vec![(cf, bitmap)])
    }

    /// 同时提供 HGLOBAL 条目与 GDI 位图的源（浏览器「文字 + 图片」拖拽的形状）。
    fn data_object_with(
        entries: Vec<(u16, Vec<u8>)>,
        gdi: Vec<(u16, HBITMAP)>,
    ) -> IDataObject {
        ComObject::new(TestDataObject { entries, gdi }).into_interface::<IDataObject>()
    }

    fn utf16_bytes(text: &str) -> Vec<u8> {
        text.encode_utf16()
            .chain(std::iter::once(0))
            .flat_map(u16::to_le_bytes)
            .collect()
    }

    /// 构造 `CF_HDROP` 载荷（`DROPFILES` + 双 NUL 结尾的宽字符路径列表）。
    fn hdrop_bytes(paths: &[&str]) -> Vec<u8> {
        let header = DROPFILES {
            pFiles: size_of::<DROPFILES>() as u32,
            pt: POINT::default(),
            fNC: BOOL(0),
            fWide: BOOL(1),
        };
        let mut bytes = vec![0u8; size_of::<DROPFILES>()];
        // SAFETY: 缓冲区至少有一个 `DROPFILES` 大小，写入头部。
        unsafe { std::ptr::write(bytes.as_mut_ptr() as *mut DROPFILES, header) };
        for path in paths {
            bytes.extend(utf16_bytes(path));
        }
        bytes.extend_from_slice(&[0, 0]);
        bytes
    }

    /// 2×1 的 24bpp DIB：左像素红、右像素蓝（bottom-up，行填充到 4 字节）。
    fn dib_bytes() -> Vec<u8> {
        let header = dib_header(2, 1, 24);
        // BGR 顺序：红 = 00 00 FF，蓝 = FF 00 00；每行 8 字节（2px*3 + 2 填充）。
        packed_dib(&header, &[0x00, 0x00, 0xFF, 0xFF, 0x00, 0x00, 0x00, 0x00])
    }

    fn dib_header(width: i32, height: i32, bit_count: u16) -> BITMAPINFOHEADER {
        BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: height,
            biPlanes: 1,
            biBitCount: bit_count,
            biCompression: BI_RGB.0,
            ..Default::default()
        }
    }

    /// `BITMAPINFOHEADER` + 像素（不含调色板）。
    fn packed_dib(header: &BITMAPINFOHEADER, pixels: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        // SAFETY: `BITMAPINFOHEADER` 是 POD，按字节追加。
        bytes.extend_from_slice(unsafe {
            std::slice::from_raw_parts(
                header as *const BITMAPINFOHEADER as *const u8,
                size_of::<BITMAPINFOHEADER>(),
            )
        });
        bytes.extend_from_slice(pixels);
        bytes
    }

    fn html_payload(body: &str) -> Vec<u8> {
        let header_len =
            "Version:0.9\r\nStartFragment:0000000000\r\nEndFragment:0000000000\r\n".len();
        let start = header_len;
        let end = start + body.len();
        format!("Version:0.9\r\nStartFragment:{start:010}\r\nEndFragment:{end:010}\r\n{body}")
            .into_bytes()
    }

    #[test]
    fn hdrop_files_are_read_through_drag_query_file() {
        let data = data_object(vec![(
            CF_HDROP.0,
            hdrop_bytes(&["C:\\tmp\\a.png", "C:\\tmp\\b.txt"]),
        )]);
        match read_content(&data).unwrap() {
            DropContent::Files(files) => {
                assert_eq!(
                    files,
                    vec![
                        PathBuf::from("C:\\tmp\\a.png"),
                        PathBuf::from("C:\\tmp\\b.txt")
                    ]
                );
            }
            other => panic!("expected Files, got {other:?}"),
        }
    }

    #[test]
    fn unicode_text_is_read_as_utf16() {
        let data = data_object(vec![(CF_UNICODETEXT.0, utf16_bytes("你好，世界"))]);
        assert_eq!(
            read_content(&data).unwrap(),
            DropContent::Text {
                plain: "你好，世界".into(),
                html: None
            }
        );
    }

    #[test]
    fn url_text_becomes_a_link_through_ole() {
        let data = data_object(vec![(
            CF_UNICODETEXT.0,
            utf16_bytes("https://example.com/page"),
        )]);
        assert_eq!(
            read_content(&data).unwrap(),
            DropContent::Url {
                url: "https://example.com/page".into()
            }
        );
    }

    #[test]
    fn html_only_drop_is_stripped_into_plain_text() {
        let id = html_format_id();
        let data = data_object(vec![(id, html_payload("<b>加粗</b> &amp; 普通"))]);
        assert!(available_formats(&data).html);
        match read_content(&data).unwrap() {
            DropContent::Text { plain, html } => {
                assert_eq!(plain, "加粗 & 普通");
                assert!(
                    html.is_some(),
                    "raw HTML should be preserved for later phases"
                );
            }
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn files_with_html_note_are_recognized_through_ole() {
        let id = html_format_id();
        let data = data_object(vec![
            (CF_HDROP.0, hdrop_bytes(&["C:\\tmp\\shot.png"])),
            (id, html_payload("<p>图注</p>")),
        ]);
        match read_content(&data).unwrap() {
            DropContent::FilesWithNote { files, note } => {
                assert_eq!(files, vec![PathBuf::from("C:\\tmp\\shot.png")]);
                assert_eq!(note, "图注");
            }
            other => panic!("expected FilesWithNote, got {other:?}"),
        }
    }

    #[test]
    fn cf_dib_is_converted_to_top_down_bgra() {
        let data = data_object(vec![(CF_DIB.0, dib_bytes())]);
        match read_content(&data).unwrap() {
            DropContent::ImageDib {
                width,
                height,
                bgra,
            } => {
                assert_eq!((width, height), (2, 1));
                assert_eq!(bgra.len(), 8);
                assert_eq!(&bgra[0..4], &[0x00, 0x00, 0xFF, 0xFF]);
                assert_eq!(&bgra[4..8], &[0xFF, 0x00, 0x00, 0xFF]);
            }
            other => panic!("expected ImageDib, got {other:?}"),
        }
    }

    #[test]
    fn registered_png_is_decoded_before_dib() {
        // 2×1 的 PNG（左红右蓝），并提供一个无效的 CF_DIB：PNG 成功时不应回退。
        let image = image::RgbaImage::from_raw(2, 1, vec![255, 0, 0, 255, 0, 0, 255, 255]).unwrap();
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(image.as_raw(), 2, 1, image::ExtendedColorType::Rgba8)
            .unwrap();

        let id = png_format_id();
        let data = data_object(vec![(id, png), (CF_DIB.0, vec![0u8; 8])]);
        assert!(png_format(&data).is_some());
        match read_image(&data).unwrap() {
            DropContent::ImageDib {
                width,
                height,
                bgra,
            } => {
                assert_eq!((width, height), (2, 1));
                assert_eq!(&bgra[0..4], &[0, 0, 255, 255], "left pixel red in BGRA");
                assert_eq!(&bgra[4..8], &[255, 0, 0, 255], "right pixel blue in BGRA");
            }
            other => panic!("expected ImageDib, got {other:?}"),
        }
    }

    #[test]
    fn invalid_png_falls_back_to_cf_dib() {
        let id = png_format_id();
        let data = data_object(vec![(id, b"not a png".to_vec()), (CF_DIB.0, dib_bytes())]);
        match read_image(&data).unwrap() {
            DropContent::ImageDib { width, height, .. } => assert_eq!((width, height), (2, 1)),
            other => panic!("expected ImageDib, got {other:?}"),
        }
    }

    #[test]
    fn texts_over_the_limit_are_rejected_through_ole() {
        let big = "a".repeat(MAX_TEXT_CHARS + 1);
        let data = data_object(vec![(CF_UNICODETEXT.0, utf16_bytes(&big))]);
        let err = read_content(&data).unwrap_err();
        assert!(format!("{err}").contains("10000"));
        assert_eq!(decision_for(&data).effect, DROPEFFECT_NONE);
    }

    // -----------------------------------------------------------------------
    // Fix round 1：HTML-only 拖拽源、块级标签空格、畸形 DIB、GDI 位图
    // -----------------------------------------------------------------------

    /// 只有 `HTML Format`（没有 `CF_UNICODETEXT`）的源：DragEnter 也要接受，
    /// 判定依据是剥离后的 HTML 文本（与 `read_content` 的回退一致）。
    #[test]
    fn html_only_drag_enter_is_accepted() {
        let id = html_format_id();
        let data = data_object(vec![(id, html_payload("<p>只有 HTML 的拖拽源</p>"))]);

        let decision = decision_for(&data);
        assert!(decision.accepted, "HTML-only drop must be accepted");
        assert_eq!(decision.effect, DROPEFFECT_COPY);
        assert_eq!(decision.label, "存为片段");

        match read_content(&data).unwrap() {
            DropContent::Text { plain, .. } => assert_eq!(plain, "只有 HTML 的拖拽源"),
            other => panic!("expected Text, got {other:?}"),
        }
    }

    /// HTML-only 的 URL 片段同样走链接判定。
    #[test]
    fn html_only_url_fragment_is_detected_as_a_link() {
        let id = html_format_id();
        let data = data_object(vec![(id, html_payload("<a>https://example.com/x</a>"))]);
        assert_eq!(decision_for(&data).label, "存为链接");
    }

    /// 块级/相邻标签之间要有空格，行内标签不能拆词。
    #[test]
    fn strip_html_keeps_block_text_apart() {
        assert_eq!(strip_html("<p>第一段</p><p>第二段</p>"), "第一段 第二段");
        assert_eq!(strip_html("a<br/>b"), "a b");
        assert_eq!(strip_html("<ul><li>一</li><li>二</li></ul>"), "一 二");
        assert_eq!(strip_html("<h1>标题</h1><div>正文</div>"), "标题 正文");
        // 行内标签保持连续（不把词拆开）。
        assert_eq!(strip_html("<b>加粗</b><span>文本</span>"), "加粗文本");
        assert_eq!(strip_html("<p>五 <b>六</b></p>"), "五 六");
    }

    /// `dib_layout`：合法布局给出像素偏移/字节数，畸形头部一律拒绝（不 panic、不溢出）。
    #[test]
    fn dib_layout_validates_bounds_and_bit_count() {
        let header = dib_header(2, 1, 24);
        let header_size = size_of::<BITMAPINFOHEADER>();
        let full = header_size + 8;
        // 2px × 24bpp = 8 字节（行已 4 字节对齐）。
        assert_eq!(dib_layout(&header, full), Some((header_size, 8)));

        // 截断：像素区少一个字节。
        assert_eq!(dib_layout(&header, full - 1), None);
        // 数据块连头部都放不下。
        assert_eq!(dib_layout(&header, header_size - 1), None);

        // 不支持的位深。
        let mut bad = header;
        bad.biBitCount = 7;
        assert_eq!(dib_layout(&bad, full), None);
        bad.biBitCount = 0;
        assert_eq!(dib_layout(&bad, full), None);

        // 零宽 / 零高 / 多平面。
        let mut bad = header;
        bad.biWidth = 0;
        assert_eq!(dib_layout(&bad, full), None);
        let mut bad = header;
        bad.biHeight = 0;
        assert_eq!(dib_layout(&bad, full), None);
        let mut bad = header;
        bad.biPlanes = 2;
        assert_eq!(dib_layout(&bad, full), None);

        // 8bpp 默认 256 色调色板：只有头部时调色板就越界。
        let header8 = dib_header(2, 1, 8);
        assert_eq!(dib_layout(&header8, header_size), None);
        assert!(dib_layout(&header8, header_size + 256 * 4 + 4).is_some());

        // 负高度（top-down）按绝对值算。
        let mut top_down = dib_header(2, 1, 24);
        top_down.biHeight = -1;
        assert_eq!(dib_layout(&top_down, full), Some((header_size, 8)));

        // 超大宽高不溢出，只被拒绝。
        let huge = dib_header(i32::MAX, i32::MAX, 32);
        assert_eq!(dib_layout(&huge, 4096), None);

        // 32bpp 1×1 = 4 字节。
        assert_eq!(
            dib_layout(&dib_header(1, 1, 32), header_size + 4),
            Some((header_size, 4))
        );
    }

    /// 截断的 `CF_DIB`（头部声称 2×1 24bpp，实际只有 4 字节像素）必须在
    /// `CreateDIBitmap` 之前被拒绝：不崩溃，Drop 报错。
    #[test]
    fn truncated_dib_is_rejected_through_ole() {
        let header = dib_header(2, 1, 24);
        let data = data_object(vec![(CF_DIB.0, packed_dib(&header, &[0; 4]))]);
        assert!(read_image(&data).is_none(), "truncated DIB must not decode");
        assert!(
            read_content(&data).is_err(),
            "truncated DIB drop is rejected"
        );
    }

    /// 非法 `biBitCount` 的 `CF_DIB` 同样拒绝。
    #[test]
    fn invalid_bit_count_dib_is_rejected_through_ole() {
        let mut header = dib_header(2, 1, 24);
        header.biBitCount = 3;
        let data = data_object(vec![(CF_DIB.0, packed_dib(&header, &[0; 8]))]);
        assert!(read_image(&data).is_none());
        assert!(read_content(&data).is_err());
    }

    /// `TYMED_GDI` 分支：空句柄直接拒绝（不调用 `GetDIBits`）。
    #[test]
    fn null_gdi_bitmap_handle_is_rejected() {
        let data = gdi_data_object(CF_DIB.0, HBITMAP(std::ptr::null_mut()));
        assert!(read_image(&data).is_none());
    }

    /// `TYMED_GDI` 分支：真实 DDB 能转成 top-down BGRA。
    ///
    /// 句柄所有权随 `STGMEDIUM` 交给 `ReleaseStgMedium`，测试不手动删除。
    #[test]
    fn gdi_bitmap_medium_is_converted_to_bgra() {
        // 2×1 的 32bpp DDB（BGRA：左红右蓝）。
        let bits: [u8; 8] = [0x00, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0x00, 0x00];
        // SAFETY: 位缓冲区在调用期间存活；失败返回空句柄。
        let bitmap = unsafe {
            windows::Win32::Graphics::Gdi::CreateBitmap(2, 1, 1, 32, Some(bits.as_ptr().cast()))
        };
        assert!(!bitmap.is_invalid(), "CreateBitmap failed");

        let data = gdi_data_object(CF_DIB.0, bitmap);
        match read_image(&data) {
            Some(DropContent::ImageDib {
                width,
                height,
                bgra,
            }) => {
                assert_eq!((width, height), (2, 1));
                assert_eq!(bgra.len(), 8);
                // 源位图没有 alpha：转出来必须是不透明的。
                assert!(bgra.chunks_exact(4).all(|pixel| pixel[3] == 255));
            }
            other => panic!("expected ImageDib, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // 浏览器拖图片：CF_BITMAP 载体与「文字 + 图片」混选
    // -----------------------------------------------------------------------

    /// 2×1 的 32bpp DDB（左红右蓝）；所有权交给 `ReleaseStgMedium`。
    fn test_bitmap() -> HBITMAP {
        let bits: [u8; 8] = [0x00, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0x00, 0x00];
        // SAFETY: 位缓冲区在调用期间存活；失败返回空句柄。
        let bitmap = unsafe {
            windows::Win32::Graphics::Gdi::CreateBitmap(2, 1, 1, 32, Some(bits.as_ptr().cast()))
        };
        assert!(!bitmap.is_invalid(), "CreateBitmap failed");
        bitmap
    }

    /// 浏览器拖网页图片：只给 `CF_BITMAP`。以前会被判成「存为链接」，现在必须是图片。
    #[test]
    fn cf_bitmap_drop_is_decoded_as_an_image() {
        let data = gdi_data_object(CF_BITMAP.0, test_bitmap());

        assert!(available_formats(&data).bitmap, "CF_BITMAP must be detected");
        match read_content(&data).unwrap() {
            DropContent::ImageDib { width, height, .. } => assert_eq!((width, height), (2, 1)),
            other => panic!("expected ImageDib, got {other:?}"),
        }
        assert_eq!(decision_for(&data).label, "存为图片");
    }

    /// 「文字 + 图片」混选：位图与 HTML 同时存在时按片段入库，文字不能被位图吞掉。
    #[test]
    fn text_and_image_drop_becomes_a_snippet_with_html() {
        let html_id = html_format_id();
        let body = "<p>图片说明文字</p><img src=\"https://example.com/a.png\">";
        let data = data_object_with(
            vec![
                (CF_UNICODETEXT.0, utf16_bytes("图片说明文字")),
                (html_id, html_payload(body)),
            ],
            vec![(CF_BITMAP.0, test_bitmap())],
        );

        assert_eq!(decision_for(&data).label, "存为片段");
        match read_content(&data).unwrap() {
            DropContent::Text { plain, html } => {
                assert_eq!(plain, "图片说明文字");
                let html = html.expect("html snippet must be kept for pasting");
                assert!(
                    html.contains("https://example.com/a.png"),
                    "image must stay referenced: {html}"
                );
            }
            other => panic!("expected Text, got {other:?}"),
        }
    }

    /// 网页单图：位图 + HTML，但文本就是图片地址 —— 仍然按图片入库。
    #[test]
    fn image_drop_with_url_text_stays_an_image() {
        let html_id = html_format_id();
        let url = "https://example.com/a.png";
        let data = data_object_with(
            vec![
                (CF_UNICODETEXT.0, utf16_bytes(url)),
                (html_id, html_payload(&format!("<img src=\"{url}\">"))),
            ],
            vec![(CF_BITMAP.0, test_bitmap())],
        );

        assert_eq!(decision_for(&data).label, "存为图片");
        match read_content(&data).unwrap() {
            DropContent::ImageDib { width, height, .. } => assert_eq!((width, height), (2, 1)),
            other => panic!("expected ImageDib, got {other:?}"),
        }
    }
}
