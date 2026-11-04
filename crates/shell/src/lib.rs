//! 系统外壳层：全局快捷键等与 Windows 窗口/输入系统直接交互的部件。
//!
//! 线程与消息所有权见 design §3：热键线程只负责注册与把按键转成
//! [`ShellEvent`]，真正的捕获编排由 app-core 完成。

pub mod hotkey;

pub use hotkey::{spawn_hotkey_thread, HotkeyHandle, HotkeySpec};

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
/// 阶段 01 的 Task 6 只产生 [`ShellEvent::CaptureRequest`]，其余变体随后续任务加入。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShellEvent {
    CaptureRequest(CaptureTrigger),
}

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
