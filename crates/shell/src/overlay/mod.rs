//! D3D11 + Direct2D 覆盖层渲染骨架（design §7.2）。
//!
//! 阶段 01 的覆盖层每台显示器一个 `WS_POPUP` 窗口、一个 swap chain；D3D11 设备、
//! swap chain 与 D2D 上下文都只在创建它们的那个线程使用，不跨线程移动（design §3）。
//! 本模块只提供渲染原语，窗口/输入/选区在后续任务接入。
//!
//! 线程不变量：D2D 的单线程工厂与设备上下文有线程亲和性，必须固定在覆盖层线程。
//! windows-rs 的 COM 接口封装（`ID3D11Device`、`IDXGISwapChain1`、`ID2D1DeviceContext`
//! 等）在 Rust 类型层面**实现了** `Send`，因此不能靠接口类型自动保证单线程；本模块给
//! [`D3dContext`]、[`SwapChain`] 与 [`Renderer`] 都加了 `PhantomData<*const ()>`，
//! 让它们成为 `!Send`，跨线程使用会在编译期被拒绝。

pub mod d3d;
pub mod render;

pub use d3d::{D3dContext, SwapChain};
pub use render::Renderer;
