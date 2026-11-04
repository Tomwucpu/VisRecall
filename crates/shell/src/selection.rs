//! 覆盖层选区状态机（纯逻辑）。
//!
//! 本模块不触碰任何 Win32/D3D 调用：输入事件由覆盖层窗口过程翻译成这里的
//! 纯函数调用，几何计算全部使用虚拟屏幕物理像素（见 design §6）。
//!
//! 状态流转（design §8）：
//!
//! ```text
//! Idle --move over window--> HoverWindow --mouse down (empty)--> Dragging
//! HoverWindow --mouse down (handle)--> Adjusting
//! Dragging/Adjusting --mouse up--> HoverWindow（保留 rect，等待 Enter）
//! 任意阶段 --Esc/右键--> Cancelled ; HoverWindow --Enter--> Committed
//! ```
//!
//! 关键取舍：
//! - 抬起鼠标后不直接提交，保留选区等待 `Enter`；`HoverWindow` 的悬停高亮
//!   只在 `rect` 为空（尚未框选）时生效，避免冲掉刚拖出来的选区。
//! - 拖拽/调整时在 8 物理 px 内吸附候选窗口边；`Ctrl` 关闭吸附；方向键微调后不吸附。
//! - 滚轮只在候选链中切换高亮索引，同样不覆盖已有选区。

use visrecall_capture::virtual_bounds;
use visrecall_domain::capture::DisplayInfo;
use visrecall_domain::geometry::{PhysicalPoint, PhysicalRect};

/// 最小可提交选区边长（物理像素，design §6）。
pub const MIN_SELECTION_SIZE: u32 = 10;

/// 吸附阈值（物理像素，design §8）。
pub const SNAP_THRESHOLD: i32 = 8;

/// 控制点命中半径的 96 DPI 设计值（物理像素，design §6）。
const HANDLE_RADIUS_AT_96_DPI: u64 = 6;

/// 选区所处阶段。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionPhase {
    /// 尚未进入覆盖层交互。
    Idle,
    /// 悬停在候选窗口上（高亮候选边界）。
    HoverWindow,
    /// 从空白处按下并拖拽框选。
    Dragging,
    /// 拖拽已有选区的控制点或整体移动。
    Adjusting,
    /// 已提交（`commit` 成功）。
    Committed,
    /// 已取消（`Esc`/右键/无效提交）。
    Cancelled,
}

/// 选区控制点与整体移动手柄。方位顺序与 design §8 一致。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handle {
    NorthWest,
    North,
    NorthEast,
    East,
    SouthEast,
    South,
    SouthWest,
    West,
    /// 在选区内部按下，整体平移。
    Move,
}

impl Handle {
    /// 该手柄是否拖动左边界。
    fn adjusts_west(self) -> bool {
        matches!(self, Self::NorthWest | Self::West | Self::SouthWest)
    }

    /// 该手柄是否拖动右边界。
    fn adjusts_east(self) -> bool {
        matches!(self, Self::NorthEast | Self::East | Self::SouthEast)
    }

    /// 该手柄是否拖动上边界。
    fn adjusts_north(self) -> bool {
        matches!(self, Self::NorthWest | Self::North | Self::NorthEast)
    }

    /// 该手柄是否拖动下边界。
    fn adjusts_south(self) -> bool {
        matches!(self, Self::SouthWest | Self::South | Self::SouthEast)
    }
}

/// 键盘修饰键状态；本轮只关心 Shift（大步微调）与 Ctrl（关闭吸附）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub shift: bool,
    pub ctrl: bool,
}

/// 覆盖层关心的按键（不含字符键）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Left,
    Right,
    Up,
    Down,
    Enter,
    Escape,
}

/// 共享给所有显示器覆盖层的选区状态。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectionState {
    pub phase: SelectionPhase,
    /// 本次拖拽/调整的起点（`Dragging` 时为框选起点）。
    pub anchor: PhysicalPoint,
    /// `Adjusting` 开始时 `rect` 的快照；`Dragging` 与 `Move` 都以此为基准。
    pub start_rect: PhysicalRect,
    /// 当前选区（半开区间）。
    pub rect: PhysicalRect,
    /// 正在拖拽的手柄；`None` 表示没有在调整。
    pub active: Option<Handle>,
    /// `on_mouse_move` 记录的候选窗口链，供滚轮切换。
    hover_candidates: Vec<PhysicalRect>,
    /// 滚轮当前指向的候选索引。
    hover_index: usize,
}

impl Default for SelectionState {
    fn default() -> Self {
        Self::new()
    }
}

impl SelectionState {
    /// 初始态：`Idle`、空选区、无手柄。
    pub fn new() -> Self {
        Self {
            phase: SelectionPhase::Idle,
            anchor: PhysicalPoint::default(),
            start_rect: PhysicalRect::default(),
            rect: PhysicalRect::default(),
            active: None,
            hover_candidates: Vec::new(),
            hover_index: 0,
        }
    }

    /// 鼠标按下。`handle` 由 [`hit_test_handle`] 得出：
    /// - `Some(Handle::Move)`：进入 `Adjusting` 并整体平移；
    /// - 其他 `Some(_)`：进入 `Adjusting` 并拖动该边/角；
    /// - `None`：从空白处开始新框选，`rect` 收缩为 `p` 处的空矩形。
    ///
    /// 三种情况的 `anchor`/`start_rect` 都重新记录，后续 `on_mouse_move` 以此计算增量。
    pub fn on_mouse_down(&mut self, p: PhysicalPoint, handle: Option<Handle>) {
        self.anchor = p;
        self.start_rect = self.rect;
        match handle {
            Some(h) => {
                self.phase = SelectionPhase::Adjusting;
                self.active = Some(h);
            }
            None => {
                self.phase = SelectionPhase::Dragging;
                self.active = None;
                self.rect = PhysicalRect::from_points(p, p);
            }
        }
    }

    /// 鼠标移动。拖拽/调整时更新 `rect` 并（未按 `Ctrl` 时）吸附候选窗口边；
    /// 空闲或悬停且尚无选区时，用包含 `p` 的候选窗口作为高亮。
    pub fn on_mouse_move(&mut self, p: PhysicalPoint, m: Modifiers, candidates: &[PhysicalRect]) {
        self.hover_candidates.clear();
        self.hover_candidates.extend_from_slice(candidates);
        if self.hover_candidates.is_empty() {
            self.hover_index = 0;
        } else {
            self.hover_index = self.hover_index.min(self.hover_candidates.len() - 1);
        }

        match self.phase {
            SelectionPhase::Dragging => {
                let mut rect = PhysicalRect::from_points(self.anchor, p);
                if !m.ctrl {
                    rect = snap_rect(rect, candidates, SNAP_THRESHOLD);
                }
                self.rect = rect;
            }
            SelectionPhase::Adjusting => {
                let delta = PhysicalPoint {
                    x: p.x.saturating_sub(self.anchor.x),
                    y: p.y.saturating_sub(self.anchor.y),
                };
                match self.active {
                    Some(handle) => {
                        let mut rect =
                            drag_rect(handle, self.start_rect, delta, MIN_SELECTION_SIZE);
                        if !m.ctrl {
                            rect = snap_rect(rect, candidates, SNAP_THRESHOLD);
                        }
                        self.rect = rect;
                    }
                    // 理论上不可达；退化为重新框选，避免状态卡死。
                    None => {
                        self.phase = SelectionPhase::Dragging;
                        self.rect = PhysicalRect::from_points(self.anchor, p);
                    }
                }
            }
            SelectionPhase::Idle | SelectionPhase::HoverWindow => {
                if !self.rect.is_empty() {
                    // 已有选区：悬停高亮不得覆盖它（等待 Enter 提交）。
                    return;
                }
                match candidates.iter().position(|c| c.contains(p)) {
                    Some(index) => {
                        self.hover_index = index;
                        self.rect = candidates[index];
                        self.phase = SelectionPhase::HoverWindow;
                    }
                    None => {
                        // 指针在候选之外的空白处：清空高亮；只要还有候选链就保持
                        // `HoverWindow`，以便滚轮继续循环。
                        self.rect = PhysicalRect::default();
                        if !candidates.is_empty() {
                            self.phase = SelectionPhase::HoverWindow;
                        }
                    }
                }
            }
            SelectionPhase::Committed | SelectionPhase::Cancelled => {}
        }
    }

    /// 结束拖拽/调整。不清空 `rect`，而是回到 `HoverWindow` 等待 `Enter` 提交。
    pub fn on_mouse_up(&mut self) {
        if matches!(
            self.phase,
            SelectionPhase::Dragging | SelectionPhase::Adjusting
        ) {
            self.phase = SelectionPhase::HoverWindow;
            self.active = None;
        }
    }

    /// 键盘输入。`Esc` 取消、`Enter` 提交；方向键以 1px（`Shift` 为 10px）平移整个选区。
    /// 微调后不做吸附。`candidates` 预留给后续边界裁剪，当前不强制使用。
    pub fn on_key(&mut self, key: Key, shift: bool, candidates: &[PhysicalRect]) {
        let _ = candidates;
        match key {
            Key::Escape => self.cancel(),
            Key::Enter => {
                self.commit();
            }
            Key::Left | Key::Right | Key::Up | Key::Down => {
                if self.rect.is_empty()
                    || matches!(
                        self.phase,
                        SelectionPhase::Committed | SelectionPhase::Cancelled
                    )
                {
                    return;
                }
                let step = if shift { 10 } else { 1 };
                let (dx, dy) = match key {
                    Key::Left => (-step, 0),
                    Key::Right => (step, 0),
                    Key::Up => (0, -step),
                    Key::Down => (0, step),
                    _ => (0, 0),
                };
                self.rect.x = self.rect.x.saturating_add(dx);
                self.rect.y = self.rect.y.saturating_add(dy);
            }
        }
    }

    /// 滚轮在候选窗口链中循环切换高亮。无候选时 no-op；
    /// 只有 `HoverWindow` 且尚无选区时才把 `rect` 换成当前候选，避免覆盖已有选区。
    pub fn on_wheel(&mut self, delta: i32) {
        if delta == 0 || self.hover_candidates.is_empty() {
            return;
        }
        let len = self.hover_candidates.len() as i64;
        let step = if delta > 0 { 1 } else { -1 };
        self.hover_index = ((self.hover_index as i64 + step).rem_euclid(len)) as usize;

        if matches!(
            self.phase,
            SelectionPhase::Idle | SelectionPhase::HoverWindow
        ) && self.rect.is_empty()
        {
            self.rect = self.hover_candidates[self.hover_index];
            self.phase = SelectionPhase::HoverWindow;
        }
    }

    /// 提交选区：宽高都 ≥ [`MIN_SELECTION_SIZE`] 时进入 `Committed` 并返回成功矩形，
    /// 否则进入 `Cancelled` 并返回 `None`。已取消（`Esc`/右键）的选区不可再提交。
    pub fn commit(&mut self) -> Option<PhysicalRect> {
        let valid = self.phase != SelectionPhase::Cancelled
            && self.rect.width >= MIN_SELECTION_SIZE
            && self.rect.height >= MIN_SELECTION_SIZE;
        if valid {
            self.phase = SelectionPhase::Committed;
            self.active = None;
            Some(self.rect)
        } else {
            self.phase = SelectionPhase::Cancelled;
            self.active = None;
            None
        }
    }

    /// 取消选区：进入 `Cancelled` 并清空活动手柄。
    pub fn cancel(&mut self) {
        self.phase = SelectionPhase::Cancelled;
        self.active = None;
    }
}

/// 命中测试：返回 `p` 命中的控制点；选区内部返回 [`Handle::Move`]；外部返回 `None`。
///
/// 控制点命中区是以手柄为中心、半径 `6×dpi/96`（至少 1px）的正方形，
/// 因此紧贴边界的点优先算作缩放而不是移动。空选区没有手柄。
pub fn hit_test_handle(rect: PhysicalRect, p: PhysicalPoint, dpi: u32) -> Option<Handle> {
    if rect.is_empty() {
        return None;
    }
    let radius = handle_radius(dpi);
    let px = p.x as i64;
    let py = p.y as i64;

    let mut best: Option<(i64, Handle)> = None;
    for (handle, hx, hy) in handle_points(rect) {
        let dx = (px - hx).abs();
        let dy = (py - hy).abs();
        if dx > radius || dy > radius {
            continue;
        }
        let d2 = dx * dx + dy * dy;
        // 角点排在边中点前面：距离相同时角点胜出。
        if best.is_none_or(|(best_d2, _)| d2 < best_d2) {
            best = Some((d2, handle));
        }
    }
    if let Some((_, handle)) = best {
        return Some(handle);
    }
    if rect.contains(p) {
        Some(Handle::Move)
    } else {
        None
    }
}

/// 按手柄拖拽 `start` 得到新矩形。
///
/// - [`Handle::Move`] 整体平移，尺寸不变；
/// - 边手柄固定对边、角手柄固定对角；
/// - 宽高不小于 `min`，也不允许反向穿过对边（对边被钳住而不是交换）。
///
/// 内部全部用 `i64` 运算，`i32::MIN/MAX` 附近的增量不会溢出。
pub fn drag_rect(
    handle: Handle,
    start: PhysicalRect,
    delta: PhysicalPoint,
    min: u32,
) -> PhysicalRect {
    let x0 = start.x as i64;
    let y0 = start.y as i64;
    let x1 = x0 + start.width as i64;
    let y1 = y0 + start.height as i64;
    let dx = delta.x as i64;
    let dy = delta.y as i64;

    if handle == Handle::Move {
        return rect_from_edges(x0 + dx, y0 + dy, x1 + dx, y1 + dy);
    }

    let min = min as i64;
    let mut nx0 = x0;
    let mut nx1 = x1;
    let mut ny0 = y0;
    let mut ny1 = y1;
    if handle.adjusts_west() {
        nx0 = (x0 + dx).min(x1 - min);
    }
    if handle.adjusts_east() {
        nx1 = (x1 + dx).max(x0 + min);
    }
    if handle.adjusts_north() {
        ny0 = (y0 + dy).min(y1 - min);
    }
    if handle.adjusts_south() {
        ny1 = (y1 + dy).max(y0 + min);
    }
    rect_from_edges(nx0, ny0, nx1, ny1)
}

/// 把选区四条边分别吸附到最近的候选窗口边。
///
/// 距离 ≤ `threshold` 才对齐；若某个方向吸附后宽/高小于 [`MIN_SELECTION_SIZE`]，
/// 放弃该方向的两条边吸附，保持原值。空候选或 `threshold <= 0` 原样返回。
pub fn snap_rect(rect: PhysicalRect, candidates: &[PhysicalRect], threshold: i32) -> PhysicalRect {
    if threshold <= 0 || candidates.is_empty() {
        return rect;
    }
    let threshold = threshold as i64;
    let x0 = rect.x as i64;
    let y0 = rect.y as i64;
    let x1 = x0 + rect.width as i64;
    let y1 = y0 + rect.height as i64;

    let mut xs = Vec::with_capacity(candidates.len() * 2);
    let mut ys = Vec::with_capacity(candidates.len() * 2);
    for c in candidates {
        xs.push(c.x as i64);
        xs.push(c.x as i64 + c.width as i64);
        ys.push(c.y as i64);
        ys.push(c.y as i64 + c.height as i64);
    }

    let sx0 = nearest_edge(x0, &xs, threshold).unwrap_or(x0);
    let sx1 = nearest_edge(x1, &xs, threshold).unwrap_or(x1);
    let sy0 = nearest_edge(y0, &ys, threshold).unwrap_or(y0);
    let sy1 = nearest_edge(y1, &ys, threshold).unwrap_or(y1);

    let min = MIN_SELECTION_SIZE as i64;
    let (fx0, fx1) = if sx1 - sx0 >= min {
        (sx0, sx1)
    } else {
        (x0, x1)
    };
    let (fy0, fy1) = if sy1 - sy0 >= min {
        (sy0, sy1)
    } else {
        (y0, y1)
    };
    rect_from_edges(fx0, fy0, fx1, fy1)
}

/// 把选区裁剪到虚拟屏幕（所有显示器 `bounds` 的并集）范围内。
///
/// 阶段 01 允许选区跨屏，因此只在虚拟屏幕整体范围外裁剪，不按单显示器裁剪。
/// 空 `displays` 原样返回；完全落在屏幕外时返回原位置上的零尺寸矩形。
pub fn clamp_to_virtual_screen(rect: PhysicalRect, displays: &[DisplayInfo]) -> PhysicalRect {
    if displays.is_empty() {
        return rect;
    }
    let bounds = virtual_bounds(displays);
    match rect.intersect(&bounds) {
        Some(clipped) => clipped,
        None => PhysicalRect::new(rect.x, rect.y, 0, 0),
    }
}

/// 控制点命中半径：96 DPI 下 6px，按 `dpi/96` 缩放，至少 1px。
fn handle_radius(dpi: u32) -> i64 {
    let scaled = HANDLE_RADIUS_AT_96_DPI * dpi as u64 / 96;
    scaled.max(1) as i64
}

/// 八个手柄的物理像素坐标：先角点后边中点。
fn handle_points(rect: PhysicalRect) -> [(Handle, i64, i64); 8] {
    let x0 = rect.x as i64;
    let y0 = rect.y as i64;
    let x1 = x0 + rect.width as i64;
    let y1 = y0 + rect.height as i64;
    let mx = x0 + (x1 - x0) / 2;
    let my = y0 + (y1 - y0) / 2;
    [
        (Handle::NorthWest, x0, y0),
        (Handle::NorthEast, x1, y0),
        (Handle::SouthEast, x1, y1),
        (Handle::SouthWest, x0, y1),
        (Handle::North, mx, y0),
        (Handle::East, x1, my),
        (Handle::South, mx, y1),
        (Handle::West, x0, my),
    ]
}

/// 在候选边中找与 `value` 距离 ≤ `threshold` 的最近一条。
fn nearest_edge(value: i64, edges: &[i64], threshold: i64) -> Option<i64> {
    edges
        .iter()
        .filter(|edge| (**edge - value).abs() <= threshold)
        .min_by_key(|edge| (**edge - value).abs())
        .copied()
}

/// 由 `i64` 边构造矩形：坐标饱和到 `i32`，宽高饱和到 `[0, u32::MAX]`。
fn rect_from_edges(x0: i64, y0: i64, x1: i64, y1: i64) -> PhysicalRect {
    let x0 = x0.clamp(i32::MIN as i64, i32::MAX as i64);
    let y0 = y0.clamp(i32::MIN as i64, i32::MAX as i64);
    let x1 = x1.clamp(i32::MIN as i64, i32::MAX as i64);
    let y1 = y1.clamp(i32::MIN as i64, i32::MAX as i64);
    PhysicalRect::new(
        x0 as i32,
        y0 as i32,
        (x1 - x0).clamp(0, u32::MAX as i64) as u32,
        (y1 - y0).clamp(0, u32::MAX as i64) as u32,
    )
}
