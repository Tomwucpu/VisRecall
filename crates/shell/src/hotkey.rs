//! 全局快捷键：解析配置字符串、在专用线程注册热键并把按键转成 [`ShellEvent`]。
//!
//! 线程模型见 design §3：热键线程只做 `RegisterHotKey` + 消息循环，按下后向
//! app-core 投递 [`ShellEvent::CaptureRequest`]；抓帧与覆盖层由后续任务接管。
//! 注册结果在 [`spawn_hotkey_thread`] 返回前确定，便于上层立刻提示「快捷键被占用」。

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

use crossbeam_channel::{bounded, Sender};
use visrecall_domain::AppError;
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT,
    MOD_SHIFT, MOD_WIN,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetMessageW, PeekMessageW, PostThreadMessageW, MSG, PM_NOREMOVE, WM_HOTKEY, WM_QUIT,
};

use crate::{CaptureTrigger, ShellEvent};

/// 热键 id；线程级热键允许 0x0000–0xBFFF，取 "VR" 的 ASCII 避免与常见值相撞。
const HOTKEY_ID: i32 = 0x5652;

/// 方向键主键。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arrow {
    Left,
    Right,
    Up,
    Down,
}

/// 主键（非修饰键）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    /// `A`–`Z`，`0` 表示 `A`。
    Letter(u8),
    /// `0`–`9`。
    Digit(u8),
    /// `F1`–`F24`，从 `1` 开始。
    Function(u8),
    Space,
    Escape,
    Arrow(Arrow),
}

impl Key {
    /// 虚拟键码（`VK_*`）。
    fn vk(self) -> u32 {
        match self {
            Key::Letter(index) => u32::from(b'A' + index),
            Key::Digit(digit) => u32::from(b'0' + digit),
            // VK_F1 = 0x70。
            Key::Function(number) => 0x70 + u32::from(number - 1),
            Key::Space => 0x20,
            Key::Escape => 0x1B,
            Key::Arrow(Arrow::Left) => 0x25,
            Key::Arrow(Arrow::Up) => 0x26,
            Key::Arrow(Arrow::Right) => 0x27,
            Key::Arrow(Arrow::Down) => 0x28,
        }
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Key::Letter(index) => f.write_str(&char::from(b'A' + index).to_string()),
            Key::Digit(digit) => f.write_str(&char::from(b'0' + digit).to_string()),
            Key::Function(number) => write!(f, "F{number}"),
            Key::Space => f.write_str("Space"),
            Key::Escape => f.write_str("Esc"),
            Key::Arrow(Arrow::Left) => f.write_str("Left"),
            Key::Arrow(Arrow::Right) => f.write_str("Right"),
            Key::Arrow(Arrow::Up) => f.write_str("Up"),
            Key::Arrow(Arrow::Down) => f.write_str("Down"),
        }
    }
}

/// 解析后的快捷键：至少一个修饰键 + 恰好一个主键。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HotkeySpec {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub win: bool,
    pub key: Key,
}

impl HotkeySpec {
    /// 解析形如 `Ctrl+Shift+S` 的配置值；修饰键大小写与别名均接受。
    ///
    /// 失败一律返回 [`AppError::InvalidInput`]，由调用方转成托盘提示。
    pub fn parse(text: &str) -> Result<HotkeySpec, AppError> {
        let mut ctrl = false;
        let mut shift = false;
        let mut alt = false;
        let mut win = false;
        let mut key: Option<Key> = None;

        for segment in text.split('+') {
            let token = segment.trim();
            if token.is_empty() {
                return Err(invalid(text, "empty segment"));
            }

            match parse_modifier(token) {
                Some(Modifier::Ctrl) => ctrl = true,
                Some(Modifier::Shift) => shift = true,
                Some(Modifier::Alt) => alt = true,
                Some(Modifier::Win) => win = true,
                None => {
                    let parsed = parse_key(token)
                        .ok_or_else(|| invalid(text, format!("unknown key {token}")))?;
                    if key.replace(parsed).is_some() {
                        return Err(invalid(text, "more than one main key"));
                    }
                }
            }
        }

        let key = key.ok_or_else(|| invalid(text, "missing main key"))?;
        if !ctrl && !shift && !alt && !win {
            return Err(invalid(text, "at least one modifier is required"));
        }

        Ok(HotkeySpec {
            ctrl,
            shift,
            alt,
            win,
            key,
        })
    }

    /// 规范化输出，修饰键顺序固定为 `Ctrl+Shift+Alt+Win`。
    pub fn format(&self) -> String {
        let mut parts = Vec::with_capacity(5);
        if self.ctrl {
            parts.push("Ctrl".to_string());
        }
        if self.shift {
            parts.push("Shift".to_string());
        }
        if self.alt {
            parts.push("Alt".to_string());
        }
        if self.win {
            parts.push("Win".to_string());
        }
        parts.push(self.key.to_string());
        parts.join("+")
    }

    /// `RegisterHotKey` 的 `(fsModifiers, vk)`；始终带 `MOD_NOREPEAT`，按住不放不重复触发。
    pub fn register_tuple(&self) -> (u32, u32) {
        let mut modifiers = MOD_NOREPEAT.0;
        if self.ctrl {
            modifiers |= MOD_CONTROL.0;
        }
        if self.shift {
            modifiers |= MOD_SHIFT.0;
        }
        if self.alt {
            modifiers |= MOD_ALT.0;
        }
        if self.win {
            modifiers |= MOD_WIN.0;
        }
        (modifiers, self.key.vk())
    }
}

impl fmt::Display for HotkeySpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.format())
    }
}

#[derive(Clone, Copy)]
enum Modifier {
    Ctrl,
    Shift,
    Alt,
    Win,
}

fn parse_modifier(token: &str) -> Option<Modifier> {
    match token.to_ascii_uppercase().as_str() {
        "CTRL" | "CONTROL" => Some(Modifier::Ctrl),
        "SHIFT" => Some(Modifier::Shift),
        "ALT" => Some(Modifier::Alt),
        "WIN" | "META" => Some(Modifier::Win),
        _ => None,
    }
}

fn parse_key(token: &str) -> Option<Key> {
    let upper = token.to_ascii_uppercase();
    let mut chars = upper.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if c.is_ascii_uppercase() => {
            return Some(Key::Letter(c as u8 - b'A'));
        }
        (Some(c), None) if c.is_ascii_digit() => {
            return Some(Key::Digit(c as u8 - b'0'));
        }
        _ => {}
    }

    match upper.as_str() {
        "SPACE" => Some(Key::Space),
        "ESC" | "ESCAPE" => Some(Key::Escape),
        "LEFT" => Some(Key::Arrow(Arrow::Left)),
        "RIGHT" => Some(Key::Arrow(Arrow::Right)),
        "UP" => Some(Key::Arrow(Arrow::Up)),
        "DOWN" => Some(Key::Arrow(Arrow::Down)),
        other => {
            let number = other.strip_prefix('F')?.parse::<u8>().ok()?;
            (1..=24).contains(&number).then_some(Key::Function(number))
        }
    }
}

fn invalid(text: &str, reason: impl fmt::Display) -> AppError {
    AppError::InvalidInput(format!("invalid hotkey {text:?}: {reason}"))
}

/// 已注册热键的句柄；drop 时等价于 [`HotkeyHandle::stop`]。
#[derive(Debug)]
pub struct HotkeyHandle {
    thread_id: u32,
    spec: HotkeySpec,
    stopped: AtomicBool,
    thread: Option<thread::JoinHandle<()>>,
}

impl HotkeyHandle {
    /// 请求热键线程退出；幂等，且不阻塞调用方（不 join）。
    ///
    /// 线程收到 `WM_QUIT` 后先 `UnregisterHotKey` 再结束。
    pub fn stop(&self) {
        if self.stopped.swap(true, Ordering::SeqCst) {
            return;
        }
        // SAFETY: `thread_id` 来自热键线程的 `GetCurrentThreadId`，该线程在退出前
        // 一直拥有消息队列；即使线程已退出，最坏情况只是返回错误。
        match unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) } {
            Ok(()) => tracing::info!(hotkey = %self.spec, "hotkey thread stop requested"),
            Err(err) => tracing::warn!(
                hotkey = %self.spec,
                error = %err,
                "failed to post WM_QUIT to hotkey thread"
            ),
        }
    }

    /// 在后台等待线程注销热键，确保同一组合键可以立即重新注册。
    pub fn stop_and_wait(mut self) {
        self.stop();
        if let Some(thread) = self.thread.take() {
            if thread.join().is_err() {
                tracing::warn!(hotkey = %self.spec, "hotkey thread panicked during shutdown");
            }
        }
    }

    /// 该句柄对应的快捷键。
    pub fn spec(&self) -> HotkeySpec {
        self.spec
    }
}

impl Drop for HotkeyHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// 在专用线程注册全局热键，注册结果确定后才返回。
///
/// 组合键已被其他程序（或本进程其他线程）占用时返回
/// `AppError::InvalidInput("hotkey occupied: ...")`；事件通道关闭后线程自行退出。
pub fn spawn_hotkey_thread(
    spec: HotkeySpec,
    tx: Sender<ShellEvent>,
) -> Result<HotkeyHandle, AppError> {
    spawn_hotkey_thread_with_gate(spec, tx, None)
}

/// 注册热键时共用截图状态，关闭期间收到的按键不进入事件队列。
pub fn spawn_hotkey_thread_with_gate(
    spec: HotkeySpec,
    tx: Sender<ShellEvent>,
    capture_enabled: Option<Arc<AtomicBool>>,
) -> Result<HotkeyHandle, AppError> {
    let (modifiers, vk) = spec.register_tuple();
    let (ready_tx, ready_rx) = bounded::<Result<u32, AppError>>(1);

    let thread = thread::Builder::new()
        .name("visrecall-hotkey".into())
        .spawn(move || hotkey_thread_main(spec, modifiers, vk, tx, ready_tx, capture_enabled))
        .map_err(|err| AppError::Internal(format!("spawn hotkey thread: {err}")))?;

    match ready_rx.recv() {
        Ok(Ok(thread_id)) => Ok(HotkeyHandle {
            thread_id,
            spec,
            stopped: AtomicBool::new(false),
            thread: Some(thread),
        }),
        Ok(Err(err)) => Err(err),
        // 线程在回报注册结果前就消失了，只能是内部错误。
        Err(_) => Err(AppError::Internal(
            "hotkey thread exited before registering".into(),
        )),
    }
}

fn hotkey_thread_main(
    spec: HotkeySpec,
    modifiers: u32,
    vk: u32,
    tx: Sender<ShellEvent>,
    ready: Sender<Result<u32, AppError>>,
    capture_enabled: Option<Arc<AtomicBool>>,
) {
    // SAFETY: 只注册线程级热键，`hwnd = None`。
    if let Err(err) = unsafe { RegisterHotKey(None, HOTKEY_ID, HOT_KEY_MODIFIERS(modifiers), vk) } {
        tracing::error!(hotkey = %spec, error = %err, "failed to register hotkey");
        let _ = ready.send(Err(AppError::InvalidInput(format!(
            "hotkey occupied: {spec} ({err})"
        ))));
        return;
    }

    let thread_id = unsafe { GetCurrentThreadId() };
    // 确保消息队列在回报之前已存在，这样调用方可以立刻 `stop()`。
    let mut msg = MSG::default();
    let _ = unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE) };

    tracing::info!(hotkey = %spec, thread_id, "global hotkey registered");
    let _ = ready.send(Ok(thread_id));

    loop {
        // SAFETY: 消息循环只读取线程自己的队列，`msg` 是栈上有效缓冲。
        let result = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        match result.0 {
            // WM_QUIT。
            0 => break,
            // -1：GetMessageW 出错。
            -1 => {
                tracing::error!("hotkey message loop failed");
                break;
            }
            _ => {}
        }

        if msg.message == WM_HOTKEY
            && capture_enabled
                .as_ref()
                .is_none_or(|enabled| enabled.load(Ordering::SeqCst))
        {
            // 按键本身由 app-core 的「capture requested」统一记录，这里只留排障细节。
            tracing::debug!(hotkey = %spec, "hotkey pressed");
            if tx
                .send(ShellEvent::CaptureRequest(CaptureTrigger::Hotkey))
                .is_err()
            {
                tracing::warn!("shell event channel closed; hotkey thread exiting");
                break;
            }
        }
    }

    // SAFETY: 与注册时相同的 `id`/`hwnd`（线程级）。
    if let Err(err) = unsafe { UnregisterHotKey(None, HOTKEY_ID) } {
        tracing::warn!(hotkey = %spec, error = %err, "failed to unregister hotkey");
    }
    tracing::info!(hotkey = %spec, "hotkey thread stopped");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试用组合键：避开默认的 `Ctrl+Shift+S`，避免与正在运行的应用冲突。
    fn spec(text: &str) -> HotkeySpec {
        HotkeySpec::parse(text).expect("valid test hotkey")
    }

    #[test]
    fn occupied_combination_is_reported_as_invalid_input() {
        let spec = spec("Ctrl+Alt+Shift+F24");
        let (modifiers, vk) = spec.register_tuple();

        // 先用「本线程 + 另一个 id」占住组合键，模拟其他程序已注册。
        unsafe {
            RegisterHotKey(None, 0x5751, HOT_KEY_MODIFIERS(modifiers), vk)
                .expect("occupy hotkey for test");
        }

        let (tx, _rx) = bounded(1);
        let err = spawn_hotkey_thread(spec, tx).expect_err("registration must fail");
        assert!(matches!(err, AppError::InvalidInput(_)));
        assert!(
            err.to_string().contains("hotkey occupied"),
            "unexpected message: {err}"
        );

        unsafe {
            let _ = UnregisterHotKey(None, 0x5751);
        }
    }

    #[test]
    fn registers_and_stop_releases_the_combination() {
        let spec = spec("Ctrl+Alt+Shift+F23");
        let (tx, rx) = bounded(1);

        let handle = spawn_hotkey_thread(spec, tx).expect("register test hotkey");
        assert_eq!(handle.spec(), spec);
        assert!(rx.is_empty(), "no capture request before key press");

        // 幂等：第二次 stop 不再投递消息。
        handle.stop();
        handle.stop();

        // 线程释放组合键后才能再次注册；给它一点调度时间。
        let mut re_registered = None;
        for _ in 0..100 {
            let (tx, _rx) = bounded(1);
            match spawn_hotkey_thread(spec, tx) {
                Ok(next) => {
                    re_registered = Some(next);
                    break;
                }
                Err(_) => thread::sleep(std::time::Duration::from_millis(20)),
            }
        }
        let re_registered = re_registered.expect("combination released after stop");
        re_registered.stop();
    }
}
