//! 覆盖层选区状态机（纯逻辑）。
//!
//! 本模块不触碰任何 Win32/D3D 调用：输入事件由覆盖层窗口过程翻译成这里的
//! 纯函数调用，几何计算全部使用虚拟屏幕物理像素（见 design §6）。
//!
//! 状态流转（design §8）：
//!
//! ```text
//! Idle --指针移到候选窗口--> HoverWindow
//! HoverWindow --按下(空白)--> Dragging --抬起--> HoverWindow
//! HoverWindow --按下(手柄)--> Adjusting --抬起--> HoverWindow
//! HoverWindow --Enter（有效）--> Committed
//! 任意阶段 --Esc/右键--> Cancelled
//! ```
//!
//! 悬停高亮与用户选区是两套独立状态（design §8 的 `rect` / `hover_rect`）：
//!
//! - `rect` 只由用户拖拽/调整、方向键微调，以及“单击采用悬停窗口边界”写入；
//! - `hover_rect` 只由 [`SelectionState::on_mouse_move`] 与 [`SelectionState::on_wheel`]
//!   写入。`HoverWindow` 下的鼠标移动**绝不**覆盖 `rect`，所以指针从窗口 A 移到窗口 B
//!   时高亮、标签与“待提交的悬停矩形”都会跟着更新。
//!
//! 其他取舍：
//!
//! - 抬起鼠标后不直接提交，保留选区等待 `Enter`；只有“按下/抬起之间没有任何位移”
//!   的单击才把 `rect` 换成 `hover_rect`（私有 `dragged` 标记记录是否真的移动过，
//!   因此“拖出去又拖回起点”的零面积拖拽不会被误判为单击）。
//! - 吸附按拖动方式分成三种变体：`Move` 只做整框平移（宽高不变），边/角手柄只吸附
//!   被拖动的那条边（固定边不动），框选只吸附随指针移动的边（`anchor` 那一侧不动）。
//!   `Ctrl` 关闭吸附；方向键微调后不吸附。

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
    /// 本次拖拽/调整的起点（`Dragging` 时为框选起点，吸附时保持不动）。
    pub anchor: PhysicalPoint,
    /// `Adjusting` 开始时 `rect` 的快照；方向键微调后同步为新位置。
    pub start_rect: PhysicalRect,
    /// 用户拖拽/调整得到的选区（半开区间），不含悬停高亮。
    pub rect: PhysicalRect,
    /// 当前悬停命中的窗口候选，仅用于高亮与“单击采用”。
    pub hover_rect: PhysicalRect,
    /// 正在拖拽的手柄；`None` 表示没有在调整。
    pub active: Option<Handle>,
    /// `on_mouse_move` 记录的候选窗口链，供滚轮循环切换。
    candidates: Vec<PhysicalRect>,
    /// 滚轮当前指向的候选索引。
    hover_index: usize,
    /// 本次按下之后是否发生过位移。`false` 表示“单击”（按下→抬起之间没有移动），
    /// 只有这种情况 `on_mouse_up` 才把 `hover_rect` 采用为 `rect`。
    dragged: bool,
}

impl Default for SelectionState {
    fn default() -> Self {
        Self::new()
    }
}

impl SelectionState {
    /// 初始态：`Idle`、空选区、空悬停高亮、无手柄。
    pub fn new() -> Self {
        Self {
            phase: SelectionPhase::Idle,
            anchor: PhysicalPoint::default(),
            start_rect: PhysicalRect::default(),
            rect: PhysicalRect::default(),
            hover_rect: PhysicalRect::default(),
            active: None,
            candidates: Vec::new(),
            hover_index: 0,
            dragged: false,
        }
    }

    /// 鼠标按下。`handle` 由 [`hit_test_handle`] 得出：
    /// - `Some(Handle::Move)`：进入 `Adjusting` 并整体平移；
    /// - 其他 `Some(_)`：进入 `Adjusting` 并拖动该边/角；
    /// - `None`：从空白处开始新框选，`rect` 收缩为 `p` 处的空矩形。
    ///
    /// 三种情况的 `anchor`/`start_rect` 都重新记录，后续 `on_mouse_move` 以此计算增量。
    /// `hover_rect` 不动：单击（按下抬起间无位移）时 `on_mouse_up` 才会采用它。
    /// 每次按下都会清空 `dragged` 标记。
    pub fn on_mouse_down(&mut self, p: PhysicalPoint, handle: Option<Handle>) {
        self.anchor = p;
        self.start_rect = self.rect;
        self.dragged = false;
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

    /// 鼠标移动。
    ///
    /// - `Dragging`：`rect` 由 `anchor` 到 `p` 决定，未按 `Ctrl` 时只吸附随指针移动的边；
    /// - `Adjusting`：按手柄调整 `start_rect`，未按 `Ctrl` 时按手柄类型吸附
    ///   （`Move` 整框平移吸附，边/角手柄只吸附被拖动的边）；
    /// - `Idle | HoverWindow`：**只**更新 `hover_rect`（包含 `p` 的候选），绝不覆盖 `rect`；
    ///   没有候选或点在候选之外时把 `hover_rect` 置空。
    pub fn on_mouse_move(&mut self, p: PhysicalPoint, m: Modifiers, candidates: &[PhysicalRect]) {
        self.candidates.clear();
        self.candidates.extend_from_slice(candidates);
        self.hover_index = if self.candidates.is_empty() {
            0
        } else {
            self.hover_index.min(self.candidates.len() - 1)
        };

        match self.phase {
            SelectionPhase::Dragging => {
                if p != self.anchor {
                    self.dragged = true;
                }
                let mut rect = PhysicalRect::from_points(self.anchor, p);
                if !m.ctrl {
                    rect = snap_drag_rect(self.anchor, rect, candidates, SNAP_THRESHOLD);
                }
                self.rect = rect;
            }
            SelectionPhase::Adjusting => {
                if p != self.anchor {
                    self.dragged = true;
                }
                let delta = PhysicalPoint {
                    x: p.x.saturating_sub(self.anchor.x),
                    y: p.y.saturating_sub(self.anchor.y),
                };
                match self.active {
                    Some(handle) => {
                        let mut rect =
                            drag_rect(handle, self.start_rect, delta, MIN_SELECTION_SIZE);
                        if !m.ctrl {
                            rect = snap_resize_rect(handle, rect, candidates, SNAP_THRESHOLD);
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
                match candidates.iter().position(|c| c.contains(p)) {
                    Some(index) => {
                        self.hover_index = index;
                        self.hover_rect = candidates[index];
                        self.phase = SelectionPhase::HoverWindow;
                    }
                    None => {
                        // 指针在候选之外的空白处：清空悬停高亮。仍保留候选链，
                        // 以便滚轮可以继续循环（也保留已有 `rect` 等待 Enter）。
                        self.hover_rect = PhysicalRect::default();
                        if !self.rect.is_empty() || !candidates.is_empty() {
                            self.phase = SelectionPhase::HoverWindow;
                        } else {
                            self.phase = SelectionPhase::Idle;
                        }
                    }
                }
            }
            SelectionPhase::Committed | SelectionPhase::Cancelled => {}
        }
    }

    /// 结束拖拽/调整，回到 `HoverWindow` 等待 `Enter`。
    ///
    /// `Dragging` 期间没有任何位移（单击）且存在悬停高亮时，把 `rect` 换成 `hover_rect`，
    /// 即“单击采用窗口边界”；拖动过（哪怕最终回到起点、面积为 0）则原样保留用户选区，
    /// 避免零宽/零高拖拽被误判为单击。
    pub fn on_mouse_up(&mut self) {
        match self.phase {
            SelectionPhase::Dragging => {
                if !self.dragged && self.rect.is_empty() && !self.hover_rect.is_empty() {
                    self.rect = self.hover_rect;
                }
                self.phase = SelectionPhase::HoverWindow;
                self.active = None;
            }
            SelectionPhase::Adjusting => {
                self.phase = SelectionPhase::HoverWindow;
                self.active = None;
            }
            _ => {}
        }
    }

    /// 键盘输入。`Esc` 取消、`Enter` 提交；方向键以 1px（`Shift` 为 10px）平移整个选区，
    /// 微调后不做吸附。
    ///
    /// 方向键微调会结束正在进行的拖拽/调整（`Dragging`/`Adjusting` → `HoverWindow`，
    /// `active = None`、`start_rect = rect`），否则下一帧 `on_mouse_move` 会按旧
    /// `anchor`/`start_rect` 把微调结果覆盖掉。
    ///
    /// `_candidates` 目前未使用（保留给出屏边界裁剪，需要时再接入），
    /// 方向键不会把选区拉回候选边或屏幕内。
    pub fn on_key(&mut self, key: Key, shift: bool, _candidates: &[PhysicalRect]) {
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
                // 微调后把基准同步到新位置并结束当前拖拽/调整：否则下一帧
                // `on_mouse_move` 会用旧 `start_rect`/旧 `anchor` 把微调覆盖掉。
                self.start_rect = self.rect;
                self.active = None;
                if matches!(
                    self.phase,
                    SelectionPhase::Dragging | SelectionPhase::Adjusting
                ) {
                    self.phase = SelectionPhase::HoverWindow;
                }
            }
        }
    }

    /// 滚轮在候选窗口链中循环切换，同步 `hover_index` 与 `hover_rect`。
    ///
    /// 只在 `Idle | HoverWindow` 处理：`Dragging | Adjusting` 期间滚轮被完全忽略，
    /// 否则把 `phase` 打回 `HoverWindow` 会让后续 `on_mouse_move` 不再按拖拽/调整更新。
    /// 不要求 `rect` 为空、也不改变用户 `rect`；无候选或 `delta == 0` 时 no-op。
    /// `Committed | Cancelled` 阶段同样忽略滚轮。
    pub fn on_wheel(&mut self, delta: i32) {
        if delta == 0 || self.candidates.is_empty() {
            return;
        }
        if !matches!(
            self.phase,
            SelectionPhase::Idle | SelectionPhase::HoverWindow
        ) {
            return;
        }
        let len = self.candidates.len() as i64;
        let step = if delta > 0 { 1 } else { -1 };
        self.hover_index = ((self.hover_index as i64 + step).rem_euclid(len)) as usize;
        self.hover_rect = self.candidates[self.hover_index];
        self.phase = SelectionPhase::HoverWindow;
    }

    /// 当前悬停高亮的窗口矩形；没有悬停命中时为 `None`（供覆盖层画高亮与标签）。
    pub fn hovered_rect(&self) -> Option<PhysicalRect> {
        if self.hover_rect.is_empty() {
            None
        } else {
            Some(self.hover_rect)
        }
    }

    /// 提交选区：宽高都 ≥ [`MIN_SELECTION_SIZE`] 时进入 `Committed` 并返回成功矩形，
    /// 否则按 [`SelectionState::cancel`] 的语义清空选区与悬停高亮后返回 `None`。
    /// 已取消（`Esc`/右键）的选区不可再提交。
    ///
    /// 失败分支必须清空 `hover_rect`/候选链：否则随后的“空白点击 + 抬起”会把陈旧的
    /// 悬停窗口当成单击目标采用为 `rect`。
    pub fn commit(&mut self) -> Option<PhysicalRect> {
        let valid = self.phase != SelectionPhase::Cancelled
            && self.rect.width >= MIN_SELECTION_SIZE
            && self.rect.height >= MIN_SELECTION_SIZE;
        if valid {
            self.phase = SelectionPhase::Committed;
            self.active = None;
            Some(self.rect)
        } else {
            self.cancel();
            None
        }
    }

    /// 取消选区：进入 `Cancelled`，清空选区、悬停高亮、候选链与活动手柄。
    ///
    /// 清空 `rect`/`hover_rect` 是为了复用实例时旧选区不会挡住后续 hover 高亮。
    pub fn cancel(&mut self) {
        self.phase = SelectionPhase::Cancelled;
        self.rect = PhysicalRect::default();
        self.hover_rect = PhysicalRect::default();
        self.candidates.clear();
        self.hover_index = 0;
        self.active = None;
        self.dragged = false;
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
///
/// 注意：这是“四条边都参与”的通用版本，状态机不会直接用它——拖拽/调整/移动分别走
/// [`snap_drag_rect`] / [`snap_resize_rect`] / [`snap_move_rect`]，避免固定边被移动。
pub fn snap_rect(rect: PhysicalRect, candidates: &[PhysicalRect], threshold: i32) -> PhysicalRect {
    if threshold <= 0 || candidates.is_empty() {
        return rect;
    }
    let threshold = threshold as i64;
    let (x0, y0, x1, y1) = edges(rect);
    let (xs, ys) = candidate_edges(candidates);

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

/// `Move` 手柄的整框平移吸附：只平移，宽高不变。
///
/// 每个轴向上分别比较“左/右边对候选竖直边”“上/下边对候选水平边”的距离，
/// 取阈值内最近的一个偏移量整体平移该轴，因此吸附永远不改变宽高。
pub(crate) fn snap_move_rect(
    rect: PhysicalRect,
    candidates: &[PhysicalRect],
    threshold: i32,
) -> PhysicalRect {
    if threshold <= 0 || candidates.is_empty() {
        return rect;
    }
    let threshold = threshold as i64;
    let (xs, ys) = candidate_edges(candidates);
    let (x0, y0, x1, y1) = edges(rect);
    let dx = translate_offset(x0, x1, &xs, threshold);
    let dy = translate_offset(y0, y1, &ys, threshold);
    rect_from_edges(x0 + dx, y0 + dy, x1 + dx, y1 + dy)
}

/// 边/角手柄的吸附：只吸附**被拖动**的那条/两条边，固定边保持不动。
///
/// [`Handle::Move`] 退化为 [`snap_move_rect`]。若吸附后该方向宽/高小于
/// [`MIN_SELECTION_SIZE`]，放弃这条边的吸附（保持拖拽结果）。
pub(crate) fn snap_resize_rect(
    handle: Handle,
    rect: PhysicalRect,
    candidates: &[PhysicalRect],
    threshold: i32,
) -> PhysicalRect {
    if handle == Handle::Move {
        return snap_move_rect(rect, candidates, threshold);
    }
    if threshold <= 0 || candidates.is_empty() {
        return rect;
    }
    let threshold = threshold as i64;
    let min = MIN_SELECTION_SIZE as i64;
    let (xs, ys) = candidate_edges(candidates);
    let (x0, y0, x1, y1) = edges(rect);

    let mut nx0 = x0;
    let mut nx1 = x1;
    let mut ny0 = y0;
    let mut ny1 = y1;
    if handle.adjusts_west() {
        if let Some(edge) = nearest_edge(x0, &xs, threshold) {
            if x1 - edge >= min {
                nx0 = edge;
            }
        }
    }
    if handle.adjusts_east() {
        if let Some(edge) = nearest_edge(x1, &xs, threshold) {
            if edge - x0 >= min {
                nx1 = edge;
            }
        }
    }
    if handle.adjusts_north() {
        if let Some(edge) = nearest_edge(y0, &ys, threshold) {
            if y1 - edge >= min {
                ny0 = edge;
            }
        }
    }
    if handle.adjusts_south() {
        if let Some(edge) = nearest_edge(y1, &ys, threshold) {
            if edge - y0 >= min {
                ny1 = edge;
            }
        }
    }
    rect_from_edges(nx0, ny0, nx1, ny1)
}

/// 框选框选（`Dragging`）的吸附：只吸附随指针移动的边，`anchor` 所在的边不动。
///
/// 逐轴判断：某轴宽/高 > 0（指针已在该轴移动）才吸附该轴随指针移动的边；两轴都为 0
/// （单击，没有位移）时原样返回。纯水平拖动（`height == 0`）仍会吸附 x 轴，纯垂直拖动同理。
pub(crate) fn snap_drag_rect(
    anchor: PhysicalPoint,
    rect: PhysicalRect,
    candidates: &[PhysicalRect],
    threshold: i32,
) -> PhysicalRect {
    if threshold <= 0 || candidates.is_empty() {
        return rect;
    }
    let threshold = threshold as i64;
    let min = MIN_SELECTION_SIZE as i64;
    let (xs, ys) = candidate_edges(candidates);
    let (x0, y0, x1, y1) = edges(rect);

    let mut nx0 = x0;
    let mut nx1 = x1;
    let mut ny0 = y0;
    let mut ny1 = y1;
    if rect.width > 0 {
        if (anchor.x as i64) <= x0 {
            // `anchor` 在左边（`x0`），随指针移动的是右边。
            if let Some(edge) = nearest_edge(x1, &xs, threshold) {
                if edge - x0 >= min {
                    nx1 = edge;
                }
            }
        } else if let Some(edge) = nearest_edge(x0, &xs, threshold) {
            // `anchor` 在右边（`x1`），随指针移动的是左边。
            if x1 - edge >= min {
                nx0 = edge;
            }
        }
    }
    if rect.height > 0 {
        if (anchor.y as i64) <= y0 {
            if let Some(edge) = nearest_edge(y1, &ys, threshold) {
                if edge - y0 >= min {
                    ny1 = edge;
                }
            }
        } else if let Some(edge) = nearest_edge(y0, &ys, threshold) {
            if y1 - edge >= min {
                ny0 = edge;
            }
        }
    }
    rect_from_edges(nx0, ny0, nx1, ny1)
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

/// 半开区间下的四条边 `(left, top, right, bottom)`，用 `i64` 避免溢出。
fn edges(rect: PhysicalRect) -> (i64, i64, i64, i64) {
    let x0 = rect.x as i64;
    let y0 = rect.y as i64;
    (x0, y0, x0 + rect.width as i64, y0 + rect.height as i64)
}

/// 候选窗口的所有竖直边与水平边（各自左/右、上/下两条）。
fn candidate_edges(candidates: &[PhysicalRect]) -> (Vec<i64>, Vec<i64>) {
    let mut xs = Vec::with_capacity(candidates.len() * 2);
    let mut ys = Vec::with_capacity(candidates.len() * 2);
    for c in candidates {
        let (x0, y0, x1, y1) = edges(*c);
        xs.push(x0);
        xs.push(x1);
        ys.push(y0);
        ys.push(y1);
    }
    (xs, ys)
}

/// 整框平移吸附的轴偏移：把 `lo`/`hi` 对齐到阈值内最近候选边的位移量。
fn translate_offset(lo: i64, hi: i64, edges: &[i64], threshold: i64) -> i64 {
    let mut best: Option<i64> = None;
    for &edge in edges {
        for offset in [edge - lo, edge - hi] {
            if offset.abs() > threshold {
                continue;
            }
            if best.is_none_or(|b| offset.abs() < b.abs()) {
                best = Some(offset);
            }
        }
    }
    best.unwrap_or(0)
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
