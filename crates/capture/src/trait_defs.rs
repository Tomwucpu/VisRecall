//! 捕获层 trait 定义与调试辅助。
//!
//! [`FrameGrabber`] 的 Win32 实现位于 [`crate::grab_gdi`]；数据形状 [`RawFrame`] 来自
//! domain（`visrecall_domain::capture`），本模块只重新导出，不重复定义。

use std::io::Cursor;

use image::codecs::png::PngEncoder;
use image::{ExtendedColorType, ImageEncoder};
use visrecall_domain::capture::DisplayInfo;
use visrecall_domain::error::AppError;

pub use visrecall_domain::capture::RawFrame;

/// 抓取给定显示器的一帧，逐个显示器返回 top-down BGRA8 位图。
///
/// 调用方按返回顺序上传纹理（Task 11），不要求实现并发抓取。
pub trait FrameGrabber: Send + Sync {
    fn grab(&self, displays: &[DisplayInfo]) -> Result<Vec<RawFrame>, AppError>;
}

/// [`RawFrame`] 的调试/测试辅助。
///
/// `RawFrame` 定义在 domain，无法在本 crate 添加 inherent impl，因此用扩展 trait；
/// 调用方需先 `use visrecall_capture::trait_defs::RawFrameExt;`。
pub trait RawFrameExt {
    /// 把 top-down BGRA8 转成 RGBA 并编码为 PNG，仅用于调试导出与人工验证。
    ///
    /// `bgra` 少于 `stride * height` 时返回 [`AppError::Internal`]，不越界读取。
    fn to_png(&self) -> Result<Vec<u8>, AppError>;
}

impl RawFrameExt for RawFrame {
    fn to_png(&self) -> Result<Vec<u8>, AppError> {
        let width = self.width as usize;
        let height = self.height as usize;
        if width == 0 || height == 0 {
            return Err(AppError::Internal("frame has zero width or height".into()));
        }

        // 每行至少 width*4 字节；stride 更大时（对齐填充）尾部补位不参与拷贝。
        let row_bytes = width
            .checked_mul(4)
            .ok_or_else(|| AppError::Internal("frame width overflows row size".into()))?;
        if self.stride < row_bytes {
            return Err(AppError::Internal(format!(
                "frame stride {} is smaller than width * 4 ({row_bytes})",
                self.stride
            )));
        }
        let needed = self
            .stride
            .checked_mul(height)
            .ok_or_else(|| AppError::Internal("frame stride * height overflows".into()))?;
        if self.bgra.len() < needed {
            return Err(AppError::Internal(format!(
                "frame buffer too small: {} bytes < stride * height ({needed})",
                self.bgra.len()
            )));
        }

        let mut rgba = Vec::with_capacity(row_bytes * height);
        for row in self.bgra[..needed].chunks_exact(self.stride) {
            for px in row[..row_bytes].chunks_exact(4) {
                // BGRA -> RGBA：交换 B/R，保留 alpha。
                rgba.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
            }
        }

        let mut png = Vec::new();
        PngEncoder::new(Cursor::new(&mut png))
            .write_image(&rgba, self.width, self.height, ExtendedColorType::Rgba8)
            .map_err(|err| AppError::Internal(format!("png encode failed: {err}")))?;
        Ok(png)
    }
}
