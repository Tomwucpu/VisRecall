//! 冻结帧：校验 CPU 位图的行距/尺寸，并上传到 D3D11 纹理（design §7.2）。
//!
//! 抓帧结果 [`RawFrame`] 是 top-down BGRA8 的 CPU `Vec<u8>`（`Send`），覆盖层线程
//! 用 [`upload_frame`] 把它上传成 `D3D11_USAGE_DEFAULT` + `B8G8R8A8_UNORM` +
//! `BIND_SHADER_RESOURCE` 的纹理。上传由 `UpdateSubresource` 同步完成，返回后调用方
//! 立即丢弃 CPU 位图（`show` 逐帧消费 `Vec<RawFrame>`），大屏上不会多留一份内存。
//!
//! 线程约束：[`FrozenTexture`] 持有 D3D11 纹理，只能在覆盖层线程创建/使用；windows-rs
//! 的 COM 接口封装本身实现了 `Send`，这里用 `PhantomData<*const ()>` 显式标成 `!Send`。

use std::ffi::c_void;
use std::marker::PhantomData;

use visrecall_domain::capture::RawFrame;
use visrecall_domain::AppError;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Texture2D, D3D11_BIND_SHADER_RESOURCE, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};

use super::d3d::D3dContext;

/// BGRA8 的每像素字节数。
const BYTES_PER_PIXEL: usize = 4;

/// 一张已上传到 GPU 的冻结帧；`width`/`height` 是纹理的物理像素尺寸。
///
/// `!Send`：纹理只能在创建它的覆盖层线程使用（见模块文档）。
pub struct FrozenTexture {
    pub texture: ID3D11Texture2D,
    pub width: u32,
    pub height: u32,
    /// 显式禁止 `Send`（windows-rs 的 COM 接口封装本身实现了 `Send`）。
    _not_send: PhantomData<*const ()>,
}

/// 校验帧尺寸与行距：宽高必须大于 0，`stride >= width * 4` 且 `stride` 按 4 字节对齐。
///
/// `stride` 允许大于 `width * 4`（DIB 行对齐填充）；填充字节不参与渲染。
/// 失败一律返回 [`AppError::InvalidInput`]，由覆盖层在 `show` 里降级为取消。
pub fn validate_frame_shape(width: u32, height: u32, stride: usize) -> Result<(), AppError> {
    if width == 0 || height == 0 {
        return Err(AppError::InvalidInput(format!(
            "frame size must be non-zero, got {width}x{height}"
        )));
    }
    let min_stride = (width as usize)
        .checked_mul(BYTES_PER_PIXEL)
        .ok_or_else(|| AppError::InvalidInput(format!("frame width {width} overflows row size")))?;
    if stride < min_stride {
        return Err(AppError::InvalidInput(format!(
            "frame stride {stride} is smaller than width * 4 ({min_stride})"
        )));
    }
    if !stride.is_multiple_of(BYTES_PER_PIXEL) {
        return Err(AppError::InvalidInput(format!(
            "frame stride {stride} is not a multiple of {BYTES_PER_PIXEL}"
        )));
    }
    Ok(())
}

/// 把一帧 top-down BGRA8 上传为 D3D11 纹理。
///
/// 先校验 `bgra.len() >= stride * height` 与尺寸，创建 `D3D11_USAGE_DEFAULT` 纹理后
/// 用 `UpdateSubresource` 上传；调用方随后即可丢弃 [`RawFrame::bgra`]。
pub fn upload_frame(device: &D3dContext, frame: &RawFrame) -> Result<FrozenTexture, AppError> {
    validate_frame_shape(frame.width, frame.height, frame.stride)?;

    let row_pitch = u32::try_from(frame.stride).map_err(|_| {
        AppError::InvalidInput(format!("frame stride {} exceeds u32", frame.stride))
    })?;
    let needed = frame
        .stride
        .checked_mul(frame.height as usize)
        .ok_or_else(|| {
            AppError::InvalidInput(format!(
                "frame buffer size overflows for {}x{}",
                frame.width, frame.height
            ))
        })?;
    if frame.bgra.len() < needed {
        return Err(AppError::InvalidInput(format!(
            "frame buffer too small: {} bytes < stride * height ({needed})",
            frame.bgra.len()
        )));
    }

    let desc = D3D11_TEXTURE2D_DESC {
        Width: frame.width,
        Height: frame.height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };

    let mut texture: Option<ID3D11Texture2D> = None;
    // SAFETY: `desc` 是栈上有效结构；输出指针指向本函数内的 `Option`；初始数据留空，
    // 内容随后由 `UpdateSubresource` 上传。
    unsafe {
        device
            .device()
            .CreateTexture2D(&desc, None, Some(&mut texture))
    }
    .map_err(|err| failed("ID3D11Device::CreateTexture2D", err))?;
    let texture =
        texture.ok_or_else(|| AppError::Internal("CreateTexture2D returned no texture".into()))?;

    // SAFETY: 纹理尺寸等于 `desc`；`pSrcData` 直接指向 `frame.bgra`（top-down BGRA8，
    // 长度已校验 >= `row_pitch * height`），**不是** `D3D11_SUBRESOURCE_DATA` 结构本身
    // （`UpdateSubresource` 的第四个参数是原始像素起点）；调用同步完成，返回后 CPU
    // 缓冲可安全释放。
    unsafe {
        device.immediate_context().UpdateSubresource(
            &texture,
            0,
            None,
            frame.bgra.as_ptr().cast::<c_void>(),
            row_pitch,
            0,
        );
    }

    Ok(FrozenTexture {
        texture,
        width: frame.width,
        height: frame.height,
        _not_send: PhantomData,
    })
}

fn failed(op: &str, err: impl std::fmt::Display) -> AppError {
    AppError::Internal(format!("{op} failed: {err}"))
}
