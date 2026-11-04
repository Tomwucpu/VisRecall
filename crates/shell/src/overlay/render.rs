//! Direct2D 渲染：把一帧测试画面画到交换链后备缓冲上（design §7.2）。
//!
//! 每帧流程：`back buffer -> IDXGISurface -> CreateBitmapFromDxgiSurface(TARGET)`
//! → `SetTarget` → `BeginDraw` → 背景/文字/移动矩形 → `EndDraw`；
//! [`Renderer::present`] 负责解除 target 并 `Present(1, 0)`。
//!
//! D2D 上下文固定 `SetDpi(96, 96)`，因此 1 DIP = 1 物理像素（design §6）。
//!
//! 线程约束：[`Renderer`] 持有交换链与 `ID2D1DeviceContext`，后者出自单线程 D2D
//! 工厂，具有线程亲和性，因此 [`Renderer`] 显式标记为 `!Send`（见 [`mod@super::d3d`]
//! 的模块文档）。windows-rs 的 COM 接口封装本身实现了 `Send`，不能靠接口类型兜底。

use std::marker::PhantomData;
use std::mem::ManuallyDrop;

use visrecall_domain::AppError;
use windows::core::Interface;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_IGNORE, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_RECT_F,
};
use windows::Win32::Graphics::Direct2D::{
    ID2D1Bitmap1, ID2D1Brush, ID2D1DeviceContext, ID2D1Image, ID2D1RenderTarget,
    ID2D1SolidColorBrush, D2D1_BITMAP_OPTIONS_CANNOT_DRAW, D2D1_BITMAP_OPTIONS_NONE,
    D2D1_BITMAP_OPTIONS_TARGET, D2D1_BITMAP_PROPERTIES1, D2D1_DEVICE_CONTEXT_OPTIONS_NONE,
    D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
    D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
};
use windows::Win32::Graphics::DirectWrite::{
    IDWriteTextFormat, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT_BOLD, DWRITE_MEASURING_MODE_NATURAL,
};
use windows::Win32::Graphics::Dxgi::IDXGISurface;

use super::d3d::{D3dContext, SwapChain, BACK_BUFFER_FORMAT};
use super::frame::FrozenTexture;

/// 文字使用的字体：中文 Windows 自带，DirectWrite 对缺字仍会回退。
const FONT_FAMILY: &str = "Microsoft YaHei UI";
const LOCALE: &str = "zh-CN";
/// 96 DPI 下 1 DIP = 1 物理像素，字号直接用物理像素。
const FONT_SIZE_PX: f32 = 52.0;

const BACKGROUND: D2D1_COLOR_F = D2D1_COLOR_F {
    r: 0.055,
    g: 0.063,
    b: 0.094,
    a: 1.0,
};
const TEXT_COLOR: D2D1_COLOR_F = D2D1_COLOR_F {
    r: 1.0,
    g: 1.0,
    b: 1.0,
    a: 1.0,
};
const ACCENT_COLOR: D2D1_COLOR_F = D2D1_COLOR_F {
    r: 0.247,
    g: 0.647,
    b: 0.988,
    a: 1.0,
};
/// 冻结帧铺满前先清成不透明黑，避免未初始化的后备缓冲内容透出。
const BLACK: D2D1_COLOR_F = D2D1_COLOR_F {
    r: 0.0,
    g: 0.0,
    b: 0.0,
    a: 1.0,
};

/// 一个窗口的 D2D 渲染器：持有交换链、D2D 上下文与复用资源。
///
/// `!Send`：D2D 单线程工厂创建的设备上下文只能在覆盖层线程使用。
pub struct Renderer {
    swap: SwapChain,
    context: ID2D1DeviceContext,
    target: ID2D1RenderTarget,
    text_format: IDWriteTextFormat,
    text_brush: ID2D1Brush,
    accent_brush: ID2D1Brush,
    frame_index: u64,
    /// 显式禁止 `Send`（交换链已 `!Send`，这里再写一次以固定不变量）。
    _not_send: PhantomData<*const ()>,
}

impl Renderer {
    /// 基于已有的交换链创建渲染器；全部资源在调用线程创建并只在该线程使用。
    pub fn new(device: &D3dContext, swap: SwapChain) -> Result<Renderer, AppError> {
        // `cast` 只做 QueryInterface；`device` 在本线程存活。
        let dxgi_device: windows::Win32::Graphics::Dxgi::IDXGIDevice = device
            .device()
            .cast()
            .map_err(|err| failed("ID3D11Device -> IDXGIDevice", err))?;
        // SAFETY: 工厂与 dxgi 设备均在作用域内有效。
        let d2d_device = unsafe { device.d2d_factory().CreateDevice(&dxgi_device) }
            .map_err(|err| failed("ID2D1Factory1::CreateDevice", err))?;
        // SAFETY: 设备上下文选项为默认值。
        let context = unsafe { d2d_device.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE) }
            .map_err(|err| failed("ID2D1Device::CreateDeviceContext", err))?;
        // `cast` 只做 QueryInterface：`ID2D1DeviceContext` 继承 `ID2D1RenderTarget`。
        let target: ID2D1RenderTarget = context
            .cast()
            .map_err(|err| failed("ID2D1DeviceContext -> ID2D1RenderTarget", err))?;

        // 1 DIP = 1 物理像素（design §6）。
        // SAFETY: 纯状态设置，无指针参数。
        unsafe {
            target.SetDpi(96.0, 96.0);
            target.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
        }

        let font_family = wide(FONT_FAMILY);
        let locale = wide(LOCALE);
        // SAFETY: 两个字符串都以 NUL 结尾且在调用期间存活。
        let text_format = unsafe {
            device.dwrite_factory().CreateTextFormat(
                windows::core::PCWSTR(font_family.as_ptr()),
                None,
                DWRITE_FONT_WEIGHT_BOLD,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                FONT_SIZE_PX,
                windows::core::PCWSTR(locale.as_ptr()),
            )
        }
        .map_err(|err| failed("IDWriteFactory::CreateTextFormat", err))?;

        let text_brush: ID2D1Brush = create_brush(&target, &TEXT_COLOR)?;
        let accent_brush: ID2D1Brush = create_brush(&target, &ACCENT_COLOR)?;

        Ok(Renderer {
            swap,
            context,
            target,
            text_format,
            text_brush,
            accent_brush,
            frame_index: 0,
            _not_send: PhantomData,
        })
    }

    /// 绘制一帧测试画面：深色背景、白色中文标签、一个每帧移动的矩形与边框。
    pub fn draw_test_frame(&mut self, label: &str) -> Result<(), AppError> {
        // 后备缓冲 bitmap 必须活到 `EndDraw`；`SetTarget` 只借用 image 的包装。
        let _back_buffer = self.bind_back_buffer()?;

        let width = self.swap.width() as f32;
        let height = self.swap.height() as f32;

        // SAFETY: 全部绘制调用都发生在 BeginDraw/EndDraw 之间，指针指向栈上结构。
        unsafe {
            self.target.BeginDraw();
            self.target.Clear(Some(&BACKGROUND));

            let border = D2D_RECT_F {
                left: 0.5,
                top: 0.5,
                right: width - 0.5,
                bottom: height - 0.5,
            };
            self.target
                .DrawRectangle(&border, &self.accent_brush, 2.0, None);

            // 每帧移动的矩形：验证画面确实在刷新而不是静止。
            let rect_size = (height * 0.22).max(64.0);
            let travel = (width - rect_size - 80.0).max(1.0);
            let x = 40.0 + (self.frame_index as f32 * 14.0) % travel;
            let moving = D2D_RECT_F {
                left: x,
                top: height - rect_size - 40.0,
                right: x + rect_size,
                bottom: height - 40.0,
            };
            self.target.FillRectangle(&moving, &self.accent_brush);

            let label_utf16: Vec<u16> = label.encode_utf16().collect();
            let text_rect = D2D_RECT_F {
                left: 64.0,
                top: 64.0,
                right: width - 64.0,
                bottom: height - 64.0,
            };
            self.target.DrawText(
                &label_utf16,
                &self.text_format,
                &text_rect,
                &self.text_brush,
                D2D1_DRAW_TEXT_OPTIONS_CLIP,
                DWRITE_MEASURING_MODE_NATURAL,
            );

            self.target
                .EndDraw(None, None)
                .map_err(|err| failed("ID2D1RenderTarget::EndDraw", err))?;
        }

        self.frame_index += 1;
        // bitmap 在函数结束时释放；下一帧重新从轮换后的后备缓冲创建。
        Ok(())
    }

    /// 把冻结纹理按 1:1 物理像素铺满整个后缓冲（design §7.2）。
    ///
    /// 冻结纹理直接包装成 D2D bitmap（`D2D1_BITMAP_OPTIONS_NONE`，BGRA +
    /// `ALPHA_MODE_IGNORE`，96 DPI）；目标矩形等于纹理尺寸，最近邻插值，因此既不缩放
    /// 也不重采样。调用前必须保证纹理尺寸与交换链一致（`show` 负责重建窗口）。
    pub fn draw_frozen(&mut self, frozen: &FrozenTexture) -> Result<(), AppError> {
        let (target_width, target_height) = self.size();
        if (frozen.width, frozen.height) != (target_width, target_height) {
            return Err(AppError::Internal(format!(
                "frozen texture {}x{} does not match swap chain {}x{}",
                frozen.width, frozen.height, target_width, target_height
            )));
        }

        // `cast` 只做 QueryInterface：DXGI surface 是 D3D11 纹理的视图。
        let surface: IDXGISurface = frozen
            .texture
            .cast()
            .map_err(|err| failed("ID3D11Texture2D -> IDXGISurface", err))?;
        let props = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: BACK_BUFFER_FORMAT,
                alphaMode: D2D1_ALPHA_MODE_IGNORE,
            },
            dpiX: 96.0,
            dpiY: 96.0,
            bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
            colorContext: ManuallyDrop::new(None),
        };
        // SAFETY: surface 由 `frozen` 持有、在本函数内存活；属性是栈上有效指针。
        let frozen_bitmap: ID2D1Bitmap1 = unsafe {
            self.context
                .CreateBitmapFromDxgiSurface(&surface, Some(&props))
        }
        .map_err(|err| failed("CreateBitmapFromDxgiSurface(frozen)", err))?;

        // 后备缓冲 bitmap 必须活到 `EndDraw`。
        let _back_buffer = self.bind_back_buffer()?;
        let dest = D2D_RECT_F {
            left: 0.0,
            top: 0.0,
            right: frozen.width as f32,
            bottom: frozen.height as f32,
        };

        // SAFETY: 全部绘制调用都发生在 BeginDraw/EndDraw 之间；`dest` 是栈上结构，
        // `frozen_bitmap` 在本函数内存活。
        unsafe {
            self.target.BeginDraw();
            self.target.Clear(Some(&BLACK));
            // `ID2D1DeviceContext::DrawBitmap` 的 windows-rs 绑定返回 `()`；
            // 失败通过随后的 `EndDraw` 上报（D2D 在下次绘制时返回 HRESULT）。
            self.context.DrawBitmap(
                &frozen_bitmap,
                Some(&dest),
                1.0,
                D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
                None,
                None,
            );
            self.target
                .EndDraw(None, None)
                .map_err(|err| failed("ID2D1RenderTarget::EndDraw", err))?;
        }

        // 与 `draw_test_frame` 的统计语义一致：成功绘制一帧就递增（Task 13 的输入层
        // 也会依赖这个计数做「本次显示是否已呈现过」的判断）。
        self.frame_index += 1;
        Ok(())
    }

    /// 交换链（窗口客户区）尺寸，物理像素。
    pub fn size(&self) -> (u32, u32) {
        (self.swap.width(), self.swap.height())
    }

    /// 把当前后备缓冲包装成 D2D target bitmap 并 `SetTarget`。
    ///
    /// FLIP 模型下每次 `Present` 后缓冲区轮换，因此每帧都要重新绑定；返回的 bitmap
    /// 必须活到本帧 `EndDraw` 之后（`SetTarget` 只持有其包装）。
    fn bind_back_buffer(&self) -> Result<ID2D1Bitmap1, AppError> {
        let surface = self.swap.back_buffer()?;
        let props = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: BACK_BUFFER_FORMAT,
                alphaMode: D2D1_ALPHA_MODE_IGNORE,
            },
            dpiX: 96.0,
            dpiY: 96.0,
            bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
            colorContext: ManuallyDrop::new(None),
        };
        // SAFETY: surface 在本函数返回前被 bitmap 引用计数持有；属性是栈上有效指针。
        let bitmap: ID2D1Bitmap1 = unsafe {
            self.context
                .CreateBitmapFromDxgiSurface(&surface, Some(&props))
        }
        .map_err(|err| failed("CreateBitmapFromDxgiSurface", err))?;
        // `cast` 只做 QueryInterface（`ID2D1Bitmap1` 继承 `ID2D1Image`）。
        let image: ID2D1Image = bitmap
            .cast()
            .map_err(|err| failed("ID2D1Bitmap1 -> ID2D1Image", err))?;
        // SAFETY: image 是后备缓冲的包装，调用方保证 bitmap 活到 `EndDraw` 之后。
        unsafe { self.context.SetTarget(&image) };
        Ok(bitmap)
    }

    /// 解除 D2D target 并 `Present(1, 0)`（vsync）。
    pub fn present(&mut self) -> Result<(), AppError> {
        // SAFETY: 传空 target 只释放对后备缓冲的引用，无额外指针参数。
        unsafe { self.context.SetTarget(None::<&ID2D1Image>) };
        self.swap.present()
    }

    /// 已绘制的帧数。
    pub fn frame_index(&self) -> u64 {
        self.frame_index
    }
}

/// 以给定颜色创建实心画刷，并立即转换成基类 [`ID2D1Brush`]（避免每帧 QueryInterface）。
fn create_brush(target: &ID2D1RenderTarget, color: &D2D1_COLOR_F) -> Result<ID2D1Brush, AppError> {
    // SAFETY: 颜色是栈上有效指针，无画刷属性。
    let brush: ID2D1SolidColorBrush = unsafe { target.CreateSolidColorBrush(color, None) }
        .map_err(|err| failed("CreateSolidColorBrush", err))?;
    // `cast` 只做 QueryInterface。
    brush
        .cast()
        .map_err(|err| failed("ID2D1SolidColorBrush -> ID2D1Brush", err))
}

/// UTF-16（NUL 结尾）缓冲区，供 Win32/DWrite 直接取指针。
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn failed(op: &str, err: impl std::fmt::Display) -> AppError {
    AppError::Internal(format!("{op} failed: {err}"))
}
