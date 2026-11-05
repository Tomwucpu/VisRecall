//! 系统外壳层：全局快捷键、悬浮球等与 Windows 窗口/输入系统直接交互的部件。
//!
//! 线程与消息所有权见 design §3：热键线程只负责注册与把按键转成
//! [`ShellEvent`]，悬浮球线程只负责窗口交互，真正的捕获编排由 app-core 完成。

pub mod ball;
pub mod hotkey;
pub mod overlay;
pub mod selection;

pub use ball::{
    clamp_ball_position, spawn_ball_thread, BallConfig, BallHandle, BALL_CLASS_NAME,
    DEFAULT_BALL_SIZE,
};
pub use hotkey::{spawn_hotkey_thread, HotkeyHandle, HotkeySpec};
pub use overlay::{
    spawn_overlay_thread, D3dContext, FrozenTexture, OverlayCommand, OverlayEvent, OverlayHandle,
    Renderer, SwapChain,
};

/// 触发捕获的来源；事件只携带类型，不携带图像数据。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureTrigger {
    Hotkey,
    BallClick,
    UiButton,
    Drop,
}

/// 外壳层（热键线程、悬浮球线程）投递给 app-core 的事件。
///
/// 阶段 01 只产生 [`ShellEvent::CaptureRequest`] 与 [`ShellEvent::BallMoved`]；
/// 拖放内容（`Drop`）、悬浮球可见性事件随后续任务加入（design §3.1）。
#[derive(Clone, Debug, PartialEq)]
pub enum ShellEvent {
    CaptureRequest(CaptureTrigger),
    /// 悬浮球拖动结束：归一化中心坐标（`0.0..=1.0`，相对显示器 `bounds`）。
    ///
    /// 悬浮球线程不写配置，由 app-core 落到 `config.ball.positions[display_id]`。
    BallMoved {
        display_id: String,
        nx: f32,
        ny: f32,
    },
}

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
