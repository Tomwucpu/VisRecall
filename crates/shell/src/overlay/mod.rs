//! D3D11 + Direct2D 覆盖层渲染骨架（design §7.2）。
//!
//! 阶段 01 的覆盖层每台显示器一个 `WS_POPUP` 窗口、一个 swap chain；D3D11 设备、
//! swap chain 与 D2D 上下文都只在创建它们的那个线程使用，不跨线程移动 COM 接口
//! （design §3）。本模块只提供渲染原语，窗口/输入/选区在后续任务接入。

pub mod d3d;
pub mod render;

pub use d3d::{D3dContext, SwapChain};
pub use render::Renderer;
