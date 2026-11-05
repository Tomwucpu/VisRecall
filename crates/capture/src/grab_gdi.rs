//! GDI 抓帧：逐显示器 `BitBlt` 到 32bpp top-down DIB，再拷贝为 [`RawFrame`]。
//!
//! 坐标全部为虚拟屏幕物理像素（Per-Monitor V2，design §6）：`GetDC(NULL)` 返回的屏幕
//! DC 以主显示器左上角为原点，副屏可为负坐标，因此 `BitBlt` 的源坐标直接用
//! [`DisplayInfo::bounds`] 的 `x`/`y`。
//!
//! 所有 GDI 资源（屏幕 DC、内存 DC、DIB、选入的原对象）由 RAII guard 无条件释放，
//! 任一步失败都不会泄漏句柄。

use std::ffi::c_void;
use std::mem::size_of;
use std::ptr::null_mut;

use tracing::{debug, warn};
use visrecall_domain::capture::{DisplayInfo, RawFrame};
use visrecall_domain::error::AppError;
use windows::core::Error as WinError;
use windows::Win32::Graphics::Dwm::DwmFlush;
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC,
    SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CAPTUREBLT, DIB_RGB_COLORS, HBITMAP, HDC,
    HGDIOBJ, SRCCOPY,
};

use crate::trait_defs::FrameGrabber;

/// 全黑帧抽样步长：每 64 个像素检查一次 BGRA，控制大屏上的判断开销。
const BLANK_SAMPLE_STEP: usize = 64;

/// 基于 GDI `BitBlt` 的抓帧器；无状态，可跨线程共享。
#[derive(Clone, Copy, Debug, Default)]
pub struct GdiFrameGrabber;

impl GdiFrameGrabber {
    pub fn new() -> Self {
        Self
    }
}

impl FrameGrabber for GdiFrameGrabber {
    fn grab(&self, displays: &[DisplayInfo]) -> Result<Vec<RawFrame>, AppError> {
        if displays.is_empty() {
            return Err(AppError::CaptureFailed("no displays to grab".into()));
        }

        let screen = ScreenDc::acquire()?;
        // 顺序抓取；每台抓完立即释放其 DIB，只把 CPU 位图留在返回的 Vec 里。
        displays
            .iter()
            .map(|display| capture_display(screen.0, display))
            .collect()
    }
}

/// 抓取单台显示器。`screen` 为 `GetDC(NULL)` 得到的虚拟屏幕 DC。
fn capture_display(screen: HDC, display: &DisplayInfo) -> Result<RawFrame, AppError> {
    let bounds = display.bounds;
    if bounds.is_empty() {
        return Err(AppError::CaptureFailed(format!(
            "display {} has empty bounds",
            display.id
        )));
    }

    let surface = MemorySurface::create(screen, bounds.width, bounds.height)?;

    // SAFETY: `surface` 持有有效的内存 DC 与已选入的 DIB；`screen` 在抓帧期间有效；
    // 目标区域恰好覆盖整个 DIB（top-down，宽高相同），源坐标来自虚拟屏幕物理像素。
    unsafe {
        BitBlt(
            surface.dc,
            0,
            0,
            surface.width,
            surface.height,
            Some(screen),
            bounds.x,
            bounds.y,
            SRCCOPY | CAPTUREBLT,
        )
    }
    .map_err(|err| AppError::CaptureFailed(format!("BitBlt for {} failed: {err}", display.id)))?;

    let frame = RawFrame {
        display_id: display.id.clone(),
        width: bounds.width,
        height: bounds.height,
        stride: surface.stride,
        bgra: surface.copy_pixels(),
    };

    // 全黑通常是 DRM 保护窗口或抓帧失败；策略（重试 / DrmProtected）由调用方决定。
    if is_blank_frame(&frame) {
        warn!(display_id = %frame.display_id, "captured frame is all black; suspected DRM or capture failure");
    }
    Ok(frame)
}

/// 抽样判断帧是否全黑（疑似 DRM 或抓帧失败）。
///
/// 每 [`BLANK_SAMPLE_STEP`] 个像素比较一次 BGRA；空 buffer 视为全黑（`true`）。
/// 抽样可能漏掉小面积内容，这里只作为启发式提示，不作为唯一判定依据。
pub fn is_blank_frame(frame: &RawFrame) -> bool {
    if frame.bgra.len() < 4 {
        return true;
    }
    frame
        .bgra
        .chunks_exact(4)
        .step_by(BLANK_SAMPLE_STEP)
        .all(|px| px.iter().all(|&b| b == 0))
}

/// 等待 DWM 合成一帧，确保刚隐藏的自有窗口不再出现在抓帧结果里（design §5）。
///
/// 失败（例如 DWM 未运行）不阻止捕获：调用方记录日志后继续抓帧，
/// 此时旧窗口内容可能仍在合成队列里。
pub fn dwm_flush() -> Result<(), AppError> {
    // SAFETY: 无参数、无生命周期要求的同步调用；失败时返回 Win32 错误。
    unsafe { DwmFlush() }.map_err(|err| AppError::Internal(format!("DwmFlush failed: {err}")))
}

/// `GetDC(NULL)` 的 RAII 包装，`Drop` 时 `ReleaseDC`。
struct ScreenDc(HDC);

impl ScreenDc {
    fn acquire() -> Result<Self, AppError> {
        // SAFETY: `None` 请求整个虚拟屏幕的 DC；失败时返回空句柄而不是 panic。
        let hdc = unsafe { GetDC(None) };
        if hdc.is_invalid() {
            return Err(AppError::CaptureFailed(format!(
                "GetDC(NULL) failed: {}",
                WinError::from_thread()
            )));
        }
        Ok(Self(hdc))
    }
}

impl Drop for ScreenDc {
    fn drop(&mut self) {
        // SAFETY: 句柄由 `acquire` 创建且只在此处释放；失败无副作用。
        unsafe {
            let _ = ReleaseDC(None, self.0);
        }
    }
}

/// 内存 DC + 32bpp top-down DIB 的组合，`Drop` 时按选入顺序反向清理。
///
/// 字段先以 `Default`（无效句柄）构造，逐步填充；任一步失败直接返回 `Err`，
/// 由 `Drop` 释放已经创建的部分。
struct MemorySurface {
    dc: HDC,
    bitmap: HBITMAP,
    /// `CreateDIBSection` 前 DC 自带的 1×1 位图，必须选回后 DC 才能删除。
    previous: HGDIOBJ,
    bits: *mut c_void,
    width: i32,
    height: i32,
    stride: usize,
    bytes: usize,
}

impl MemorySurface {
    fn create(screen: HDC, width: u32, height: u32) -> Result<Self, AppError> {
        let width_i32 = i32::try_from(width)
            .map_err(|_| AppError::CaptureFailed(format!("display width {width} too large")))?;
        let height_i32 = i32::try_from(height)
            .map_err(|_| AppError::CaptureFailed(format!("display height {height} too large")))?;
        let stride = (width as usize)
            .checked_mul(4)
            .ok_or_else(|| AppError::CaptureFailed(format!("stride overflow for width {width}")))?;
        let bytes = stride.checked_mul(height as usize).ok_or_else(|| {
            AppError::CaptureFailed(format!("frame buffer size overflow for {width}x{height}"))
        })?;

        // SAFETY: `screen` 是有效的屏幕 DC；失败返回空句柄。
        let dc = unsafe { CreateCompatibleDC(Some(screen)) };
        let mut surface = Self {
            dc,
            bitmap: HBITMAP::default(),
            previous: HGDIOBJ::default(),
            bits: null_mut(),
            width: width_i32,
            height: height_i32,
            stride,
            bytes,
        };
        if surface.dc.is_invalid() {
            return Err(AppError::CaptureFailed(format!(
                "CreateCompatibleDC failed: {}",
                WinError::from_thread()
            )));
        }

        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width_i32,
                // 负高度 = top-down DIB，与 `RawFrame` 的行序一致。
                biHeight: -height_i32,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                biSizeImage: u32::try_from(bytes).unwrap_or(0),
                biXPelsPerMeter: 0,
                biYPelsPerMeter: 0,
                biClrUsed: 0,
                biClrImportant: 0,
            },
            ..Default::default()
        };

        let mut bits: *mut c_void = null_mut();
        // SAFETY: `bmi` 在上一步初始化完毕且存活到调用结束；`bits` 指向栈上有效指针；
        // 传入 `screen` 保证 DIB 格式与屏幕兼容。
        let bitmap =
            unsafe { CreateDIBSection(Some(screen), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) }
                .map_err(|err| {
                    AppError::CaptureFailed(format!("CreateDIBSection failed: {err}"))
                })?;
        // 先记录再检查：即使 bits 为空，Drop 也负责删除 bitmap。
        surface.bitmap = bitmap;
        surface.bits = bits;
        if surface.bits.is_null() {
            return Err(AppError::CaptureFailed(
                "CreateDIBSection returned null pixel pointer".into(),
            ));
        }

        // SAFETY: 两个句柄都有效；返回原对象用于 `Drop` 时恢复。
        let previous = unsafe { SelectObject(surface.dc, HGDIOBJ(surface.bitmap.0)) };
        if previous.is_invalid() {
            return Err(AppError::CaptureFailed(format!(
                "SelectObject failed: {}",
                WinError::from_thread()
            )));
        }
        surface.previous = previous;

        debug!(
            width = surface.width,
            height = surface.height,
            stride = surface.stride,
            "gdi surface created"
        );
        Ok(surface)
    }

    /// 把 DIB 像素复制成 `Vec<u8>`（长度 `stride * height`）。
    fn copy_pixels(&self) -> Vec<u8> {
        // SAFETY: `bits` 非空、`bytes` 与 DIB 布局一致，DIB 在复制完成前不会被释放。
        unsafe { std::slice::from_raw_parts(self.bits.cast::<u8>(), self.bytes).to_vec() }
    }
}

impl Drop for MemorySurface {
    fn drop(&mut self) {
        // SAFETY: 每一步都对句柄做有效性检查；释放顺序为「恢复原对象 → 删除位图 →
        // 删除内存 DC」，保证位图未被选入时再删除。
        unsafe {
            if !self.previous.is_invalid() {
                SelectObject(self.dc, self.previous);
            }
            if !self.bitmap.is_invalid() {
                let _ = DeleteObject(HGDIOBJ(self.bitmap.0));
            }
            if !self.dc.is_invalid() {
                let _ = DeleteDC(self.dc);
            }
        }
    }
}
