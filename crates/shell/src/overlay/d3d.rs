//! D3D11 设备、DXGI 工厂与 swap chain 封装（design §7.2）。
//!
//! 创建设备的顺序固定为：
//! `D3D11CreateDevice(BGRA_SUPPORT, 硬件 → WARP 回退)`
//! → `IDXGIDevice` → `IDXGIAdapter` → `IDXGIFactory2`
//! → `CreateSwapChainForHwnd(B8G8R8A8_UNORM, FLIP_SEQUENTIAL, 2 buffers)`。
//!
//! 线程约束：D2D 的单线程工厂（[`D2D1_FACTORY_TYPE_SINGLE_THREADED`]）、
//! `ID2D1DeviceContext` 与 DXGI 交换链都有线程亲和性，必须固定在创建它们的覆盖层线程。
//! 注意 windows-rs 生成的 COM 接口封装（`ID3D11Device`/`IDXGISwapChain1`/`ID2D1DeviceContext`）
//! 在 Rust 类型层面**实现了** `Send`，所以“接口不 Send”并不能自动阻止跨线程移动；
//! 本模块用 `PhantomData<*const ()>` 显式把 [`D3dContext`]/[`SwapChain`] 标成 `!Send`
//! （`render::Renderer` 同样处理），让编译器替我们守住这条不变量。
//! 设备/交换链创建失败一律转成 [`AppError::Internal`]，由调用方决定是否降级。

use std::fmt;
use std::marker::PhantomData;

use tracing::warn;
use visrecall_domain::AppError;
use windows::core::{Interface, BOOL};
use windows::Win32::Foundation::{HMODULE, HWND};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Factory1, D2D1_FACTORY_TYPE_SINGLE_THREADED,
};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE, D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP, D3D_FEATURE_LEVEL_11_0,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_BOX,
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAPPED_SUBRESOURCE,
    D3D11_MAP_READ, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, DWRITE_FACTORY_TYPE_SHARED,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_IGNORE, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    IDXGIAdapter, IDXGIDevice, IDXGIFactory2, IDXGIOutput, IDXGISurface, IDXGISwapChain1,
    DXGI_PRESENT, DXGI_SCALING_STRETCH, DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
    DXGI_USAGE_RENDER_TARGET_OUTPUT,
};

/// swap chain 的缓冲区数量；FLIP_SEQUENTIAL 至少 2 个。
pub const SWAP_CHAIN_BUFFERS: u32 = 2;

/// 覆盖层使用的后备缓冲格式（design §7.2）。
pub const BACK_BUFFER_FORMAT: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT =
    DXGI_FORMAT_B8G8R8A8_UNORM;

/// D3D11 设备 + 两个全局工厂（D2D/DWrite）。
///
/// 构造失败即认为覆盖层渲染不可用；调用方按 design §7.3 决定降级，
/// 本类型不做任何静默回退，除硬件失败后尝试 WARP 之外。
///
/// `!Send`：见模块文档。设备/立即上下文/交换链只能在覆盖层线程使用。
pub struct D3dContext {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    dxgi_factory: IDXGIFactory2,
    d2d_factory: ID2D1Factory1,
    dwrite_factory: IDWriteFactory,
    driver: D3D_DRIVER_TYPE,
    adapter_name: String,
    /// 显式禁止 `Send`（windows-rs 的 COM 接口封装本身实现了 `Send`）。
    _not_send: PhantomData<*const ()>,
}

impl D3dContext {
    /// 创建硬件设备；失败时回退到 WARP 软件设备。
    pub fn new() -> Result<D3dContext, AppError> {
        let (device, context, driver) = match create_device(D3D_DRIVER_TYPE_HARDWARE) {
            Ok(pair) => (pair.0, pair.1, D3D_DRIVER_TYPE_HARDWARE),
            Err(err) => {
                warn!(error = %err, "hardware D3D11 device failed; falling back to WARP");
                let (device, context) = create_device(D3D_DRIVER_TYPE_WARP)?;
                (device, context, D3D_DRIVER_TYPE_WARP)
            }
        };

        // `cast` 只做 QueryInterface；`device` 刚创建、在本线程存活。
        let dxgi_device: IDXGIDevice = device
            .cast()
            .map_err(|err| failed("ID3D11Device -> IDXGIDevice", err))?;
        // SAFETY: `dxgi_device` 在作用域内有效。
        let adapter: IDXGIAdapter = unsafe { dxgi_device.GetAdapter() }
            .map_err(|err| failed("IDXGIDevice::GetAdapter", err))?;
        // SAFETY: 适配器在作用域内有效，`GetParent` 返回其创建工厂。
        let dxgi_factory: IDXGIFactory2 =
            unsafe { adapter.GetParent() }.map_err(|err| failed("IDXGIAdapter::GetParent", err))?;
        // SAFETY: 只读取适配器描述；失败时回退到 `unknown`，不影响渲染。
        let adapter_name = unsafe { adapter.GetDesc() }
            .map(|desc| utf16_lossy(&desc.Description))
            .unwrap_or_else(|_| "unknown".to_string());

        // SAFETY: 单线程 D2D 工厂，无额外选项；工厂由本结构持有。
        let d2d_factory =
            unsafe { D2D1CreateFactory::<ID2D1Factory1>(D2D1_FACTORY_TYPE_SINGLE_THREADED, None) }
                .map_err(|err| failed("D2D1CreateFactory", err))?;
        // SAFETY: 共享 DWrite 工厂，无参数指针。
        let dwrite_factory =
            unsafe { DWriteCreateFactory::<IDWriteFactory>(DWRITE_FACTORY_TYPE_SHARED) }
                .map_err(|err| failed("DWriteCreateFactory", err))?;

        Ok(D3dContext {
            device,
            context,
            dxgi_factory,
            d2d_factory,
            dwrite_factory,
            driver,
            adapter_name,
            _not_send: PhantomData,
        })
    }

    /// 实际使用的驱动类型，用于日志与 spike 报告（`hardware` 或 `warp`）。
    pub fn driver_name(&self) -> &'static str {
        if self.driver == D3D_DRIVER_TYPE_WARP {
            "warp"
        } else {
            "hardware"
        }
    }

    /// 默认适配器名称（DXGI 描述），用于 spike 报告。
    pub fn adapter_name(&self) -> &str {
        &self.adapter_name
    }

    /// 为窗口创建 FLIP_SEQUENTIAL 交换链；尺寸单位是物理像素。
    pub fn create_swap_chain(
        &self,
        hwnd: HWND,
        width: u32,
        height: u32,
    ) -> Result<SwapChain, AppError> {
        if width == 0 || height == 0 {
            return Err(AppError::InvalidInput(format!(
                "swap chain size must be non-zero, got {width}x{height}"
            )));
        }

        let desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: width,
            Height: height,
            Format: BACK_BUFFER_FORMAT,
            Stereo: BOOL(0),
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: SWAP_CHAIN_BUFFERS,
            Scaling: DXGI_SCALING_STRETCH,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
            AlphaMode: DXGI_ALPHA_MODE_IGNORE,
            Flags: 0,
        };

        // SAFETY: `hwnd` 由调用方保证在本线程存活；`desc` 是栈上有效指针；
        // 设备与工厂在本结构内存活。
        let chain = unsafe {
            self.dxgi_factory.CreateSwapChainForHwnd(
                &self.device,
                hwnd,
                &desc,
                None,
                None::<&IDXGIOutput>,
            )
        }
        .map_err(|err| failed("IDXGIFactory2::CreateSwapChainForHwnd", err))?;

        Ok(SwapChain {
            chain,
            width,
            height,
            _not_send: PhantomData,
        })
    }

    pub fn device(&self) -> &ID3D11Device {
        &self.device
    }

    pub fn immediate_context(&self) -> &ID3D11DeviceContext {
        &self.context
    }

    pub fn d2d_factory(&self) -> &ID2D1Factory1 {
        &self.d2d_factory
    }

    pub fn dwrite_factory(&self) -> &IDWriteFactory {
        &self.dwrite_factory
    }

    /// 从 `source` 回读一块区域，返回紧排（每行 `width * 4` 字节）的 top-down BGRA8。
    ///
    /// 流程（design §7.2）：创建与区域同尺寸的 `D3D11_USAGE_STAGING` 纹理 →
    /// `CopySubresourceRegion`（源纹理 → staging）→ `Map(D3D11_MAP_READ)` →
    /// 按 `RowPitch` 逐行复制 → `Unmap`。staging 纹理在函数返回时释放。
    ///
    /// 调用方负责保证 `x + width`/`y + height` 不超过源纹理尺寸；参数非法返回
    /// [`AppError::InvalidInput`]，`Map` 失败返回 [`AppError::Internal`]（不 panic）。
    pub fn read_back_region(
        &self,
        source: &ID3D11Texture2D,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    ) -> Result<Vec<u8>, AppError> {
        if width == 0 || height == 0 {
            return Err(AppError::InvalidInput(format!(
                "readback region must be non-zero, got {width}x{height}"
            )));
        }
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
        };
        let mut staging: Option<ID3D11Texture2D> = None;
        // SAFETY: `desc` 是栈上有效结构；输出指针指向本函数内的 `Option`。
        unsafe { self.device.CreateTexture2D(&desc, None, Some(&mut staging)) }
            .map_err(|err| failed("ID3D11Device::CreateTexture2D(staging)", err))?;
        let staging = staging
            .ok_or_else(|| AppError::Internal("staging CreateTexture2D returned none".into()))?;

        let Some(right) = x.checked_add(width) else {
            return Err(AppError::InvalidInput(
                "readback x + width overflows".into(),
            ));
        };
        let Some(bottom) = y.checked_add(height) else {
            return Err(AppError::InvalidInput(
                "readback y + height overflows".into(),
            ));
        };
        let region = D3D11_BOX {
            left: x,
            top: y,
            front: 0,
            right,
            bottom,
            back: 1,
        };
        // SAFETY: 两个纹理都有效；`region` 是栈上有效结构，坐标由调用方保证在源纹理内。
        unsafe {
            self.context
                .CopySubresourceRegion(&staging, 0, 0, 0, 0, source, 0, Some(&region));
        }

        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: `mapped` 指向栈上有效结构；staging 纹理以 `CPU_ACCESS_READ` 创建。
        unsafe {
            self.context
                .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
        }
        .map_err(|err| failed("ID3D11DeviceContext::Map(staging)", err))?;

        // `Map` 成功但数据指针为空时无法复制：立即 `Unmap` 并报错，绝不带着空指针读。
        if mapped.pData.is_null() {
            // SAFETY: 与上面的 `Map` 成对；未解引用 `pData`。
            unsafe { self.context.Unmap(&staging, 0) };
            return Err(AppError::Internal(
                "ID3D11DeviceContext::Map(staging) returned a null data pointer".into(),
            ));
        }

        let row_bytes = width as usize * 4;
        let mut out = vec![0u8; row_bytes * height as usize];
        // SAFETY: `pData` 指向 staging 纹理的映射区，至少 `RowPitch * height` 字节可读；
        // 逐行按 `RowPitch` 步进，只读取 `row_bytes` 字节，不越界。复制完成后立即 `Unmap`。
        unsafe {
            let src = mapped.pData.cast::<u8>();
            for row in 0..height as usize {
                let src_row = src.add(row * mapped.RowPitch as usize);
                std::ptr::copy_nonoverlapping(
                    src_row,
                    out.as_mut_ptr().add(row * row_bytes),
                    row_bytes,
                );
            }
            self.context.Unmap(&staging, 0);
        }
        Ok(out)
    }
}

/// 一个窗口的交换链；只允许在创建它的线程上使用（`!Send`）。
pub struct SwapChain {
    chain: IDXGISwapChain1,
    width: u32,
    height: u32,
    /// 显式禁止 `Send`（windows-rs 的 COM 接口封装本身实现了 `Send`）。
    _not_send: PhantomData<*const ()>,
}

impl SwapChain {
    /// 后备缓冲宽度（物理像素）。
    pub fn width(&self) -> u32 {
        self.width
    }

    /// 后备缓冲高度（物理像素）。
    pub fn height(&self) -> u32 {
        self.height
    }

    /// 当前后备缓冲（buffer 0）。FLIP 模型下每帧 `Present` 后缓冲区轮换，
    /// 因此每帧都要重新取一次。
    pub fn back_buffer(&self) -> Result<IDXGISurface, AppError> {
        // SAFETY: 交换链在结构内存活，buffer 0 恒存在（BufferCount = 2）。
        unsafe { self.chain.GetBuffer::<IDXGISurface>(0) }
            .map_err(|err| failed("IDXGISwapChain::GetBuffer", err))
    }

    /// `Present(1, 0)`：与 vsync 同步，单帧阻塞到下一个刷新周期。
    pub fn present(&self) -> Result<(), AppError> {
        self.present_with_vsync(true)
    }

    /// `Present(SyncInterval, 0)`：`vsync = true` 用 1（等 vblank），`false` 用 0（立即返回）。
    ///
    /// 多屏时只有包含鼠标指针的窗口需要等 vblank，其余窗口用 0 立即入队，
    /// 避免一次重绘串行等待多个刷新周期（design §7.2）。
    pub fn present_with_vsync(&self, vsync: bool) -> Result<(), AppError> {
        let interval = u32::from(vsync);
        // SAFETY: 交换链存活；Present 不持有额外指针。
        unsafe { self.chain.Present(interval, DXGI_PRESENT(0)) }
            .ok()
            .map_err(|err| failed("IDXGISwapChain::Present", err))
    }
}

impl fmt::Debug for SwapChain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SwapChain")
            .field("width", &self.width)
            .field("height", &self.height)
            .finish()
    }
}

/// 以给定驱动类型创建设备与立即上下文；特征级别只要求 11.0。
fn create_device(driver: D3D_DRIVER_TYPE) -> Result<(ID3D11Device, ID3D11DeviceContext), AppError> {
    let levels = [D3D_FEATURE_LEVEL_11_0];
    let mut device: Option<ID3D11Device> = None;
    let mut context: Option<ID3D11DeviceContext> = None;
    let mut level = D3D_FEATURE_LEVEL_11_0;

    // SAFETY: 适配器传 `None` 表示默认适配器；三个输出指针都指向栈上有效变量。
    unsafe {
        D3D11CreateDevice(
            None,
            driver,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            Some(&levels),
            D3D11_SDK_VERSION,
            Some(&mut device),
            Some(&mut level),
            Some(&mut context),
        )
    }
    .map_err(|err| failed("D3D11CreateDevice", err))?;

    match (device, context) {
        (Some(device), Some(context)) => Ok((device, context)),
        _ => Err(AppError::Internal(
            "D3D11CreateDevice returned no device/context".into(),
        )),
    }
}

/// 统一把 Windows HRESULT 错误包成 [`AppError::Internal`]，保留操作名便于定位。
fn failed(op: &str, err: impl fmt::Display) -> AppError {
    AppError::Internal(format!("{op} failed: {err}"))
}

/// 定长 UTF-16（NUL 结尾）字段转 `String`。
fn utf16_lossy(raw: &[u16]) -> String {
    let len = raw.iter().position(|&c| c == 0).unwrap_or(raw.len());
    String::from_utf16_lossy(&raw[..len])
}
