//! 把原始内容写入系统剪贴板（design §6.4）。
//!
//! 各种素材的目标格式：
//! - 图片：同时写 `CF_DIB`（32bpp BGRA、自底向上）与注册格式 `PNG`（完整 PNG 字节）。
//!   两个格式都要写——Word/微信读 `CF_DIB`，Chrome 与部分应用优先读 `PNG`；
//! - 文本：`CF_UNICODETEXT`，行尾统一 CRLF，不附加摘要/标签/来源；
//! - 片段（带 HTML）：`CF_HTML` + `CF_UNICODETEXT`，粘贴时图片与格式由目标应用按
//!   HTML 里的 URL 还原；
//! - 链接：`CF_UNICODETEXT` 的原始 URL（由调用方传入）。
//!
//! 三条容易写错的细节：
//! 1. **不写 `BITMAPFILEHEADER`**：`CF_DIB` 就是裸的 `BITMAPINFOHEADER` + 像素数据；
//!    多写 14 字节文件头会让部分应用把图像解析成全黑或直接粘贴失败。
//! 2. `SetClipboardData` 成功后内存**归系统所有**，绝不能 `GlobalFree`。
//! 3. `OpenClipboard` 会因其他进程占用失败，需要重试；任何提前返回都必须 `CloseClipboard`。
//!
//! 纯函数（[`dib_bytes`] / [`utf16_crlf`]）单独拆出来做单测；OS 调用只在手工验收里跑
//! （剪贴板是全局单例，CI 上并行测试会互相干扰）。

use std::thread;
use std::time::Duration;

use visrecall_domain::AppError;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{GlobalFree, HANDLE};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::{CF_DIB, CF_UNICODETEXT};

/// 剪贴板占用时的重试次数与间隔。
const OPEN_RETRIES: u32 = 5;
const OPEN_RETRY_DELAY: Duration = Duration::from_millis(20);

/// 注册格式名：完整 PNG 文件字节（含 8 字节签名）。
const PNG_FORMAT_NAME: &str = "PNG";

/// `BITMAPINFOHEADER` 的字节数。
const BITMAPINFOHEADER_SIZE: u32 = 40;

/// 构造 `CF_DIB` 数据：40 字节 `BITMAPINFOHEADER` + 自底向上的 BGRA 行。
///
/// - `biHeight` 为正数表示 bottom-up，因此行序从最后一行开始；
/// - `biBitCount = 32`、`biCompression = BI_RGB`、`biSizeImage = w * h * 4`；
/// - 不包含 `BITMAPFILEHEADER`。
pub fn dib_bytes(rgba: &[u8], width: u32, height: u32) -> Vec<u8> {
    let (width_usize, height_usize) = (width as usize, height as usize);
    let row = width_usize * 4;
    let mut out = Vec::with_capacity(BITMAPINFOHEADER_SIZE as usize + row * height_usize);

    out.extend_from_slice(&BITMAPINFOHEADER_SIZE.to_le_bytes()); // biSize
    out.extend_from_slice(&(width as i32).to_le_bytes()); // biWidth
    out.extend_from_slice(&(height as i32).to_le_bytes()); // biHeight（正数 = bottom-up）
    out.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
    out.extend_from_slice(&32u16.to_le_bytes()); // biBitCount
    out.extend_from_slice(&0u32.to_le_bytes()); // biCompression = BI_RGB
    out.extend_from_slice(&((row * height_usize) as u32).to_le_bytes()); // biSizeImage
    out.extend_from_slice(&[0u8; 16]); // biXPelsPerMeter / biYPelsPerMeter / clrUsed / clrImportant

    for y in (0..height_usize).rev() {
        for x in 0..width_usize {
            let index = (y * width_usize + x) * 4;
            out.extend_from_slice(&[
                rgba[index + 2], // B
                rgba[index + 1], // G
                rgba[index],     // R
                rgba[index + 3], // A
            ]);
        }
    }
    out
}

/// 把文本转成 `CF_UNICODETEXT` 需要的 UTF-16 序列：CRLF 化 + 结尾 NUL。
///
/// 先把 CRLF 归一成 LF，再统一换成 CRLF，避免 `\r\n` 变成 `\r\r\n`。
pub fn utf16_crlf(text: &str) -> Vec<u16> {
    let normalized = text.replace("\r\n", "\n").replace('\n', "\r\n");
    normalized
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect()
}

/// 写图片到剪贴板：`CF_DIB` + 注册格式 `PNG`。
///
/// 失败语义：剪贴板被占用（重试 5×20ms 仍失败）→ [`AppError::ClipboardBusy`]；
/// PNG 解码失败/内存分配失败/`SetClipboardData` 失败 → [`AppError::ClipboardFailed`]。
pub fn copy_image_png(png: &[u8]) -> Result<(), AppError> {
    let image = image::load_from_memory(png)
        .map_err(|err| AppError::ClipboardFailed(format!("decode png: {err}")))?;
    let rgba = image.to_rgba8();
    let (width, height) = rgba.dimensions();
    let dib = dib_bytes(rgba.as_raw(), width, height);

    let _guard = open_clipboard_with_retry()?;
    empty_clipboard()?;

    let png_format = register_format(PNG_FORMAT_NAME)?;
    set_global(u32::from(CF_DIB.0), &dib)?;
    // PNG 格式失败时不回收已写入的 CF_DIB：系统已持有它，且图片仍可被 Word/微信读取。
    set_global(png_format, png)
}

/// 写纯文本到剪贴板（`CF_UNICODETEXT`，CRLF 行尾）。
pub fn copy_text(text: &str) -> Result<(), AppError> {
    let bytes = utf16_bytes(text);

    let _guard = open_clipboard_with_retry()?;
    empty_clipboard()?;
    set_global(u32::from(CF_UNICODETEXT.0), &bytes)
}

/// 写片段到剪贴板：`CF_HTML` + `CF_UNICODETEXT`。
///
/// 像浏览器复制选中内容那样：HTML 让 Word/WPS/网页编辑器按原样还原图片与格式
/// （图片保留源站 URL），纯文本保证只认文本的应用也能粘。
/// `CF_HTML` 写入失败时**降级**为纯文本并返回成功——粘贴出文字好过粘贴出空。
pub fn copy_html(fragment: &str, plain: &str) -> Result<(), AppError> {
    let html = crate::html_snippet::cf_html_bytes(fragment);
    let text = utf16_bytes(plain);

    let _guard = open_clipboard_with_retry()?;
    empty_clipboard()?;

    let html_result = register_format(crate::html_snippet::HTML_FORMAT_NAME)
        .and_then(|format| set_global(format, &html));
    if let Err(err) = &html_result {
        tracing::warn!(error = %err, "failed to write CF_HTML; falling back to plain text");
    }
    set_global(u32::from(CF_UNICODETEXT.0), &text)
}

/// `CF_UNICODETEXT` 的字节序列：CRLF 行尾 + 结尾 NUL，UTF-16LE。
fn utf16_bytes(text: &str) -> Vec<u8> {
    let utf16 = utf16_crlf(text);
    let mut bytes = Vec::with_capacity(utf16.len() * 2);
    for unit in &utf16 {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes
}

/// 保证任何提前返回都关闭剪贴板。
struct ClipboardGuard;

impl ClipboardGuard {
    /// 打开剪贴板；被其他进程占用时重试 5 次、每次间隔 20ms。
    fn open() -> Result<ClipboardGuard, AppError> {
        for attempt in 0..OPEN_RETRIES {
            // SAFETY: 不传窗口句柄表示与当前任务关联；失败只是「被占用」，无资源需要清理。
            if unsafe { OpenClipboard(None) }.is_ok() {
                return Ok(ClipboardGuard);
            }
            if attempt + 1 < OPEN_RETRIES {
                thread::sleep(OPEN_RETRY_DELAY);
            }
        }
        Err(AppError::ClipboardBusy)
    }
}

impl Drop for ClipboardGuard {
    fn drop(&mut self) {
        // SAFETY: 只有 `open()` 成功才会构造本类型，因此此时剪贴板确实由我们持有。
        unsafe {
            let _ = CloseClipboard();
        }
    }
}

fn open_clipboard_with_retry() -> Result<ClipboardGuard, AppError> {
    ClipboardGuard::open()
}

fn empty_clipboard() -> Result<(), AppError> {
    // SAFETY: 调用前已成功 `OpenClipboard`（guard 存活）。
    unsafe { EmptyClipboard() }
        .map_err(|err| AppError::ClipboardFailed(format!("EmptyClipboard: {err}")))
}

/// 注册自定义剪贴板格式，返回其 uformat 编号。
fn register_format(name: &str) -> Result<u32, AppError> {
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: 字符串以 NUL 结尾且在调用期间存活。
    let format = unsafe { RegisterClipboardFormatW(PCWSTR(wide.as_ptr())) };
    if format == 0 {
        return Err(AppError::ClipboardFailed(format!(
            "RegisterClipboardFormatW({name}) failed"
        )));
    }
    Ok(format)
}

/// 把 `bytes` 放进全局内存并交给剪贴板。
///
/// 成功后内存归系统所有，**不得** `GlobalFree`；失败路径必须在 `SetClipboardData`
/// 之前释放，避免泄漏。
fn set_global(format: u32, bytes: &[u8]) -> Result<(), AppError> {
    if bytes.is_empty() {
        return Err(AppError::ClipboardFailed("empty payload".into()));
    }

    // SAFETY: 分配 `bytes.len()` 字节的可移动全局内存。
    let handle = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes.len()) }
        .map_err(|err| AppError::ClipboardFailed(format!("GlobalAlloc: {err}")))?;

    // SAFETY: 句柄刚由 GlobalAlloc 返回且未移交系统，锁定的指针在 GlobalUnlock 前有效。
    let ptr = unsafe { GlobalLock(handle) };
    if ptr.is_null() {
        // SAFETY: 句柄仍归我们所有且未交给系统，可以释放。
        unsafe {
            let _ = GlobalFree(Some(handle));
        }
        return Err(AppError::ClipboardFailed("GlobalLock returned null".into()));
    }
    // SAFETY: 目标缓冲区恰好 `bytes.len()` 字节，源与目标不重叠。
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr as *mut u8, bytes.len());
        let _ = GlobalUnlock(handle);
    }

    // SAFETY: 剪贴板已打开；成功后所有权移交系统，因此这里不再 GlobalFree。
    match unsafe { SetClipboardData(format, Some(HANDLE(handle.0))) } {
        Ok(_) => Ok(()),
        Err(err) => {
            // SAFETY: SetClipboardData 失败表示系统没有接管内存，需要自行释放。
            unsafe {
                let _ = GlobalFree(Some(handle));
            }
            Err(AppError::ClipboardFailed(format!(
                "SetClipboardData({format}): {err}"
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dib_header_is_a_bare_bitmapinfoheader() {
        let dib = dib_bytes(&[0u8; 4 * 4], 2, 2);
        assert_eq!(dib.len(), 40 + 2 * 2 * 4);
        assert_eq!(u32::from_le_bytes(dib[0..4].try_into().unwrap()), 40);
        assert_eq!(i32::from_le_bytes(dib[4..8].try_into().unwrap()), 2);
        assert_eq!(i32::from_le_bytes(dib[8..12].try_into().unwrap()), 2);
        assert_eq!(u16::from_le_bytes(dib[12..14].try_into().unwrap()), 1);
        assert_eq!(u16::from_le_bytes(dib[14..16].try_into().unwrap()), 32);
        assert_eq!(u32::from_le_bytes(dib[16..20].try_into().unwrap()), 0);
        assert_eq!(u32::from_le_bytes(dib[20..24].try_into().unwrap()), 16);
    }

    #[test]
    fn dib_rows_start_from_bottom_left() {
        // 2×2：左上红、右上绿、左下蓝、右下白。
        let rgba = [
            255, 0, 0, 255, // (0,0) 红
            0, 255, 0, 255, // (1,0) 绿
            0, 0, 255, 255, // (0,1) 蓝
            255, 255, 255, 255, // (1,1) 白
        ];
        let dib = dib_bytes(&rgba, 2, 2);
        // 第 41 字节起是第一个像素 = 左下角的蓝，BGRA 顺序。
        assert_eq!(&dib[40..44], &[255, 0, 0, 255]);
        // 第二个像素 = 右下角的白。
        assert_eq!(&dib[44..48], &[255, 255, 255, 255]);
        // 第三行开始 = 左上角的红：B=0,G=0,R=255,A=255。
        assert_eq!(&dib[48..52], &[0, 0, 255, 255]);
    }

    #[test]
    fn utf16_crlf_normalizes_line_endings() {
        let units = utf16_crlf("a\nb");
        assert_eq!(units, vec![97, 13, 10, 98, 0]);
        assert_eq!(units.len(), 5);

        // 已经是 CRLF 的输入不会变成 CRCRLF。
        let already = utf16_crlf("a\r\nb");
        assert_eq!(already, vec![97, 13, 10, 98, 0]);
    }

    #[test]
    fn utf16_crlf_keeps_surrogate_pairs() {
        let units = utf16_crlf("😀");
        assert_eq!(units.len(), 3); // 两个代理 + NUL
        assert_eq!(units[2], 0);
    }

    /// 真实剪贴板写入：全局单例，CI 上会互相干扰，因此默认忽略，手工验收执行。
    #[test]
    #[ignore = "writes to the system clipboard; run manually"]
    fn writes_png_and_text_to_the_system_clipboard() {
        let image = image::RgbaImage::from_pixel(4, 4, image::Rgba([10, 20, 30, 255]));
        let mut png = Vec::new();
        image
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        copy_image_png(&png).unwrap();
        copy_text("hello\nworld").unwrap();
    }
}
