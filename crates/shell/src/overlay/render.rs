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

use visrecall_domain::geometry::{PhysicalPoint, PhysicalRect};
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
    DWRITE_FONT_WEIGHT_BOLD, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_MEASURING_MODE_NATURAL,
    DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_WORD_WRAPPING_NO_WRAP,
};
use windows::Win32::Graphics::Dxgi::IDXGISurface;

use super::d3d::{D3dContext, SwapChain, BACK_BUFFER_FORMAT};
use super::frame::FrozenTexture;
use crate::selection::{handle_centers, Handle};

/// 文字使用的字体：中文 Windows 自带，DirectWrite 对缺字仍会回退。
const FONT_FAMILY: &str = "Microsoft YaHei UI";
const LOCALE: &str = "zh-CN";
/// 96 DPI 下 1 DIP = 1 物理像素，字号直接用物理像素。
const FONT_SIZE_PX: f32 = 52.0;
/// 覆盖层交互信息（尺寸标签/窗口标签/角标/提示）的字号。
const OVERLAY_FONT_SIZE_PX: f32 = 18.0;

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
/// 选区外压暗：黑色约 50% alpha（design §7.2）。
const DIM_COLOR: D2D1_COLOR_F = D2D1_COLOR_F {
    r: 0.0,
    g: 0.0,
    b: 0.0,
    a: 0.5,
};
/// 选区边框：近白色实线。
const BORDER_COLOR: D2D1_COLOR_F = D2D1_COLOR_F {
    r: 1.0,
    g: 1.0,
    b: 1.0,
    a: 0.95,
};
/// 控制点描边。
const HANDLE_OUTLINE: D2D1_COLOR_F = D2D1_COLOR_F {
    r: 0.08,
    g: 0.08,
    b: 0.11,
    a: 0.9,
};
/// 悬停窗口高亮：与选区分色的青色。
const HOVER_COLOR: D2D1_COLOR_F = D2D1_COLOR_F {
    r: 0.24,
    g: 0.86,
    b: 0.72,
    a: 1.0,
};
/// 文字底板（尺寸标签、窗口标签）。
const LABEL_BG: D2D1_COLOR_F = D2D1_COLOR_F {
    r: 0.0,
    g: 0.0,
    b: 0.0,
    a: 0.62,
};
/// 「已冻结」角标底板。
const BADGE_BG: D2D1_COLOR_F = D2D1_COLOR_F {
    r: 0.0,
    g: 0.0,
    b: 0.0,
    a: 0.55,
};
/// 「选区过小」一类提示的底板。
const NOTICE_BG: D2D1_COLOR_F = D2D1_COLOR_F {
    r: 0.6,
    g: 0.16,
    b: 0.16,
    a: 0.92,
};

/// 一帧覆盖层场景；所有矩形都是**本窗口客户区**坐标（物理像素）。
///
/// 调用方（覆盖层线程）已经把全局坐标减去本窗口所在显示器的 `bounds.x/y`，
/// 与窗口完全不相交的元素置 `None`。渲染器只负责画，不做坐标换算与 DPI 缩放。
///
/// 跨屏策略（design §7.2）：边框按窗口裁剪后绘制；控制点、尺寸标签、悬停标签
/// 由调用方用 [`handles_for_window`] / [`point_owner`] 决定**归属某个窗口**，
/// 因此同一个控制点/标签在所有显示器上只出现一次。
#[derive(Clone, Debug, Default)]
pub struct Scene {
    /// 用户选区（本窗口本地坐标，**未**与窗口求交：越界部分交给 D2D 裁剪，
    /// 这样跨屏时不会在屏间缝隙画出多余的边框）；`None` 表示选区不可见。
    pub selection: Option<PhysicalRect>,
    /// 选区实时尺寸标签（`W×H`，物理像素）；只在归属窗口上有值。
    pub selection_label: Option<String>,
    /// 需要在本窗口绘制的控制点（中心为本窗口本地坐标）。
    pub handles: Vec<(Handle, PhysicalPoint)>,
    /// 悬停窗口高亮矩形。
    pub hover: Option<PhysicalRect>,
    /// 悬停窗口标签（标题/类名/进程/尺寸，调用方已截断）；只在归属窗口上有值。
    pub hover_label: Option<String>,
    /// 「选区过小」一类提示文字。
    pub notice: Option<String>,
    /// 「已冻结」角标不透明度；`<= 0` 不画。
    pub badge_alpha: f32,
    /// 本窗口所在显示器 DPI，控制点尺寸 = `8 × dpi / 96`。
    pub dpi: u32,
}

/// 八个控制点中归属 `window_bounds` 的那些，中心换算成窗口本地坐标。
///
/// `selection`、`window_bounds` 与 `displays`（所有显示器的 bounds）都是**全局**虚拟
/// 屏幕物理像素；返回的点可以直接画在本窗口客户区（客户区左上角 = 显示器 `bounds`
/// 左上角）。归属规则见 [`owned_center`]：
///
/// - 中心落在窗口内（半开区间）→ 归该窗口；
/// - 中心恰好落在**显示器临空边**（窗口的 `right`/`bottom`，且外侧 1px 处没有任何
///   其他显示器覆盖）→ 归该窗口（中心夹到该窗口最后一个像素）。选区被吸附到较矮
///   副屏下沿、较窄副屏右沿或虚拟屏外沿时，控制点中心正好等于 `right()`/`bottom()`，
///   半开区间会把它排除在外，不修就会出现「贴边一侧的控制点一个都不画」。
///
/// 内部屏缝（外侧还有别的显示器）仍走半开区间：分界线上的中心归右/下那个窗口，
/// 左侧/上方窗口不再重复画，因此每个控制点全局只被一个窗口绘制。空选区没有控制点。
pub fn handles_for_window(
    selection: PhysicalRect,
    window_bounds: PhysicalRect,
    displays: &[PhysicalRect],
) -> Vec<(Handle, PhysicalPoint)> {
    if selection.is_empty() {
        return Vec::new();
    }
    handle_centers(selection)
        .into_iter()
        .filter_map(|(handle, point)| {
            owned_center(point, window_bounds, displays).map(|local| (handle, local))
        })
        .collect()
}

/// 控制点中心归属判定：返回本窗口本地坐标，`None` 表示不归本窗口。
///
/// 1. 中心在窗口内（半开区间 `[x, right) × [y, bottom)`）→ 归本窗口；
/// 2. 否则只容忍中心恰好落在窗口（本身就是一台显示器）的 `right`/`bottom` 边界上：
///    该边界**外侧 1px** 处（右边界取 `(right, y)`，下边界取 `(x, bottom)`）没有任何
///    其他显示器覆盖时，这条边就是显示器的临空边，把该维夹到窗口最后一个像素
///    （`right - 1` / `bottom - 1`）后归属本窗口。角点的两维分别判定，只有临空的那一维
///    才夹取。
///
/// 内部屏缝外侧还有其他显示器覆盖，不满足第 2 条，仍只归包含该点的右/下窗口。夹取后
/// 重新用半开 `contains` 校验，因此远处（屏间空隙/其他显示器）窗口不会被拉进来，每个
/// 控制点全局仍只被一个窗口绘制。
fn owned_center(
    center: PhysicalPoint,
    window_bounds: PhysicalRect,
    displays: &[PhysicalRect],
) -> Option<PhysicalPoint> {
    if window_bounds.contains(center) {
        return Some(local_point(center, window_bounds));
    }
    // 只有窗口本身就是一台显示器时，窗口的右/下边才可能是显示器边界。
    if !displays.contains(&window_bounds) {
        return None;
    }
    let on_right = center.x == window_bounds.right();
    let on_bottom = center.y == window_bounds.bottom();
    if !on_right && !on_bottom {
        return None;
    }
    // 外侧 1px 处被其他显示器覆盖 → 内部屏缝，控制点仍归右/下那个窗口。
    let exposed_right = on_right
        && !covered_by_display(
            displays,
            PhysicalPoint {
                x: window_bounds.right(),
                y: center.y,
            },
        );
    let exposed_bottom = on_bottom
        && !covered_by_display(
            displays,
            PhysicalPoint {
                x: center.x,
                y: window_bounds.bottom(),
            },
        );
    if !exposed_right && !exposed_bottom {
        return None;
    }
    let clamped = PhysicalPoint {
        x: if exposed_right {
            window_bounds.right().saturating_sub(1)
        } else {
            center.x
        },
        y: if exposed_bottom {
            window_bounds.bottom().saturating_sub(1)
        } else {
            center.y
        },
    };
    window_bounds
        .contains(clamped)
        .then(|| local_point(clamped, window_bounds))
}

/// 半开区间判定：`point` 是否落在任意一台显示器内。
fn covered_by_display(displays: &[PhysicalRect], point: PhysicalPoint) -> bool {
    displays.iter().any(|bounds| bounds.contains(point))
}

/// 全局坐标 → 窗口本地坐标（窗口客户区左上角 = 显示器 `bounds` 左上角）。
fn local_point(point: PhysicalPoint, window_bounds: PhysicalRect) -> PhysicalPoint {
    PhysicalPoint {
        x: point.x.saturating_sub(window_bounds.x),
        y: point.y.saturating_sub(window_bounds.y),
    }
}

/// 矩形右下角的最后一个像素（半开区间：`right() - 1`, `bottom() - 1`）。
pub fn bottom_right(rect: PhysicalRect) -> PhysicalPoint {
    PhysicalPoint {
        x: rect.right().saturating_sub(1),
        y: rect.bottom().saturating_sub(1),
    }
}

/// 矩形中心的像素。
pub fn center(rect: PhysicalRect) -> PhysicalPoint {
    // 超长边（理论上只有异常输入）饱和到 `i32::MAX`，不做回绕。
    let half_w = i32::try_from(rect.width / 2).unwrap_or(i32::MAX);
    let half_h = i32::try_from(rect.height / 2).unwrap_or(i32::MAX);
    PhysicalPoint {
        x: rect.x.saturating_add(half_w),
        y: rect.y.saturating_add(half_h),
    }
}

/// `point` 落在哪个窗口边界内（按 `windows` 顺序返回第一个匹配项）。
///
/// 用来给「只画一次」的标签挑唯一归属窗口：标签的锚点依次尝试（如右下角 →
/// 中心），第一个命中的窗口就是归属窗口；都不命中时返回 `None`（不画标签）。
pub fn point_owner(point: PhysicalPoint, windows: &[PhysicalRect]) -> Option<usize> {
    windows.iter().position(|bounds| bounds.contains(point))
}

/// 一个窗口的 D2D 渲染器：持有交换链、D2D 上下文与复用资源。
///
/// `!Send`：D2D 单线程工厂创建的设备上下文只能在覆盖层线程使用。
pub struct Renderer {
    swap: SwapChain,
    context: ID2D1DeviceContext,
    target: ID2D1RenderTarget,
    text_format: IDWriteTextFormat,
    overlay_format: IDWriteTextFormat,
    center_format: IDWriteTextFormat,
    text_brush: ID2D1Brush,
    accent_brush: ID2D1Brush,
    dim_brush: ID2D1SolidColorBrush,
    border_brush: ID2D1SolidColorBrush,
    handle_outline_brush: ID2D1SolidColorBrush,
    hover_brush: ID2D1SolidColorBrush,
    label_bg_brush: ID2D1SolidColorBrush,
    badge_bg_brush: ID2D1SolidColorBrush,
    badge_text_brush: ID2D1SolidColorBrush,
    notice_bg_brush: ID2D1SolidColorBrush,
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

        let text_format =
            create_text_format(device, FONT_SIZE_PX, DWRITE_FONT_WEIGHT_BOLD, false, false)?;
        // 覆盖层信息文字：常规字重、不换行（超出文本框被 CLIP 截断）。
        let overlay_format = create_text_format(
            device,
            OVERLAY_FONT_SIZE_PX,
            DWRITE_FONT_WEIGHT_NORMAL,
            false,
            true,
        )?;
        // 居中文字（角标/提示）。
        let center_format = create_text_format(
            device,
            OVERLAY_FONT_SIZE_PX,
            DWRITE_FONT_WEIGHT_NORMAL,
            true,
            true,
        )?;

        let text_brush: ID2D1Brush = create_brush(&target, &TEXT_COLOR)?;
        let accent_brush: ID2D1Brush = create_brush(&target, &ACCENT_COLOR)?;
        // 画刷先全部按 `target` 创建，再把 `target` 移进结构体。
        let dim_brush = create_solid_brush(&target, &DIM_COLOR)?;
        let border_brush = create_solid_brush(&target, &BORDER_COLOR)?;
        let handle_outline_brush = create_solid_brush(&target, &HANDLE_OUTLINE)?;
        let hover_brush = create_solid_brush(&target, &HOVER_COLOR)?;
        let label_bg_brush = create_solid_brush(&target, &LABEL_BG)?;
        let badge_bg_brush = create_solid_brush(&target, &BADGE_BG)?;
        let badge_text_brush = create_solid_brush(&target, &TEXT_COLOR)?;
        let notice_bg_brush = create_solid_brush(&target, &NOTICE_BG)?;

        Ok(Renderer {
            swap,
            context,
            target,
            text_format,
            overlay_format,
            center_format,
            text_brush,
            accent_brush,
            dim_brush,
            border_brush,
            handle_outline_brush,
            hover_brush,
            label_bg_brush,
            badge_bg_brush,
            badge_text_brush,
            notice_bg_brush,
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

    /// 绘制冻结帧 + 覆盖层交互场景（design §7.2）。
    ///
    /// 顺序：冻结帧铺满 → 选区外压暗（黑色 50%）→ 悬停窗口高亮 → 选区边框/控制点/
    /// 尺寸标签 → 「已冻结」角标（按 `badge_alpha` 淡出）→ 提示文字。所有矩形都在
    /// `scene` 里以本窗口客户区坐标给出；越界部分由 D2D 裁剪。控制点与标签由调用方
    /// 按归属窗口分配（跨屏时不会重复绘制）。
    pub fn draw_scene(&mut self, frozen: &FrozenTexture, scene: &Scene) -> Result<(), AppError> {
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
        let window = PhysicalRect::new(0, 0, target_width, target_height);
        let dest = D2D_RECT_F {
            left: 0.0,
            top: 0.0,
            right: frozen.width as f32,
            bottom: frozen.height as f32,
        };
        let width = target_width as f32;
        let height = target_height as f32;

        // 选区：压暗区域按与窗口的交集裁剪，边框/控制点用未裁剪坐标（越界部分被 D2D 裁剪，
        // 因此跨屏选区不会在屏间缝隙多画一条边）。
        let dim = scene.selection.and_then(|sel| sel.intersect(&window));

        // SAFETY: 全部绘制调用都发生在 BeginDraw/EndDraw 之间；所有 D2D 结构都是栈上
        // 有效值，brush/text format 由 `self` 持有并在调用期间存活。
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

            // 选区外压暗：没有选区、或选区完全落在其他显示器上时整屏压暗；否则压暗四条边带。
            match dim {
                Some(sel) => {
                    let (l, t) = (sel.x as f32, sel.y as f32);
                    let (r, b) = (sel.right() as f32, sel.bottom() as f32);
                    for band in [
                        D2D_RECT_F {
                            left: 0.0,
                            top: 0.0,
                            right: width,
                            bottom: t,
                        },
                        D2D_RECT_F {
                            left: 0.0,
                            top: b,
                            right: width,
                            bottom: height,
                        },
                        D2D_RECT_F {
                            left: 0.0,
                            top: t,
                            right: l,
                            bottom: b,
                        },
                        D2D_RECT_F {
                            left: r,
                            top: t,
                            right: width,
                            bottom: b,
                        },
                    ] {
                        if band.right > band.left && band.bottom > band.top {
                            self.target.FillRectangle(&band, &self.dim_brush);
                        }
                    }
                }
                None => {
                    let all = D2D_RECT_F {
                        left: 0.0,
                        top: 0.0,
                        right: width,
                        bottom: height,
                    };
                    self.target.FillRectangle(&all, &self.dim_brush);
                }
            }

            // 悬停窗口：青色边框 + 左上角标签。
            if let Some(hover) = scene.hover {
                let rect = to_d2d(hover);
                self.target
                    .DrawRectangle(&rect, &self.hover_brush, 2.0, None);
                if let Some(label) = scene.hover_label.as_deref() {
                    let (left, top) = label_origin(hover, 420.0, 50.0, width, height);
                    let bg = D2D_RECT_F {
                        left,
                        top,
                        right: left + 420.0,
                        bottom: top + 50.0,
                    };
                    self.target.FillRectangle(&bg, &self.label_bg_brush);
                    let text = utf16(label);
                    self.target.DrawText(
                        &text,
                        &self.overlay_format,
                        &bg,
                        &self.hover_brush,
                        D2D1_DRAW_TEXT_OPTIONS_CLIP,
                        DWRITE_MEASURING_MODE_NATURAL,
                    );
                }
            }

            // 选区：2px 实线边框 + 归属本窗口的控制点 + 实时尺寸标签（标签只在归属窗口上）。
            if let Some(selection) = scene.selection {
                let rect = to_d2d(selection);
                self.target
                    .DrawRectangle(&rect, &self.border_brush, 2.0, None);
                draw_handles(
                    &self.target,
                    &scene.handles,
                    scene.dpi,
                    &self.border_brush,
                    &self.handle_outline_brush,
                );
                if let Some(label) = scene.selection_label.as_deref() {
                    let (left, top) = size_label_origin(selection, width, height);
                    let bg = D2D_RECT_F {
                        left,
                        top,
                        right: left + 150.0,
                        bottom: top + 26.0,
                    };
                    self.target.FillRectangle(&bg, &self.label_bg_brush);
                    let text = utf16(label);
                    self.target.DrawText(
                        &text,
                        &self.overlay_format,
                        &bg,
                        &self.text_brush,
                        D2D1_DRAW_TEXT_OPTIONS_CLIP,
                        DWRITE_MEASURING_MODE_NATURAL,
                    );
                }
            }

            // 左上角「已冻结」角标：按 `badge_alpha` 淡出。
            if scene.badge_alpha > 0.0 {
                let alpha = scene.badge_alpha.clamp(0.0, 1.0);
                self.badge_bg_brush.SetOpacity(alpha * BADGE_BG.a);
                self.badge_text_brush.SetOpacity(alpha);
                let bg = D2D_RECT_F {
                    left: 16.0,
                    top: 16.0,
                    right: 116.0,
                    bottom: 50.0,
                };
                self.target.FillRectangle(&bg, &self.badge_bg_brush);
                let text = utf16("已冻结");
                self.target.DrawText(
                    &text,
                    &self.center_format,
                    &bg,
                    &self.badge_text_brush,
                    D2D1_DRAW_TEXT_OPTIONS_CLIP,
                    DWRITE_MEASURING_MODE_NATURAL,
                );
                // 复位不透明度，避免影响后续帧的其他画刷使用者。
                self.badge_bg_brush.SetOpacity(BADGE_BG.a);
                self.badge_text_brush.SetOpacity(1.0);
            }

            // 提示（如「选区过小」）：屏幕中央。
            if let Some(notice) = scene.notice.as_deref() {
                let bg = D2D_RECT_F {
                    left: width / 2.0 - 130.0,
                    top: height / 2.0 - 30.0,
                    right: width / 2.0 + 130.0,
                    bottom: height / 2.0 + 30.0,
                };
                self.target.FillRectangle(&bg, &self.notice_bg_brush);
                let text = utf16(notice);
                self.target.DrawText(
                    &text,
                    &self.center_format,
                    &bg,
                    &self.text_brush,
                    D2D1_DRAW_TEXT_OPTIONS_CLIP,
                    DWRITE_MEASURING_MODE_NATURAL,
                );
            }

            self.target
                .EndDraw(None, None)
                .map_err(|err| failed("ID2D1RenderTarget::EndDraw", err))?;
        }

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
        self.present_vsync(true)
    }

    /// 解除 D2D target 并按 `vsync` 选择 `Present(1, 0)` / `Present(0, 0)`。
    ///
    /// 多屏重绘时只有包含鼠标指针的窗口传 `true`，其余窗口传 `false` 立即入队，
    /// 避免逐屏串行等待 vblank（design §7.2）。
    pub fn present_vsync(&mut self, vsync: bool) -> Result<(), AppError> {
        // SAFETY: 传空 target 只释放对后备缓冲的引用，无额外指针参数。
        unsafe { self.context.SetTarget(None::<&ID2D1Image>) };
        self.swap.present_with_vsync(vsync)
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

/// 创建实心画刷；返回具体类型（需要 `SetOpacity` 时使用）。
fn create_solid_brush(
    target: &ID2D1RenderTarget,
    color: &D2D1_COLOR_F,
) -> Result<ID2D1SolidColorBrush, AppError> {
    // SAFETY: 颜色是栈上有效指针，无画刷属性。
    unsafe { target.CreateSolidColorBrush(color, None) }
        .map_err(|err| failed("CreateSolidColorBrush", err))
}

/// 创建文字格式：可选居中、关闭换行。
fn create_text_format(
    device: &D3dContext,
    size: f32,
    weight: windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT,
    centered: bool,
    no_wrap: bool,
) -> Result<IDWriteTextFormat, AppError> {
    let font_family = wide(FONT_FAMILY);
    let locale = wide(LOCALE);
    // SAFETY: 两个字符串都以 NUL 结尾且在调用期间存活。
    let format = unsafe {
        device.dwrite_factory().CreateTextFormat(
            windows::core::PCWSTR(font_family.as_ptr()),
            None,
            weight,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            size,
            windows::core::PCWSTR(locale.as_ptr()),
        )
    }
    .map_err(|err| failed("IDWriteFactory::CreateTextFormat", err))?;

    // SAFETY: 纯状态设置；失败只影响排版观感，不影响绘制流程。
    unsafe {
        if no_wrap {
            let _ = format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
        }
        if centered {
            let _ = format.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
            let _ = format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
        }
    }
    Ok(format)
}

/// domain 矩形 → D2D 矩形。
fn to_d2d(rect: PhysicalRect) -> D2D_RECT_F {
    D2D_RECT_F {
        left: rect.x as f32,
        top: rect.y as f32,
        right: rect.right() as f32,
        bottom: rect.bottom() as f32,
    }
}

/// 画本窗口归属的控制点：白填充 + 深色描边，边长 `8 × dpi / 96`（至少 4px）。
///
/// `handles` 由调用方用 [`handles_for_window`] 算出（只含落在这个窗口里的点，
/// 坐标已是窗口本地坐标），因此跨屏选区不会在每个相交显示器重复画出全套控制点。
/// 只在 `BeginDraw`/`EndDraw` 之间调用；`target`/`brush` 由 [`Renderer`] 持有。
fn draw_handles(
    target: &ID2D1RenderTarget,
    handles: &[(Handle, PhysicalPoint)],
    dpi: u32,
    fill: &ID2D1SolidColorBrush,
    outline: &ID2D1SolidColorBrush,
) {
    let size = ((8 * dpi.max(96) as u64) / 96).max(4) as f32;
    let half = size / 2.0;
    // SAFETY: 调用方保证处于 BeginDraw/EndDraw 之间；这里只构造栈上矩形并调用 D2D。
    unsafe {
        for (_, point) in handles {
            let cx = point.x as f32;
            let cy = point.y as f32;
            let rect = D2D_RECT_F {
                left: cx - half,
                top: cy - half,
                right: cx + half,
                bottom: cy + half,
            };
            target.FillRectangle(&rect, fill);
            target.DrawRectangle(&rect, outline, 1.0, None);
        }
    }
}

/// 选区尺寸标签的左上角：优先放在选区上沿之外，空间不足时放到选区内侧。
///
/// `top` 始终夹在 `[0, window_h - label_h]`（窗口比标签还矮时取 0）：竖直错位/负坐标
/// 的双屏下选区可能在窗口上方或下方，不夹的话标签会被 D2D 裁掉。
fn size_label_origin(selection: PhysicalRect, window_w: f32, window_h: f32) -> (f32, f32) {
    let label_w = 150.0;
    let label_h = 26.0;
    let max_top = (window_h - label_h).max(0.0);
    let left = (selection.x as f32).clamp(0.0, (window_w - label_w).max(0.0));
    let above = selection.y as f32 - label_h - 6.0;
    let top = if above >= 0.0 {
        above
    } else {
        selection.y as f32 + 6.0
    };
    (left, top.clamp(0.0, max_top))
}

/// 通用标签位置：先试上方，再试下方，最后夹在窗口内；`top` 始终夹在
/// `[0, window_h - label_h]`（窗口更矮时取 0），避免标签被 D2D 裁掉。
fn label_origin(
    rect: PhysicalRect,
    label_w: f32,
    label_h: f32,
    window_w: f32,
    window_h: f32,
) -> (f32, f32) {
    let max_top = (window_h - label_h).max(0.0);
    let left = (rect.x as f32).clamp(0.0, (window_w - label_w).max(0.0));
    let above = rect.y as f32 - label_h - 6.0;
    let top = if above >= 0.0 {
        above
    } else {
        let below = rect.bottom() as f32 + 6.0;
        if below + label_h <= window_h {
            below
        } else {
            max_top
        }
    };
    (left, top.clamp(0.0, max_top))
}

/// 绘制用 UTF-16（不带 NUL；`DrawText` 按切片长度取字符）。
fn utf16(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

/// UTF-16（NUL 结尾）缓冲区，供 Win32/DWrite 直接取指针。
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn failed(op: &str, err: impl std::fmt::Display) -> AppError {
    AppError::Internal(format!("{op} failed: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handle_list(handles: &[(Handle, PhysicalPoint)]) -> Vec<Handle> {
        handles.iter().map(|(handle, _)| *handle).collect()
    }

    fn find(handles: &[(Handle, PhysicalPoint)], wanted: Handle) -> PhysicalPoint {
        handles
            .iter()
            .find(|(handle, _)| *handle == wanted)
            .map(|(_, point)| *point)
            .expect("handle should be owned by this window")
    }

    #[test]
    fn handles_for_single_screen_are_all_local() {
        let selection = PhysicalRect::new(100, 50, 400, 300);
        let window = PhysicalRect::new(0, 0, 2560, 1440);
        let handles = handles_for_window(selection, window, &[window]);
        assert_eq!(handle_list(&handles).len(), 8);
        // 全局坐标减去窗口原点：单屏原点 (0,0) 时本地坐标与全局相同。
        assert_eq!(
            find(&handles, Handle::NorthWest),
            PhysicalPoint { x: 100, y: 50 }
        );
        assert_eq!(
            find(&handles, Handle::SouthEast),
            PhysicalPoint { x: 500, y: 350 }
        );
        assert_eq!(
            find(&handles, Handle::North),
            PhysicalPoint { x: 300, y: 50 }
        );
    }

    #[test]
    fn cross_screen_handles_split_between_windows() {
        let left = PhysicalRect::new(0, 0, 2560, 1440);
        let right = PhysicalRect::new(2560, 0, 2560, 1440);
        let displays = [left, right];
        // 选区横跨两台显示器：x 2400..2800，y 100..300。
        let selection = PhysicalRect::new(2400, 100, 400, 200);

        let left_handles = handles_for_window(selection, left, &displays);
        assert_eq!(
            handle_list(&left_handles),
            vec![Handle::NorthWest, Handle::SouthWest, Handle::West]
        );
        assert_eq!(
            find(&left_handles, Handle::West),
            PhysicalPoint { x: 2400, y: 200 }
        );

        let right_handles = handles_for_window(selection, right, &displays);
        assert_eq!(
            handle_list(&right_handles),
            vec![
                Handle::NorthEast,
                Handle::SouthEast,
                Handle::North,
                Handle::East,
                Handle::South
            ]
        );
        // 右屏原点 x = 2560：North (2600,100) → (40,100)，East (2800,200) → (240,200)。
        assert_eq!(
            find(&right_handles, Handle::North),
            PhysicalPoint { x: 40, y: 100 }
        );
        assert_eq!(
            find(&right_handles, Handle::East),
            PhysicalPoint { x: 240, y: 200 }
        );

        // 两台窗口合计正好八个控制点：每个点只被一个窗口绘制。
        assert_eq!(left_handles.len() + right_handles.len(), 8);
    }

    #[test]
    fn handles_handle_negative_origin_and_offscreen_selection() {
        // 副屏在主屏左侧：原点 x = -2560。
        let negative = PhysicalRect::new(-2560, 0, 2560, 1440);
        let elsewhere = PhysicalRect::new(0, 0, 2560, 1440);
        let displays = [negative, elsewhere];
        let selection = PhysicalRect::new(-300, 10, 200, 100);
        let handles = handles_for_window(selection, negative, &displays);
        assert_eq!(handles.len(), 8);
        assert_eq!(
            find(&handles, Handle::NorthWest),
            PhysicalPoint { x: 2260, y: 10 }
        );

        // 选区完全不在本显示器内（虚拟屏幕的屏间空隙）时没有任何控制点。
        assert!(handles_for_window(selection, elsewhere, &displays).is_empty());
        // 空选区同理。
        assert!(handles_for_window(PhysicalRect::default(), negative, &displays).is_empty());
    }

    #[test]
    fn handle_on_seam_belongs_to_right_window_only() {
        let left = PhysicalRect::new(0, 0, 2560, 1440);
        let right = PhysicalRect::new(2560, 0, 2560, 1440);
        let displays = [left, right];
        // 左边界正好压在分界线上（半开区间 → 归属右屏）。
        let selection = PhysicalRect::new(2560, 100, 100, 100);
        assert!(handles_for_window(selection, left, &displays).is_empty());
        assert_eq!(handles_for_window(selection, right, &displays).len(), 8);
    }

    #[test]
    fn right_edge_handles_reach_the_last_pixel() {
        // 单屏：选区被吸附到屏幕右边缘，右边界 == 虚拟屏 right()。
        let screen = PhysicalRect::new(0, 0, 2560, 1440);
        let selection = PhysicalRect::new(2160, 100, 400, 200);
        assert_eq!(selection.right(), screen.right());
        let handles = handles_for_window(selection, screen, &[screen]);
        // 贴右边缘的一列控制点必须画出来（回归：半开 contains 曾把它们全部过滤掉）。
        assert_eq!(handles.len(), 8);
        // 中心夹到窗口最后一个像素：全局 2559 → 本地 2559。
        assert_eq!(
            find(&handles, Handle::East),
            PhysicalPoint { x: 2559, y: 200 }
        );
        assert_eq!(
            find(&handles, Handle::NorthEast),
            PhysicalPoint { x: 2559, y: 100 }
        );
        assert_eq!(
            find(&handles, Handle::SouthEast),
            PhysicalPoint { x: 2559, y: 300 }
        );
    }

    #[test]
    fn bottom_edge_handles_reach_the_last_pixel() {
        // 选区右下角贴住屏幕右下角：三条边上的控制点在两个维度都要夹回窗口内。
        let screen = PhysicalRect::new(0, 0, 2560, 1440);
        let selection = PhysicalRect::new(2160, 1140, 400, 300);
        assert_eq!(
            (selection.right(), selection.bottom()),
            (screen.right(), screen.bottom())
        );
        let handles = handles_for_window(selection, screen, &[screen]);
        assert_eq!(handles.len(), 8);
        assert_eq!(
            find(&handles, Handle::South),
            PhysicalPoint { x: 2360, y: 1439 }
        );
        assert_eq!(
            find(&handles, Handle::SouthEast),
            PhysicalPoint { x: 2559, y: 1439 }
        );
        assert_eq!(
            find(&handles, Handle::SouthWest),
            PhysicalPoint { x: 2160, y: 1439 }
        );
    }

    #[test]
    fn negative_origin_window_owns_bottom_edge_handles() {
        // 主屏 (0,0)，副屏在主屏**下方左侧**（负 x + 正 y）：虚拟屏 right/bottom 都由副屏决定。
        let primary = PhysicalRect::new(0, 0, 2560, 1440);
        let secondary = PhysicalRect::new(-2560, 1440, 2560, 1440);
        let displays = [primary, secondary];
        // 副屏上的选区贴住虚拟屏下沿（副屏下沿临空）。
        let selection = PhysicalRect::new(-2000, 2580, 400, 300);
        assert_eq!(selection.bottom(), secondary.bottom());

        let secondary_handles = handles_for_window(selection, secondary, &displays);
        assert_eq!(secondary_handles.len(), 8);
        // 副屏原点 (-2560, 1440)：全局 (-1600, 2879) → 本地 (960, 1439)。
        assert_eq!(
            find(&secondary_handles, Handle::SouthEast),
            PhysicalPoint { x: 960, y: 1439 }
        );
        assert_eq!(
            find(&secondary_handles, Handle::South),
            PhysicalPoint { x: 760, y: 1439 }
        );

        // 主屏下边不是虚拟屏下沿，且 y 范围不含这些点：一个控制点也不归主屏。
        assert!(handles_for_window(selection, primary, &displays).is_empty());

        // 选区贴虚拟屏左沿（负坐标）时，左列控制点仍归副屏且本地 x = 0。
        let flush_left = PhysicalRect::new(-2560, 1500, 200, 100);
        let left_handles = handles_for_window(flush_left, secondary, &displays);
        assert_eq!(left_handles.len(), 8);
        assert_eq!(
            find(&left_handles, Handle::West),
            PhysicalPoint { x: 0, y: 110 }
        );
    }

    #[test]
    fn internal_seam_handles_are_not_duplicated() {
        // 横向双屏：选区右边界正好压在内部屏缝上。
        let left = PhysicalRect::new(0, 0, 2560, 1440);
        let right = PhysicalRect::new(2560, 0, 2560, 1440);
        let displays = [left, right];
        let selection = PhysicalRect::new(2400, 100, 160, 200);
        assert_eq!(selection.right(), left.right());

        let left_handles = handles_for_window(selection, left, &displays);
        let right_handles = handles_for_window(selection, right, &displays);
        // 屏缝上的三个控制点（NE/SE/E）只归右屏；左屏只剩不在屏缝上的五个。
        assert_eq!(
            handle_list(&left_handles),
            vec![
                Handle::NorthWest,
                Handle::SouthWest,
                Handle::North,
                Handle::South,
                Handle::West
            ]
        );
        assert_eq!(
            handle_list(&right_handles),
            vec![Handle::NorthEast, Handle::SouthEast, Handle::East]
        );
        assert_eq!(left_handles.len() + right_handles.len(), 8);

        // 竖直双屏：水平屏缝同理，分界行只归下方的窗口。
        let top = PhysicalRect::new(0, 0, 2560, 1440);
        let bottom = PhysicalRect::new(0, 1440, 2560, 1440);
        let displays = [top, bottom];
        let selection = PhysicalRect::new(100, 1340, 200, 100);
        assert_eq!(selection.bottom(), top.bottom());
        let top_handles = handles_for_window(selection, top, &displays);
        let bottom_handles = handles_for_window(selection, bottom, &displays);
        assert_eq!(
            handle_list(&top_handles),
            vec![
                Handle::NorthWest,
                Handle::NorthEast,
                Handle::North,
                Handle::East,
                Handle::West
            ]
        );
        assert_eq!(
            handle_list(&bottom_handles),
            vec![Handle::SouthEast, Handle::SouthWest, Handle::South]
        );
        assert_eq!(top_handles.len() + bottom_handles.len(), 8);
    }

    #[test]
    fn outer_edge_clamp_does_not_leak_into_far_windows() {
        // 虚拟屏外沿上的控制点只归与外沿重合的窗口：远处（屏间空隙/其他显示器）
        // 的窗口即使几何上在另一维对齐也不能接收。
        let left = PhysicalRect::new(0, 0, 2560, 1440);
        let right = PhysicalRect::new(2560, 0, 2560, 1440);
        let displays = [left, right];
        let selection = PhysicalRect::new(4720, 100, 400, 200);
        assert_eq!(selection.right(), right.right());

        assert_eq!(handles_for_window(selection, right, &displays).len(), 8);
        assert!(handles_for_window(selection, left, &displays).is_empty());
    }

    #[test]
    fn exposed_bottom_edge_of_shorter_display_owns_handles() {
        // 主屏 2560×1440 + 右侧较矮副屏 2560×720：副屏下沿外侧没有其他显示器覆盖
        //（主屏的 y 范围虽包含 720，但 x 范围不覆盖副屏所在列）→ 临空边。
        let primary = PhysicalRect::new(0, 0, 2560, 1440);
        let secondary = PhysicalRect::new(2560, 0, 2560, 720);
        let displays = [primary, secondary];
        let selection = PhysicalRect::new(2800, 520, 400, 200);
        assert_eq!(selection.bottom(), secondary.bottom());

        let handles = handles_for_window(selection, secondary, &displays);
        assert_eq!(handles.len(), 8);
        // 下沿中心夹到副屏最后一个像素：全局 y = 719 → 本地 719。
        assert_eq!(
            find(&handles, Handle::SouthWest),
            PhysicalPoint { x: 240, y: 719 }
        );
        assert_eq!(
            find(&handles, Handle::South),
            PhysicalPoint { x: 440, y: 719 }
        );
        assert_eq!(
            find(&handles, Handle::SouthEast),
            PhysicalPoint { x: 640, y: 719 }
        );

        // 主屏在 x 上完全不含该选区：一个控制点也不归它（不重复绘制）。
        assert!(handles_for_window(selection, primary, &displays).is_empty());
    }

    #[test]
    fn exposed_right_edge_of_narrower_display_owns_handles() {
        // 主屏 2560×1440 + 右侧较窄副屏 1280×1440：副屏右沿临空。
        let primary = PhysicalRect::new(0, 0, 2560, 1440);
        let secondary = PhysicalRect::new(2560, 0, 1280, 1440);
        let displays = [primary, secondary];
        let selection = PhysicalRect::new(3440, 100, 400, 200);
        assert_eq!(selection.right(), secondary.right());

        let handles = handles_for_window(selection, secondary, &displays);
        assert_eq!(handles.len(), 8);
        // 右沿中心夹到副屏最后一个像素：全局 x = 3839 → 本地 1279。
        assert_eq!(
            find(&handles, Handle::NorthEast),
            PhysicalPoint { x: 1279, y: 100 }
        );
        assert_eq!(
            find(&handles, Handle::East),
            PhysicalPoint { x: 1279, y: 200 }
        );
        assert_eq!(
            find(&handles, Handle::SouthEast),
            PhysicalPoint { x: 1279, y: 300 }
        );

        assert!(handles_for_window(selection, primary, &displays).is_empty());
    }

    #[test]
    fn interior_seam_is_not_claimed_by_both_windows() {
        // 屏缝两侧高度不同：分界线外侧有其他显示器覆盖，仍只归右/下窗口。
        let primary = PhysicalRect::new(0, 0, 2560, 1440);
        let shorter = PhysicalRect::new(2560, 0, 2560, 720);
        let displays = [primary, shorter];
        let selection = PhysicalRect::new(2400, 100, 160, 200);
        assert_eq!(selection.right(), primary.right());

        let left_handles = handles_for_window(selection, primary, &displays);
        let right_handles = handles_for_window(selection, shorter, &displays);
        assert_eq!(
            handle_list(&left_handles),
            vec![
                Handle::NorthWest,
                Handle::SouthWest,
                Handle::North,
                Handle::South,
                Handle::West
            ]
        );
        assert_eq!(
            handle_list(&right_handles),
            vec![Handle::NorthEast, Handle::SouthEast, Handle::East]
        );
        assert_eq!(left_handles.len() + right_handles.len(), 8);

        // 横向屏缝、下方显示器较窄：分界行只归下方窗口。
        let upper = PhysicalRect::new(0, 0, 2560, 720);
        let lower = PhysicalRect::new(0, 720, 1280, 720);
        let displays = [upper, lower];
        let selection = PhysicalRect::new(100, 620, 200, 100);
        assert_eq!(selection.bottom(), upper.bottom());

        let upper_handles = handles_for_window(selection, upper, &displays);
        let lower_handles = handles_for_window(selection, lower, &displays);
        assert_eq!(
            handle_list(&upper_handles),
            vec![
                Handle::NorthWest,
                Handle::NorthEast,
                Handle::North,
                Handle::East,
                Handle::West
            ]
        );
        assert_eq!(
            handle_list(&lower_handles),
            vec![Handle::SouthEast, Handle::SouthWest, Handle::South]
        );
        assert_eq!(upper_handles.len() + lower_handles.len(), 8);
    }

    #[test]
    fn negative_origin_display_exposed_bottom_edge_owns_handles() {
        // 主屏较矮，副屏在主屏左侧且更高（负 x）：副屏下沿临空。
        let primary = PhysicalRect::new(0, 0, 2560, 720);
        let secondary = PhysicalRect::new(-2560, 0, 2560, 1440);
        let displays = [primary, secondary];
        let selection = PhysicalRect::new(-2000, 1240, 400, 200);
        assert_eq!(selection.bottom(), secondary.bottom());

        let handles = handles_for_window(selection, secondary, &displays);
        assert_eq!(handles.len(), 8);
        // 副屏原点 (-2560, 0)：全局 (-1600, 1439) → 本地 (960, 1439)。
        assert_eq!(
            find(&handles, Handle::SouthEast),
            PhysicalPoint { x: 960, y: 1439 }
        );
        assert_eq!(
            find(&handles, Handle::South),
            PhysicalPoint { x: 760, y: 1439 }
        );
        assert_eq!(
            find(&handles, Handle::SouthWest),
            PhysicalPoint { x: 560, y: 1439 }
        );

        // 主屏的 y 范围不含该选区：一个控制点也不归它。
        assert!(handles_for_window(selection, primary, &displays).is_empty());
    }

    #[test]
    fn virtual_outer_edges_still_reach_the_last_pixel() {
        // 主屏 2560×1440 + 右侧较矮副屏 2560×720：虚拟屏右沿在副屏、下沿在主屏。
        let primary = PhysicalRect::new(0, 0, 2560, 1440);
        let secondary = PhysicalRect::new(2560, 0, 2560, 720);
        let displays = [primary, secondary];

        // 贴虚拟屏右沿：右列控制点归副屏，夹到本屏最后一个像素。
        let right = PhysicalRect::new(4720, 100, 400, 200);
        assert_eq!(right.right(), secondary.right());
        let right_handles = handles_for_window(right, secondary, &displays);
        assert_eq!(right_handles.len(), 8);
        assert_eq!(
            find(&right_handles, Handle::East),
            PhysicalPoint { x: 2559, y: 200 }
        );
        assert_eq!(
            find(&right_handles, Handle::SouthEast),
            PhysicalPoint { x: 2559, y: 300 }
        );
        assert!(handles_for_window(right, primary, &displays).is_empty());

        // 贴虚拟屏下沿：下行控制点归主屏，夹到本屏最后一个像素。
        let bottom = PhysicalRect::new(2160, 1140, 400, 300);
        assert_eq!(bottom.bottom(), primary.bottom());
        let bottom_handles = handles_for_window(bottom, primary, &displays);
        assert_eq!(bottom_handles.len(), 8);
        assert_eq!(
            find(&bottom_handles, Handle::South),
            PhysicalPoint { x: 2360, y: 1439 }
        );
        assert_eq!(
            find(&bottom_handles, Handle::SouthEast),
            PhysicalPoint { x: 2559, y: 1439 }
        );
        assert!(handles_for_window(bottom, secondary, &displays).is_empty());
    }

    #[test]
    fn label_anchor_points_and_owner() {
        let left = PhysicalRect::new(0, 0, 2560, 1440);
        let right = PhysicalRect::new(2560, 0, 2560, 1440);
        let windows = [left, right];
        let selection = PhysicalRect::new(2400, 100, 400, 200);

        // 右下角用最后一个像素（半开区间），跨屏时落在右屏。
        assert_eq!(bottom_right(selection), PhysicalPoint { x: 2799, y: 299 });
        assert_eq!(center(selection), PhysicalPoint { x: 2600, y: 200 });
        assert_eq!(point_owner(bottom_right(selection), &windows), Some(1));
        assert_eq!(point_owner(center(selection), &windows), Some(1));

        // 单屏选区与负坐标。
        let single = PhysicalRect::new(100, 100, 50, 50);
        assert_eq!(point_owner(bottom_right(single), &[left]), Some(0));
        let negative_window = [PhysicalRect::new(-2560, 0, 2560, 1440)];
        let negative_selection = PhysicalRect::new(-300, 10, 100, 100);
        assert_eq!(
            point_owner(bottom_right(negative_selection), &negative_window),
            Some(0)
        );
        // 落在所有窗口之外。
        assert_eq!(point_owner(PhysicalPoint { x: -1, y: 0 }, &windows), None);
    }

    #[test]
    fn size_label_origin_keeps_top_inside_the_window() {
        let (window_w, window_h) = (2560.0, 1440.0);
        // 常规：选区上方有空间 → 放在上方。
        let (_, top) = size_label_origin(PhysicalRect::new(100, 200, 300, 200), window_w, window_h);
        assert_eq!(top, 200.0 - 26.0 - 6.0);
        // 选区贴顶：上方没有空间 → 放在选区内侧，且不为负。
        let (_, top) = size_label_origin(PhysicalRect::new(100, 0, 300, 200), window_w, window_h);
        assert_eq!(top, 6.0);
        // 选区在窗口上方（负 y，竖直错位双屏）：内侧也在窗口之上，必须夹回 0。
        let (_, top) =
            size_label_origin(PhysicalRect::new(100, -300, 300, 100), window_w, window_h);
        assert_eq!(top, 0.0);
        // 选区在窗口下方：上方的起点超出窗口，夹到窗口内最后一个标签位置。
        let (_, top) =
            size_label_origin(PhysicalRect::new(100, 1600, 300, 100), window_w, window_h);
        assert_eq!(top, window_h - 26.0);
        // 窗口比标签还矮：只能取 0。
        let (_, top) = size_label_origin(PhysicalRect::new(0, 10, 100, 100), 200.0, 20.0);
        assert_eq!(top, 0.0);
    }

    #[test]
    fn label_origin_keeps_top_inside_the_window() {
        let (window_w, window_h) = (2560.0, 1440.0);
        // 常规：上方有空间。
        let (_, top) = label_origin(
            PhysicalRect::new(500, 600, 300, 200),
            420.0,
            50.0,
            window_w,
            window_h,
        );
        assert_eq!(top, 600.0 - 50.0 - 6.0);
        // 竖直错位双屏：悬停窗口在窗口下方（上方无空间、下方也放不下）→ 夹到窗口下沿。
        let (_, top) = label_origin(
            PhysicalRect::new(500, 1500, 300, 200),
            420.0,
            50.0,
            window_w,
            window_h,
        );
        assert_eq!(top, window_h - 50.0);
        // 悬停窗口在窗口上方（负 y）：下方位置为负，必须夹回 0。
        let (_, top) = label_origin(
            PhysicalRect::new(500, -400, 300, 200),
            420.0,
            50.0,
            window_w,
            window_h,
        );
        assert_eq!(top, 0.0);
        // 上方空间不足、下方够放 → 放在下方。
        let (_, top) = label_origin(
            PhysicalRect::new(500, 20, 300, 200),
            420.0,
            50.0,
            window_w,
            window_h,
        );
        assert_eq!(top, 226.0);
        // 窗口比标签还矮。
        let (_, top) = label_origin(PhysicalRect::new(0, 0, 100, 100), 420.0, 50.0, 200.0, 30.0);
        assert_eq!(top, 0.0);
    }
}
