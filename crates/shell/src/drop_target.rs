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
//! `HTML Format`）、`CF_DIB`/`TYMED_GDI`，以及拖拽源注册的 `PNG`。PNG 优先于
//! `CF_DIB`：解码失败再回退 `CF_DIB`（design §9.2）。

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
    IDropTarget, IDropTarget_Impl, RegisterDragDrop, ReleaseStgMedium, RevokeDragDrop, CF_DIB,
    CF_HDROP, CF_UNICODETEXT, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_NONE,
};
use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::ShellEvent;

pub use visrecall_domain::drop::DropContent;

/// 一次拖放最多接受的文件数；超出整批拒绝（P1-R09）。
pub const MAX_DROP_FILES: usize = 20;
/// 文本/HTML 片段的最大字符数；超出明确拒绝、不截断（P1-R09）。
pub const MAX_TEXT_CHARS: usize = 10_000;

/// 通知悬浮球窗口重新应用拖入视觉（`WM_APP + 2`）。
pub(crate) const WM_BALL_DROP_VISUAL: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 2;

/// 拖拽源提供的格式（只做 `QueryGetData`，不读取数据）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DropFormats {
    pub hdrop: bool,
    pub unicode_text: bool,
    pub html: bool,
    pub dib: bool,
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
        match result {
            Ok(content) => {
                debug!(?content, "drop content captured");
                self.send(ShellEvent::Drop(content));
            }
            Err(err) => {
                warn!(error = %err, "drop rejected");
                self.send(ShellEvent::DropRejected(err.to_string()));
            }
        }
        self.reset_visual();
        // SAFETY: 同 `DragEnter`；读取失败时也不再显示「可放下」光标。
        unsafe { *pdweffect = DROPEFFECT_COPY };
        Ok(())
    }
}

/// DragEnter 的轻量判定：只读文件列表与文本（不解码位图）。
fn decision_for(data: &IDataObject) -> DragDecision {
    let formats = available_formats(data);
    let files = if formats.hdrop {
        read_files(data).unwrap_or_default()
    } else {
        Vec::new()
    };
    let plain = if formats.unicode_text {
        read_utf16_text(data, CF_UNICODETEXT.0)
    } else {
        None
    }
    .filter(|text| !text.trim().is_empty());

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

    if formats.dib || png_format(data).is_some() {
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

/// 读取完整拖放载荷；文件 → 文本 → 位图的优先级与 design §9.2 一致。
fn read_content(data: &IDataObject) -> Result<DropContent, AppError> {
    let formats = available_formats(data);
    let files = if formats.hdrop {
        read_files(data).unwrap_or_default()
    } else {
        Vec::new()
    };

    let html = if formats.html {
        read_html(data).map(|raw| {
            let fragment = html_fragment(&raw).to_string();
            (raw, strip_html(&fragment))
        })
    } else {
        None
    };
    let plain = if formats.unicode_text {
        read_utf16_text(data, CF_UNICODETEXT.0)
    } else {
        None
    }
    .or_else(|| html.as_ref().map(|(_, text)| text.clone()))
    .filter(|text| !text.trim().is_empty());

    if !files.is_empty() {
        return classify_drop(&formats, plain.as_deref(), &files);
    }

    if formats.dib || png_format(data).is_some() {
        if let Some(image) = read_image(data) {
            return Ok(image);
        }
    }

    let mut content = classify_drop(&formats, plain.as_deref(), &files)?;
    if let DropContent::Text { html: slot, .. } = &mut content {
        *slot = html.map(|(raw, _)| raw);
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

fn read_files(data: &IDataObject) -> Option<Vec<PathBuf>> {
    let medium = get_medium(data, CF_HDROP.0, TYMED_HGLOBAL)?;
    let handle = global_handle(&medium)?;
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
    Some(files)
}

// ---------------------------------------------------------------------------
// 位图：注册 PNG 优先，失败回退 CF_DIB（GetDIBits → top-down BGRA）
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
    read_dib(data)
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
    let medium = get_medium(data, CF_DIB.0, TYMED_GDI)
        .or_else(|| get_medium(data, CF_DIB.0, TYMED_HGLOBAL))?;

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
        let valid = header.biSize as usize >= size_of::<BITMAPINFOHEADER>()
            && header.biWidth > 0
            && header.biHeight != 0;
        if !valid {
            let _ = GlobalUnlock(handle);
            return None;
        }

        let palette_entries = if header.biClrUsed > 0 {
            header.biClrUsed as usize
        } else if header.biBitCount <= 8 {
            1usize << header.biBitCount
        } else {
            0
        };
        let offset = header.biSize as usize + palette_entries * size_of::<u32>();
        if offset > size {
            let _ = GlobalUnlock(handle);
            return None;
        }

        let bitmap = CreateDIBitmap(
            screen,
            Some(base as *const BITMAPINFOHEADER),
            CBM_INIT as u32,
            Some(base.add(offset) as *const c_void),
            Some(base as *const BITMAPINFO),
            DIB_RGB_COLORS,
        );
        let _ = GlobalUnlock(handle);
        (!bitmap.is_invalid()).then_some(bitmap)
    }
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

/// 取 `CF_HTML` 的 fragment 区间；没有 `StartFragment`/`EndFragment` 头时返回全文。
fn html_fragment(raw: &str) -> &str {
    let start = header_offset(raw, "StartFragment:");
    let end = header_offset(raw, "EndFragment:");
    match (start, end) {
        (Some(start), Some(end)) if start < end && end <= raw.len() => {
            // 偏移必须落在字符边界上，否则按全文处理。
            if raw.is_char_boundary(start) && raw.is_char_boundary(end) {
                &raw[start..end]
            } else {
                raw
            }
        }
        _ => raw,
    }
}

fn header_offset(raw: &str, key: &str) -> Option<usize> {
    let index = raw.find(key)?;
    let digits: String = raw[index + key.len()..]
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

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
    fn html_fragment_uses_declared_offsets() {
        let body = "<html>hello</html>";
        // CF_HTML 头里的偏移是相对整个数据块的字节位置（固定宽度数字）。
        let header_len =
            "Version:0.9\r\nStartFragment:0000000000\r\nEndFragment:0000000000\r\n".len();
        let start = header_len;
        let end = start + body.len();
        let raw =
            format!("Version:0.9\r\nStartFragment:{start:010}\r\nEndFragment:{end:010}\r\n{body}");
        assert_eq!(html_fragment(&raw), body);
        assert_eq!(html_fragment("<b>hi</b>"), "<b>hi</b>");
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

    // -----------------------------------------------------------------------
    // 用真实的 OLE 媒介（HGLOBAL/HDROP/BITMAPINFO）验证格式读取
    // -----------------------------------------------------------------------

    /// 假数据对象：按剪贴板格式返回 HGLOBAL 载荷，覆盖 `QueryGetData`/`GetData`。
    #[implement(IDataObject)]
    struct TestDataObject {
        entries: Vec<(u16, Vec<u8>)>,
    }

    impl IDataObject_Impl for TestDataObject_Impl {
        fn GetData(&self, format: *const FORMATETC) -> windows::core::Result<STGMEDIUM> {
            // SAFETY: OLE/测试代码传入的 `FORMATETC` 在调用期间有效。
            let format = unsafe { &*format };
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
            if self.entries.iter().any(|(cf, _)| *cf == format.cfFormat) {
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
        ComObject::new(TestDataObject { entries }).into_interface::<IDataObject>()
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
        let header = BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: 2,
            biHeight: 1,
            biPlanes: 1,
            biBitCount: 24,
            biCompression: BI_RGB.0,
            biSizeImage: 8,
            ..Default::default()
        };
        let mut bytes = Vec::new();
        // SAFETY: `BITMAPINFOHEADER` 是 POD，按字节追加。
        bytes.extend_from_slice(unsafe {
            std::slice::from_raw_parts(
                &header as *const BITMAPINFOHEADER as *const u8,
                size_of::<BITMAPINFOHEADER>(),
            )
        });
        // BGR 顺序：红 = 00 00 FF，蓝 = FF 00 00；每行 8 字节（2px*3 + 2 填充）。
        bytes.extend_from_slice(&[0x00, 0x00, 0xFF, 0xFF, 0x00, 0x00, 0x00, 0x00]);
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
}
