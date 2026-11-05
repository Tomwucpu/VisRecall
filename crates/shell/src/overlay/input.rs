//! 覆盖层输入：把 Win32 消息翻译成 [`InputEvent`]，以及提交时的 BGRA 回读拼合。
//!
//! 窗口过程（[`super::overlay_wnd_proc`]）在覆盖层线程上执行，但拿不到覆盖层线程的
//! 可变状态（`SelectionState` 等）。因此它只把输入**翻译成纯数据**塞进线程本地队列
//! [`push`]，覆盖层线程 pump 完消息后用 [`take`] 一次性取走并按序处理。好处是：
//!
//! - 窗口过程里没有对覆盖层状态的借用，不会出现「消息分发期间持有 `&mut self`」；
//! - 一次 pump 内的连续 `WM_MOUSEMOVE` 在入队时合并成一条（只保留最新位置），
//!   避免快速拖动时堆积上百条移动、每条都做一次窗口命中链。
//!
//! 回读拼合：[`compose_bgra`] 把各显示器的相交区域按 `(x, y, w, h, bgra)` 拼进全局
//! 选区尺寸的目标缓冲（top-down BGRA8），未覆盖区域填 0（design §6 的屏间空隙限制）。
//! [`region_offsets`] 负责「全局选区 ∩ 显示器」到「纹理内偏移 / 目标内偏移」的换算。

use std::cell::RefCell;

use visrecall_domain::geometry::{PhysicalPoint, PhysicalRect};
use windows::Win32::Foundation::POINT;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, VIRTUAL_KEY, VK_CONTROL, VK_DOWN, VK_ESCAPE, VK_LEFT, VK_RETURN, VK_RIGHT,
    VK_SHIFT, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

use crate::selection::{Key, Modifiers};

/// 把若干 `(x, y, w, h, bgra)` 区域拼进 `dst_w × dst_h` 的目标缓冲。
///
/// - 目标是 top-down BGRA8，长度恰为 `dst_w * dst_h * 4`；未覆盖区域填 0（透明黑）。
/// - `bgra` 按紧排（每行 `w * 4` 字节）解释；长度不足 `w * h * 4` 的区域整体忽略。
/// - 区域超出目标右/下边界时按目标裁剪，完全在目标外时忽略；宽或高为 0 忽略。
///
/// 纯函数，不触碰 Win32/D3D，便于单测（见 `crates/shell/tests/stitch.rs`）。
pub fn compose_bgra(dst_w: u32, dst_h: u32, regions: &[(u32, u32, u32, u32, Vec<u8>)]) -> Vec<u8> {
    let (dst_w, dst_h) = (dst_w as usize, dst_h as usize);
    let Some(len) = dst_w
        .checked_mul(dst_h)
        .and_then(|pixels| pixels.checked_mul(4))
    else {
        return Vec::new();
    };
    let dst_stride = dst_w * 4;
    let mut dst = vec![0u8; len];

    for (x, y, w, h, bgra) in regions {
        let (x, y) = (*x as usize, *y as usize);
        let (w, h) = (*w as usize, *h as usize);
        if w == 0 || h == 0 {
            continue;
        }
        let Some(src_len) = w.checked_mul(h).and_then(|pixels| pixels.checked_mul(4)) else {
            continue;
        };
        if bgra.len() < src_len {
            continue;
        }
        // 越界部分裁剪：只拷贝落在目标内的行列。
        let copy_w = w.min(dst_w.saturating_sub(x));
        let copy_h = h.min(dst_h.saturating_sub(y));
        if copy_w == 0 || copy_h == 0 {
            continue;
        }
        let src_stride = w * 4;
        for row in 0..copy_h {
            let src = &bgra[row * src_stride..row * src_stride + copy_w * 4];
            let dst_offset = (y + row) * dst_stride + x * 4;
            dst[dst_offset..dst_offset + copy_w * 4].copy_from_slice(src);
        }
    }
    dst
}

/// 全局选区与显示器边界的相交换算。
///
/// 返回 `(dst_x, dst_y, src_x, src_y, width, height)`：`dst_*` 是相交区域在全局选区
/// 内的偏移，`src_*` 是它在显示器（冻结纹理）内的偏移。无交集时返回 `None`。
pub fn region_offsets(
    selection: PhysicalRect,
    display_bounds: PhysicalRect,
) -> Option<(u32, u32, u32, u32, u32, u32)> {
    let intersection = selection.intersect(&display_bounds)?;
    let dst_x = u32::try_from(intersection.x - selection.x).ok()?;
    let dst_y = u32::try_from(intersection.y - selection.y).ok()?;
    let src_x = u32::try_from(intersection.x - display_bounds.x).ok()?;
    let src_y = u32::try_from(intersection.y - display_bounds.y).ok()?;
    Some((
        dst_x,
        dst_y,
        src_x,
        src_y,
        intersection.width,
        intersection.height,
    ))
}

/// 窗口过程 → 覆盖层线程的输入事件（纯数据，可跨消息分发边界）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InputEvent {
    /// `WM_LBUTTONDOWN`；`point` 是 `GetCursorPos` 的全局物理坐标。
    MouseDown { point: PhysicalPoint },
    /// `WM_MOUSEMOVE`；修饰键在窗口过程里读取，保证与消息同一时刻。
    MouseMove {
        point: PhysicalPoint,
        modifiers: Modifiers,
    },
    /// `WM_LBUTTONUP`。
    MouseUp,
    /// 方向键微调（`Shift` 为 10px）。
    Nudge { key: Key, shift: bool },
    /// `Enter`：提交。
    Commit,
    /// `Esc` 或右键：取消。
    Cancel,
    /// `WM_MOUSEWHEEL`：在候选窗口链中切换高亮。
    Wheel {
        point: PhysicalPoint,
        modifiers: Modifiers,
        delta: i32,
    },
}

thread_local! {
    /// 覆盖层线程的消息队列：窗口过程写、pump 结束后取。
    static QUEUE: RefCell<Vec<InputEvent>> = const { RefCell::new(Vec::new()) };
}

/// 入队一条输入事件；连续的 [`InputEvent::MouseMove`] 合并为最新位置。
pub(crate) fn push(event: InputEvent) {
    QUEUE.with(|queue| {
        let mut queue = queue.borrow_mut();
        if matches!(event, InputEvent::MouseMove { .. }) {
            if let Some(last) = queue.last_mut() {
                if matches!(last, InputEvent::MouseMove { .. }) {
                    *last = event;
                    return;
                }
            }
        }
        queue.push(event);
    });
}

/// 取走当前排队的所有输入事件（保持入队顺序）。
pub(crate) fn take() -> Vec<InputEvent> {
    QUEUE.with(|queue| std::mem::take(&mut *queue.borrow_mut()))
}

/// 当前鼠标位置（全局物理像素）；`GetCursorPos` 失败时返回 `None`。
pub(crate) fn cursor_point() -> Option<PhysicalPoint> {
    let mut point = POINT::default();
    // SAFETY: `point` 指向栈上有效 `POINT`。
    match unsafe { GetCursorPos(&mut point) } {
        Ok(()) => Some(PhysicalPoint {
            x: point.x,
            y: point.y,
        }),
        Err(_) => None,
    }
}

/// 当前 `Ctrl`/`Shift` 状态。
///
/// `GetKeyState` 读取的是调用线程输入队列里的键状态，窗口过程在分发消息时读取，
/// 与消息同一时刻；覆盖层线程随后处理事件，误差不超过一个 pump 周期。
pub(crate) fn current_modifiers() -> Modifiers {
    Modifiers {
        shift: key_down(VK_SHIFT),
        ctrl: key_down(VK_CONTROL),
    }
}

/// `WM_KEYDOWN` 的虚拟键码 → 输入事件；覆盖层不关心的键返回 `None`。
pub(crate) fn event_for_key(vk: u16, shift: bool) -> Option<InputEvent> {
    let event = match vk {
        v if v == VK_ESCAPE.0 => InputEvent::Cancel,
        v if v == VK_RETURN.0 => InputEvent::Commit,
        v if v == VK_LEFT.0 => InputEvent::Nudge {
            key: Key::Left,
            shift,
        },
        v if v == VK_RIGHT.0 => InputEvent::Nudge {
            key: Key::Right,
            shift,
        },
        v if v == VK_UP.0 => InputEvent::Nudge {
            key: Key::Up,
            shift,
        },
        v if v == VK_DOWN.0 => InputEvent::Nudge {
            key: Key::Down,
            shift,
        },
        _ => return None,
    };
    Some(event)
}

/// `WM_MOUSEWHEEL` 的 `wParam` 高 16 位是带符号增量（通常 ±120）。
pub(crate) fn wheel_delta(wparam: usize) -> i32 {
    ((wparam >> 16) as u16 as i16) as i32
}

/// 虚拟键当前是否按下（`GetKeyState` 高位为 1 表示按下）。
fn key_down(vk: VIRTUAL_KEY) -> bool {
    // SAFETY: 纯查询，无指针参数。`i16` 为负即高位为 1（按下）。
    (unsafe { GetKeyState(vk.0 as i32) }) < 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wheel_delta_is_signed_high_word() {
        // 上滚：+120 << 16；下滚：-120 的补码。
        assert_eq!(wheel_delta(120usize << 16), 120);
        assert_eq!(wheel_delta((-120i32 as u32 as usize) << 16), -120);
        assert_eq!(wheel_delta(0), 0);
        // 低 16 位是按键状态，必须被忽略。
        assert_eq!(wheel_delta((120usize << 16) | 0x0001), 120);
    }

    #[test]
    fn key_mapping_covers_overlay_keys() {
        assert_eq!(event_for_key(VK_ESCAPE.0, false), Some(InputEvent::Cancel));
        assert_eq!(event_for_key(VK_RETURN.0, false), Some(InputEvent::Commit));
        assert_eq!(
            event_for_key(VK_LEFT.0, true),
            Some(InputEvent::Nudge {
                key: Key::Left,
                shift: true
            })
        );
        assert_eq!(
            event_for_key(VK_DOWN.0, false),
            Some(InputEvent::Nudge {
                key: Key::Down,
                shift: false
            })
        );
        // 其他键忽略（例如字母键、Tab）。
        assert_eq!(event_for_key(0x41, false), None);
        assert_eq!(event_for_key(0x09, false), None);
    }

    #[test]
    fn mouse_move_queue_coalesces_only_consecutive_moves() {
        // 线程本地队列只在本测试线程使用；先清空避免与其他用例互相干扰。
        take();
        let m = Modifiers::default();
        push(InputEvent::MouseMove {
            point: PhysicalPoint { x: 1, y: 1 },
            modifiers: m,
        });
        push(InputEvent::MouseMove {
            point: PhysicalPoint { x: 2, y: 2 },
            modifiers: m,
        });
        push(InputEvent::MouseUp);
        push(InputEvent::MouseMove {
            point: PhysicalPoint { x: 3, y: 3 },
            modifiers: m,
        });

        let events = take();
        assert_eq!(
            events,
            vec![
                InputEvent::MouseMove {
                    point: PhysicalPoint { x: 2, y: 2 },
                    modifiers: m,
                },
                InputEvent::MouseUp,
                InputEvent::MouseMove {
                    point: PhysicalPoint { x: 3, y: 3 },
                    modifiers: m,
                },
            ]
        );
        assert!(take().is_empty());
    }
}
