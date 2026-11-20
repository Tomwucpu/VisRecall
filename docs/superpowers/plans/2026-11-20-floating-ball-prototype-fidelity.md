# 悬浮球原型复刻 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在原生分层窗口内复刻 MonoDrop 悬浮球的视觉与交互动效，并接入拖放导入进度环/对勾。

**Architecture:** 保持 `crates/shell/src/ball` 的原生线程与 OLE 拖放不变。把视觉几何与缓动抽成纯逻辑模块，状态机只产出目标形态；窗口层用 16ms 定时器逐帧插值并 `UpdateLayeredWindow`。app-core 通过可克隆的反馈句柄向悬浮球回推导入进度与结果。

**Tech Stack:** Rust 2021、windows 0.62（GDI+ / User32 / Registry）、crossbeam-channel、crossbeam select。

**Spec:** `docs/superpowers/specs/2026-11-20-floating-ball-prototype-fidelity-design.md`

> 执行状态：已完成（2026-11-20）。自动化与实机验证结果见 Spec §11；未覆盖项已在 §11.3 列出。

## Global Constraints

- Windows only；不引入 WebView、新进程或新线程。
- 单进程、单悬浮球线程；动画在现有消息循环内驱动。
- 保留单击截图、OLE 拖放、抓帧前同步隐藏、全屏隐藏、位置记忆、托盘开关。
- 不新增 IPC 命令、不新增配置字段、不改前端。
- 注释/文档/UI 文案用中文，标识符用英文。
- 错误统一 `AppError`；不使用 `unwrap()` 处理运行时输入。
- 不自动提交 git；提交需用户明确要求。
- 最低验证：`cargo test -p visrecall-shell`、`cargo test -p visrecall-app-core`、`cargo clippy --workspace --all-targets`。

---

## File Structure

| 文件 | 职责 |
| --- | --- |
| `crates/shell/src/ball/anim.rs` | 视觉度量、四边几何、命中测试、缓动与帧插值（纯逻辑） |
| `crates/shell/src/ball/state.rs` | 形态状态机；使用 `BallMetrics`，四边胶囊，立即展开 |
| `crates/shell/src/ball/mod.rs` | 绘制、窗口矩形、命中、动画驱动、主题、反馈消息 |
| `crates/shell/src/drop_target.rs` | 拖入视觉状态（沿用 `DropVisual`） |
| `crates/shell/src/lib.rs` | 导出新类型 |
| `crates/app-core/src/app.rs` | 反馈句柄传递、闸门拒绝反馈 |
| `crates/app-core/src/import_flow.rs` | 带进度回调的导入入口 |
| `crates/shell/tests/ball_anim.rs` | 几何/缓动集成测试（新增） |
| `crates/shell/tests/ball_state.rs` | 状态机时序测试（更新） |
| `crates/shell/tests/ball_position.rs` | 位置/夹取测试（预计不变） |
| `crates/app-core/src/import_flow.rs` 测试模块 | 进度回调测试 |

---

### Task 1: 视觉度量、几何与缓动

**Files:**
- Create: `crates/shell/src/ball/anim.rs`
- Modify: `crates/shell/src/ball/mod.rs`（加入 `pub mod anim;`）
- Test: `crates/shell/tests/ball_anim.rs`

**Interfaces:**
- Produces:
  - `pub struct BallMetrics { size, threshold, strip_thickness, strip_length, hit_padding, edge_offset, shadow_margin, tooltip_space }`
  - `BallMetrics::new(size: u32) -> BallMetrics`
  - `pub struct CornerRadii { tl, tr, br, bl }` + `capsule(f32)` / `docked_strip(BallEdge, f32)` / `lerp`
  - `pub fn window_rect(visual: PhysicalRect, anchor: Option<BallEdge>, drop_active: bool, m: BallMetrics) -> PhysicalRect`
  - `pub fn hits_visual(visual: PhysicalRect, anchor: Option<BallEdge>, m: BallMetrics, point: PhysicalPoint) -> bool`
  - `pub fn tooltip_rect(text_chars: usize, m: BallMetrics, visual: PhysicalRect, work: PhysicalRect) -> PhysicalRect`
  - `pub enum Curve { Standard, Spring, Linear }`
  - `pub fn curve_value(curve: Curve, t: f32) -> f32`
  - `pub struct VisualFrame { rect, radii, scale, opacity, icon_alpha, icon_scale, grip_alpha, halo_alpha, tooltip_alpha }`
  - `pub struct Transition { from: VisualFrame, to: VisualFrame, start_ms: u64, duration_ms: u64, curve: Curve }`
  - `Transition::sample(now_ms: u64) -> (VisualFrame, bool)`（第二项表示是否结束）

- [ ] **Step 1: 写失败测试**

创建 `crates/shell/tests/ball_anim.rs`：

```rust
use visrecall_domain::geometry::{PhysicalPoint, PhysicalRect};
use visrecall_shell::ball::anim::{
    curve_value, hits_visual, tooltip_rect, window_rect, BallMetrics, CornerRadii, Curve,
};
use visrecall_shell::ball::state::BallEdge;

#[test]
fn metrics_follow_the_56px_reference() {
    let m = BallMetrics::new(56);
    assert_eq!(m.threshold, 64);
    assert_eq!(m.strip_thickness, 10);
    assert_eq!(m.strip_length, 62);
    assert_eq!(m.hit_padding, 14);
    assert_eq!(m.edge_offset, 12);
}

#[test]
fn metrics_scale_down_with_the_ball() {
    let m = BallMetrics::new(40);
    assert_eq!(m.threshold, 46);
    assert_eq!(m.strip_thickness, 7);
    assert_eq!(m.strip_length, 44);
    assert_eq!(m.hit_padding, 10);
    assert_eq!(m.edge_offset, 9);
}

#[test]
fn free_window_adds_shadow_margin_on_all_sides() {
    let m = BallMetrics::new(56);
    let visual = PhysicalRect::new(100, 100, 56, 56);
    let window = window_rect(visual, None, false, m);
    assert_eq!(window, PhysicalRect::new(86, 86, 84, 84));
}

#[test]
fn docked_window_extends_outwards_for_the_hit_area() {
    let m = BallMetrics::new(56);
    let visual = PhysicalRect::new(12, 200, 56, 56);
    let window = window_rect(visual, Some(BallEdge::Left), false, m);
    assert_eq!(window.x, 12 - 14);
    assert_eq!(window.y, 200 - 14);
    assert_eq!(window.right(), 12 + 56 + 14);
}

#[test]
fn drop_active_window_reserves_space_above_for_the_tooltip() {
    let m = BallMetrics::new(56);
    let visual = PhysicalRect::new(400, 300, 56, 56);
    let normal = window_rect(visual, None, false, m);
    let with_tip = window_rect(visual, None, true, m);
    assert_eq!(with_tip.y, normal.y - m.tooltip_space as i32);
    assert_eq!(with_tip.height, normal.height + m.tooltip_space);
}

#[test]
fn free_hit_test_uses_the_circle() {
    let m = BallMetrics::new(56);
    let visual = PhysicalRect::new(100, 100, 56, 56);
    assert!(hits_visual(visual, None, m, PhysicalPoint { x: 128, y: 128 }));
    assert!(!hits_visual(visual, None, m, PhysicalPoint { x: 100, y: 100 }));
}

#[test]
fn docked_hit_test_includes_the_padding() {
    let m = BallMetrics::new(56);
    let visual = PhysicalRect::new(12, 200, 56, 56);
    assert!(hits_visual(
        visual,
        Some(BallEdge::Left),
        m,
        PhysicalPoint { x: 0, y: 228 }
    ));
    assert!(!hits_visual(
        visual,
        Some(BallEdge::Left),
        m,
        PhysicalPoint { x: 12 + 56 + 15, y: 228 }
    ));
}

#[test]
fn tooltip_is_centered_and_clamped() {
    let m = BallMetrics::new(56);
    let work = PhysicalRect::new(0, 0, 400, 300);
    let visual = PhysicalRect::new(10, 100, 56, 56);
    let tip = tooltip_rect(8, m, visual, work);
    assert!(tip.x >= work.x);
    assert!(tip.right() <= work.right());
    assert!(tip.bottom() <= visual.y);
}

#[test]
fn curves_hit_the_endpoints() {
    for curve in [Curve::Standard, Curve::Spring, Curve::Linear] {
        assert!((curve_value(curve, 0.0) - 0.0).abs() < 0.001);
        assert!((curve_value(curve, 1.0) - 1.0).abs() < 0.001);
    }
    assert!(curve_value(Curve::Spring, 0.6) > 1.0);
}

#[test]
fn strip_radii_round_only_the_screen_side() {
    let left = CornerRadii::docked_strip(BallEdge::Left, 10.0);
    assert_eq!(left, CornerRadii { tl: 0.0, tr: 5.0, br: 5.0, bl: 0.0 });
    let top = CornerRadii::docked_strip(BallEdge::Top, 10.0);
    assert_eq!(top, CornerRadii { tl: 0.0, tr: 0.0, br: 5.0, bl: 5.0 });
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p visrecall-shell --test ball_anim`  
Expected: 编译失败，`visrecall_shell::ball::anim` 不存在。

- [ ] **Step 3: 实现 `anim.rs` 的纯逻辑**

核心实现（按此落地，注释保留关键约束）：

```rust
//! 悬浮球视觉度量、四边几何与缓动（纯逻辑，不触碰窗口）。

use visrecall_domain::geometry::{PhysicalPoint, PhysicalRect};

use crate::ball::state::BallEdge;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BallMetrics {
    pub size: u32,
    pub threshold: i32,
    pub strip_thickness: u32,
    pub strip_length: u32,
    pub hit_padding: u32,
    pub edge_offset: u32,
    pub shadow_margin: u32,
    pub tooltip_space: u32,
}

impl BallMetrics {
    pub fn new(size: u32) -> Self {
        let k = size as f32 / 56.0;
        let scaled = |value: f32| (value * k).round().max(1.0) as u32;
        Self {
            size,
            threshold: (64.0 * k).round() as i32,
            strip_thickness: scaled(10.0),
            strip_length: size + scaled(6.0),
            hit_padding: scaled(14.0),
            edge_offset: scaled(12.0),
            shadow_margin: scaled(14.0),
            tooltip_space: scaled(34.0),
        }
    }

    pub fn factor(&self) -> f32 {
        self.size as f32 / 56.0
    }
}

pub fn window_rect(
    visual: PhysicalRect,
    anchor: Option<BallEdge>,
    drop_active: bool,
    m: BallMetrics,
) -> PhysicalRect {
    let shadow = m.shadow_margin as i32;
    let pad = m.hit_padding as i32;
    let (mut left, mut top) = (visual.x - shadow, visual.y - shadow);
    let (mut right, mut bottom) = (visual.right() + shadow, visual.bottom() + shadow);
    if let Some(edge) = anchor {
        match edge {
            BallEdge::Left => left = visual.x - pad,
            BallEdge::Right => right = visual.right() + pad,
            BallEdge::Top => top = visual.y - pad,
            BallEdge::Bottom => bottom = visual.bottom() + pad,
        }
    }
    if drop_active {
        top -= m.tooltip_space as i32;
    }
    PhysicalRect::new(
        left,
        top,
        (right - left).max(1) as u32,
        (bottom - top).max(1) as u32,
    )
}

pub fn hits_visual(
    visual: PhysicalRect,
    anchor: Option<BallEdge>,
    m: BallMetrics,
    point: PhysicalPoint,
) -> bool {
    if anchor.is_some() {
        let pad = m.hit_padding as i32;
        return point.x >= visual.x - pad
            && point.x < visual.right() + pad
            && point.y >= visual.y - pad
            && point.y < visual.bottom() + pad;
    }
    let cx = visual.x as f32 + visual.width as f32 / 2.0;
    let cy = visual.y as f32 + visual.height as f32 / 2.0;
    let dx = point.x as f32 - cx;
    let dy = point.y as f32 - cy;
    let r = visual.width as f32 / 2.0;
    dx * dx + dy * dy <= r * r
}

pub fn tooltip_rect(
    text_chars: usize,
    m: BallMetrics,
    visual: PhysicalRect,
    work: PhysicalRect,
) -> PhysicalRect {
    let font = 11.0 * m.factor();
    let pad_x = 11.0 * m.factor();
    let height = (font + 10.0 * m.factor()).round().max(18.0) as u32;
    let width = (text_chars as f32 * font * 0.95 + pad_x * 2.0)
        .round()
        .max(52.0) as u32;
    let gap = 14.0 * m.factor();
    let center = visual.x + visual.width as i32 / 2;
    let x = (center - width as i32 / 2).clamp(work.x + 4, work.right() - width as i32 - 4);
    let y = (visual.y as f32 - gap - height as f32).round() as i32;
    PhysicalRect::new(x, y.max(work.y + 4), width, height)
}
```

`CornerRadii`、`Curve`、`VisualFrame`、`Transition`：

```rust
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CornerRadii {
    pub tl: f32,
    pub tr: f32,
    pub br: f32,
    pub bl: f32,
}

impl CornerRadii {
    pub fn capsule(radius: f32) -> Self {
        Self { tl: radius, tr: radius, br: radius, bl: radius }
    }

    pub fn docked_strip(edge: BallEdge, thickness: f32) -> Self {
        let r = thickness / 2.0;
        match edge {
            BallEdge::Left => Self { tl: 0.0, tr: r, br: r, bl: 0.0 },
            BallEdge::Right => Self { tl: r, tr: 0.0, br: 0.0, bl: r },
            BallEdge::Top => Self { tl: 0.0, tr: 0.0, br: r, bl: r },
            BallEdge::Bottom => Self { tl: r, tr: r, br: 0.0, bl: 0.0 },
        }
    }

    pub fn lerp(self, to: Self, t: f32) -> Self {
        let mix = |a: f32, b: f32| a + (b - a) * t;
        Self {
            tl: mix(self.tl, to.tl),
            tr: mix(self.tr, to.tr),
            br: mix(self.br, to.br),
            bl: mix(self.bl, to.bl),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Curve {
    Standard,
    Spring,
    Linear,
}

fn cubic_bezier(x1: f32, y1: f32, x2: f32, y2: f32, t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    let sample = |u: f32, a: f32, b: f32| {
        let v = 1.0 - u;
        3.0 * v * v * u * a + 3.0 * v * u * u * b + u * u * u
    };
    let mut lo = 0.0;
    let mut hi = 1.0;
    for _ in 0..24 {
        let mid = (lo + hi) / 2.0;
        if sample(mid, x1, x2) < t {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    sample((lo + hi) / 2.0, y1, y2)
}

pub fn curve_value(curve: Curve, t: f32) -> f32 {
    match curve {
        Curve::Linear => t.clamp(0.0, 1.0),
        Curve::Standard => cubic_bezier(0.22, 1.0, 0.36, 1.0, t),
        Curve::Spring => cubic_bezier(0.34, 1.38, 0.64, 1.0, t),
    }
}
```

`VisualFrame` 与 `Transition`（字段与插值）：

```rust
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VisualFrame {
    pub rect: PhysicalRect,
    pub radii: CornerRadii,
    pub scale: f32,
    pub opacity: f32,
    pub icon_alpha: f32,
    pub icon_scale: f32,
    pub grip_alpha: f32,
    pub halo_alpha: f32,
    pub tooltip_alpha: f32,
}

impl VisualFrame {
    pub fn lerp(self, to: Self, t: f32) -> Self {
        let mix = |a: f32, b: f32| a + (b - a) * t;
        Self {
            rect: PhysicalRect::new(
                mix(self.rect.x as f32, to.rect.x as f32).round() as i32,
                mix(self.rect.y as f32, to.rect.y as f32).round() as i32,
                mix(self.rect.width as f32, to.rect.width as f32).round().max(1.0) as u32,
                mix(self.rect.height as f32, to.rect.height as f32).round().max(1.0) as u32,
            ),
            radii: self.radii.lerp(to.radii, t),
            scale: mix(self.scale, to.scale),
            opacity: mix(self.opacity, to.opacity),
            icon_alpha: mix(self.icon_alpha, to.icon_alpha),
            icon_scale: mix(self.icon_scale, to.icon_scale),
            grip_alpha: mix(self.grip_alpha, to.grip_alpha),
            halo_alpha: mix(self.halo_alpha, to.halo_alpha),
            tooltip_alpha: mix(self.tooltip_alpha, to.tooltip_alpha),
        }
    }
}

pub struct Transition {
    pub from: VisualFrame,
    pub to: VisualFrame,
    pub start_ms: u64,
    pub duration_ms: u64,
    pub curve: Curve,
}

impl Transition {
    pub fn sample(&self, now_ms: u64) -> (VisualFrame, bool) {
        if self.duration_ms == 0 {
            return (self.to, true);
        }
        let raw = ((now_ms.saturating_sub(self.start_ms)) as f32
            / self.duration_ms as f32)
            .clamp(0.0, 1.0);
        let t = curve_value(self.curve, raw);
        (self.from.lerp(self.to, t), raw >= 1.0)
    }
}
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test -p visrecall-shell --test ball_anim`  
Expected: 全部 PASS。

---

### Task 2: 状态机改为四边胶囊与原型时序

**Files:**
- Modify: `crates/shell/src/ball/state.rs`
- Modify: `crates/shell/tests/ball_state.rs`

**Interfaces:**
- Consumes: `BallMetrics`、`CornerRadii`（Task 1）。
- Produces: `BallEvent::DragStarted`；`BallStateMachine::current_rect()` 返回视觉矩形（不再含命中留白）。

- [ ] **Step 1: 更新测试（先写失败断言）**

修改 `crates/shell/tests/ball_state.rs`：

```rust
const SIZE: u32 = 56;
```

把 12px/13px 用例改为 64px/65px：

```rust
#[test]
fn released_within_64px_collapses_into_strip() {
    let mut m = machine();
    let actions = release_and_settle(&mut m, at_left(64), 1_000);
    assert!(actions.iter().any(|a| matches!(a, BallAction::ScheduleTimer { .. })));
    assert_eq!(m.visual(), BallVisual::Strip);
    assert_eq!(m.anchor(), Some(BallEdge::Left));
    let rect = m.current_rect();
    assert_eq!(rect.x, WORK.x);
    assert_eq!(rect.width, 10);
    assert_eq!(rect.height, 62);
}

#[test]
fn released_65px_away_keeps_the_ball() {
    let mut m = machine();
    let actions = release_and_settle(&mut m, at_left(65), 1_000);
    assert_eq!(m.visual(), BallVisual::Ball);
    assert_eq!(m.anchor(), None);
    assert!(actions.iter().all(|a| match a {
        BallAction::Show { rect } => rect.width == SIZE && rect.height == SIZE,
        BallAction::ScheduleTimer { .. } => false,
        _ => true,
    }));
}
```

`hovering_the_strip_expands_after_150ms` 改为立即展开：

```rust
#[test]
fn hovering_the_strip_expands_immediately() {
    let mut m = machine();
    release_and_settle(&mut m, at_left(0), 1_000);
    assert_eq!(m.visual(), BallVisual::Strip);

    let actions = m.handle(BallEvent::CursorEnter { now_ms: 2_000 });
    assert!(matches!(actions.as_slice(), [BallAction::Show { .. }]));
    assert_eq!(m.visual(), BallVisual::Ball);
    let rect = m.current_rect();
    assert_eq!(rect.x, WORK.x + 12);
    assert_eq!(rect.width, SIZE);
}
```

`leaving_the_expanded_ball_retracts_after_600ms` 改为 800ms，并跳过展开 Tick。

新增四边胶囊用例：

```rust
#[test]
fn top_and_bottom_strips_are_horizontal() {
    let mut m = machine();
    release_and_settle(&mut m, PhysicalPoint { x: 900, y: 0 }, 1_000);
    assert_eq!(m.anchor(), Some(BallEdge::Top));
    let top = m.current_rect();
    assert_eq!(top.y, WORK.y);
    assert_eq!(top.width, 62);
    assert_eq!(top.height, 10);

    let mut m = machine();
    release_and_settle(
        &mut m,
        PhysicalPoint { x: 900, y: WORK.bottom() - SIZE },
        1_000,
    );
    assert_eq!(m.anchor(), Some(BallEdge::Bottom));
    let bottom = m.current_rect();
    assert_eq!(bottom.bottom(), WORK.bottom());
    assert_eq!(bottom.width, 62);
    assert_eq!(bottom.height, 10);
}

#[test]
fn dragging_clears_the_anchor() {
    let mut m = machine();
    release_and_settle(&mut m, at_left(0), 1_000);
    assert_eq!(m.anchor(), Some(BallEdge::Left));
    let actions = m.handle(BallEvent::DragStarted);
    assert_eq!(m.anchor(), None);
    assert_eq!(m.visual(), BallVisual::Ball);
    assert!(matches!(actions.as_slice(), [BallAction::Show { .. }]));
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p visrecall-shell --test ball_state`  
Expected: 旧常量/旧矩形断言失败，`DragStarted` 不存在。

- [ ] **Step 3: 修改 `state.rs`**

- 删除 `STRIP_THICKNESS` / `STRIP_HIT_PADDING` 常量，改用 `BallMetrics`：

```rust
use crate::ball::anim::BallMetrics;

pub const EDGE_SNAP_PX: i32 = 64; // 仅作为文档基准；实际阈值来自 BallMetrics
pub const COLLAPSE_DELAY_MS: u64 = 800;
pub const RETRACT_DELAY_MS: u64 = 800;
```

- `on_released`、`nearest_edge` 使用 `BallMetrics::new(self.settings.size).threshold`。
- `current_rect()`：

```rust
pub fn current_rect(&self) -> PhysicalRect {
    match self.visual {
        BallVisual::Strip => self.strip_rect(),
        BallVisual::Ball if self.anchor.is_some() => self.docked_ball_rect(),
        _ => self.ball_rect(),
    }
}
```

- `strip_rect()` 返回视觉矩形（10×62），沿边夹取：

```rust
pub fn strip_rect(&self) -> PhysicalRect {
    let m = BallMetrics::new(self.settings.size);
    let t = m.strip_thickness;
    let len = m.strip_length;
    match self.anchor {
        Some(edge) if edge.is_vertical() => {
            let x = match edge {
                BallEdge::Left => self.work_area.x,
                _ => self.work_area.right() - t as i32,
            };
            let y = (self.center.y - len as i32 / 2).clamp(
                self.work_area.y,
                self.work_area.bottom() - len as i32,
            );
            PhysicalRect::new(x, y, t, len)
        }
        Some(edge) => {
            let y = match edge {
                BallEdge::Top => self.work_area.y,
                _ => self.work_area.bottom() - t as i32,
            };
            let x = (self.center.x - len as i32 / 2).clamp(
                self.work_area.x,
                self.work_area.right() - len as i32,
            );
            PhysicalRect::new(x, y, len, t)
        }
        None => self.ball_rect(),
    }
}
```

- 新增 `docked_ball_rect()`：

```rust
pub fn docked_ball_rect(&self) -> PhysicalRect {
    let m = BallMetrics::new(self.settings.size);
    let size = m.size as i32;
    let offset = m.edge_offset as i32;
    let (x, y) = match self.anchor {
        Some(BallEdge::Left) => (self.work_area.x + offset, self.center.y - size / 2),
        Some(BallEdge::Right) => (self.work_area.right() - offset - size, self.center.y - size / 2),
        Some(BallEdge::Top) => (self.center.x - size / 2, self.work_area.y + offset),
        Some(BallEdge::Bottom) => (self.center.x - size / 2, self.work_area.bottom() - offset - size),
        None => return self.ball_rect(),
    };
    PhysicalRect::new(
        x.clamp(self.work_area.x, self.work_area.right() - size),
        y.clamp(self.work_area.y, self.work_area.bottom() - size),
        m.size,
        m.size,
    )
}
```

- `BallEvent` 增加 `DragStarted`；`handle` 中：

```rust
BallEvent::DragStarted => {
    self.anchor = None;
    self.collapse_at = None;
    self.expand_at = None;
    self.retract_at = None;
    if self.visual == BallVisual::Strip {
        self.visual = BallVisual::Ball;
    }
    vec![BallAction::Show { rect: self.current_rect() }]
}
```

- `on_cursor_enter` 遇到 Strip 立即展开（不再调度 `EXPAND_DELAY_MS`）：

```rust
fn on_cursor_enter(&mut self, _now_ms: u64) -> Vec<BallAction> {
    self.hover = true;
    self.collapse_at = None;
    self.expand_at = None;
    if self.visual == BallVisual::Strip {
        self.visual = BallVisual::Ball;
        return vec![BallAction::Show { rect: self.current_rect() }];
    }
    Vec::new()
}
```

- `on_cursor_leave` / `on_tick` 的回缩延时改用 `RETRACT_DELAY_MS`（800）。

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test -p visrecall-shell --test ball_state`  
Expected: 全部 PASS。

---

### Task 3: 反馈类型与消息

**Files:**
- Modify: `crates/shell/src/ball/mod.rs`
- Modify: `crates/shell/src/lib.rs`
- Test: `crates/shell/tests/ball_feedback.rs`（新增）

**Interfaces:**
- Produces:
  - `pub enum BallFeedback { Busy { progress: Option<u8> }, Success, Failure }`
  - `BallFeedback::pack(self) -> usize` / `BallFeedback::unpack(payload: usize) -> BallFeedback`
  - `pub struct BallFeedbackHandle`（`Clone + Send`）
  - `BallHandle::feedback_handle(&self) -> BallFeedbackHandle`
  - `BallFeedbackHandle::send(&self, feedback: BallFeedback)`（窗口不存在时静默返回）

- [ ] **Step 1: 写失败测试**

```rust
use visrecall_shell::ball::{BallFeedback, BallFeedbackHandle};

#[test]
fn feedback_roundtrips_through_the_payload() {
    for feedback in [
        BallFeedback::Busy { progress: None },
        BallFeedback::Busy { progress: Some(0) },
        BallFeedback::Busy { progress: Some(99) },
        BallFeedback::Success,
        BallFeedback::Failure,
    ] {
        assert_eq!(BallFeedback::unpack(feedback.pack()), feedback);
    }
}

#[test]
fn a_detached_handle_is_a_no_op() {
    BallFeedbackHandle::detached().send(BallFeedback::Success);
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p visrecall-shell --test ball_feedback`  
Expected: 类型不存在。

- [ ] **Step 3: 实现**

- `WM_BALL_FEEDBACK = WM_APP + 12`。
- 打包：低 2 位 kind（0 Busy / 1 Success / 2 Failure），第 8–15 位 progress（Busy 且非空时 1..=99）。
- `BallFeedbackHandle { hwnd: Arc<AtomicIsize> }`；`detached()` 用 `Arc::new(AtomicIsize::new(0))`。
- `send` 读取 hwnd，0 则返回；否则 `PostMessageW(hwnd, WM_BALL_FEEDBACK, WPARAM(packed), LPARAM(0))`。
- `BallHandle::feedback_handle` 克隆内部 `hwnd`。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p visrecall-shell --test ball_feedback`  
Expected: PASS。

---

### Task 4: 导入进度回调

**Files:**
- Modify: `crates/app-core/src/import_flow.rs`
- Test: `crates/app-core/src/import_flow.rs` 内联测试模块

**Interfaces:**
- Produces:
  - `pub fn ingest_with_progress(&self, content: DropContent, progress: &mut dyn FnMut(u32, u32)) -> Result<Vec<Item>, AppError>`
  - 原 `ingest` 保持签名，内部调用 `ingest_with_progress` + 空回调。

- [ ] **Step 1: 写失败测试**

```rust
#[test]
fn progress_reports_every_file() {
    use visrecall_domain::drop::DropContent;

    let fixture = fixture();
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.txt");
    let b = dir.path().join("b.txt");
    fs::write(&a, "alpha").unwrap();
    fs::write(&b, "beta").unwrap();

    let mut seen = Vec::new();
    fixture
        .flow
        .ingest_with_progress(
            DropContent::Files(vec![a, b]),
            &mut |done, total| seen.push((done, total)),
        )
        .unwrap();

    assert_eq!(seen, vec![(1, 2), (2, 2)]);
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p visrecall-app-core import_flow::tests::progress_reports_every_file`  
Expected: 方法不存在。

- [ ] **Step 3: 实现**

- `ingest` 调用 `self.ingest_with_progress(content, &mut |_, _| {})`。
- `ingest_with_progress` 按 `DropContent` 分支，把回调传入 `import_files` / 单次分支。
- `import_files` 的循环末尾：

```rust
processed += 1;
progress(processed, files.len() as u32);
```

- 文本/链接/DIB 分支：处理前 `progress(0, 1)`，成功后 `progress(1, 1)`；失败不调用完成值。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p visrecall-app-core import_flow::tests::progress_reports_every_file`  
Expected: PASS。

---

### Task 5: app-core 接线

**Files:**
- Modify: `crates/app-core/src/app.rs`
- Modify: `crates/app-core/tests/relocate_flow.rs`

**Interfaces:**
- Consumes: `BallFeedbackHandle`、`BallFeedback`、`ImportFlow::ingest_with_progress`。
- Produces: `spawn_capture_loop(..., feedback: Option<BallFeedbackHandle>)`；`handle_shell_event(..., feedback: Option<&BallFeedbackHandle>, event: ShellEvent)`。

- [ ] **Step 1: 改签名并让测试传 `None`**

- `spawn_capture_loop` 增加 `feedback: Option<BallFeedbackHandle>`，在循环内克隆传给 `handle_shell_event`。
- `handle_shell_event` 增加 `feedback: Option<&BallFeedbackHandle>`：
  - 闸门拒绝 `Drop`：`feedback.map(|f| f.send(BallFeedback::Failure))`。
  - 放行 `Drop`：先 `Busy { progress: None }`，再 `spawn_import(import, content, feedback.cloned())`。
  - `DropRejected`：`Failure`。
- `spawn_import(import, content, feedback: Option<BallFeedbackHandle>)`：

```rust
let spawned = thread::Builder::new()
    .name("visrecall-import".into())
    .spawn(move || {
        let mut report = |done: u32, total: u32| {
            if let Some(handle) = &feedback {
                let pct = if total == 0 {
                    None
                } else {
                    Some(((done as f32 / total as f32) * 99.0).round() as u8)
                };
                handle.send(BallFeedback::Busy { progress: pct });
            }
        };
        match import.ingest_with_progress(content, &mut report) {
            Ok(items) => {
                import.emit_created(&items);
                if let Some(handle) = &feedback {
                    handle.send(BallFeedback::Success);
                }
            }
            Err(err) => {
                tracing::warn!(error = %err, "drop import failed");
                if let Some(handle) = &feedback {
                    handle.send(BallFeedback::Failure);
                }
            }
        }
    });
```

- `AppCore::start_runtime`：`let feedback = ball.feedback_handle();`，传给 `spawn_capture_loop`。
- 测试调用点补 `None`（`app.rs` 内测试、`relocate_flow.rs` 两处）。

- [ ] **Step 2: 编译并跑测试**

Run: `cargo test -p visrecall-app-core`  
Expected: 全部 PASS（含 `spawn_import_dispatches_to_a_worker_thread`）。

---

### Task 6: 绘制重做

**Files:**
- Modify: `crates/shell/src/ball/mod.rs`
- Modify: `crates/shell/Cargo.toml`（增加 `Win32_System_Registry` feature）

**Interfaces:**
- Produces:
  - `enum Theme { Light, Dark }` + `fn system_theme() -> Theme`
  - `fn render_ball(hwnd, window_rect, frame: &VisualFrame, payload: &PaintPayload, theme: Theme) -> Result<(), AppError>`
  - `struct PaintPayload { icon: IconKind, tooltip: Option<String>, progress: Option<f32>, halo_rotation: f32, halo_breath: f32, shadow: ShadowLevel }`

- [ ] **Step 1: 增加注册表读取（主题）**

`Cargo.toml` 的 windows features 追加 `"Win32_System_Registry"`。实现：

```rust
fn system_theme() -> Theme {
    let subkey = wide(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
    let value = wide("AppsUseLightTheme");
    let mut data: u32 = 1;
    let mut size = std::mem::size_of::<u32>() as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(subkey.as_ptr()),
            PCWSTR(value.as_ptr()),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut data as *mut u32 as *mut c_void),
            Some(&mut size),
        )
    };
    if status.0 == 0 && data == 0 {
        Theme::Dark
    } else {
        Theme::Light
    }
}
```

- [ ] **Step 2: 替换 `paint_ball`**

按 Spec §3.4/§3.5 实现：

1. `themed_colors(theme)` 返回球体/描边/图标/阴影颜色。
2. 用 `GdipCreatePath` + `GdipAddPathArc`/`GdipAddPathLine`/`GdipClosePathFigure` 画圆角矩形（支持四角独立半径）。
3. 阴影：同一路径向下偏移，重复 4 层，alpha 递减。
4. 球体：填充 + 描边 + 顶部 1px 内高光（上半圆弧，白色低透明）。
5. 图标：
   - 默认：圆角方框 + 十字（`GdipDrawPath`）。
   - 拖入：向下箭头 + 底部横线。
   - 完成：两段折线对勾。
6. 指示线：`GdipSetPenLineCap197819(.., LineCapRound, LineCapRound, DashCapFlat)` + `GdipDrawLine`。
7. 进度环：`GdipDrawArc` 画轨道 + 前景；`progress == None` 时用时间旋转 60° 弧。
8. 光环：半径 = 球半径 + 10k，画 16 段虚线圆弧，按 `halo_rotation` 旋转，alpha 用 `halo_breath`。
9. 气泡：`GdipMeasureString` 量宽 → 胶囊底 + 反色文字。

绘制坐标一律相对窗口；视觉矩形偏移 = `frame.rect - window_rect`；`scale` 以视觉中心为锚放大。

- [ ] **Step 3: 编译**

Run: `cargo check -p visrecall-shell`  
Expected: 编译通过。

---

### Task 7: 窗口层动画、命中与反馈

**Files:**
- Modify: `crates/shell/src/ball/mod.rs`

**Interfaces:**
- Consumes: Task 1–6 的类型与函数。
- Produces: 完整交互实现。

- [ ] **Step 1: 扩展 `BallWindowState`**

新增字段：

```rust
theme: Theme,
frame: VisualFrame,
transition: Option<Transition>,
feedback: FeedbackState,     // Idle / Busy(Option<f32>) / Success / Failure
feedback_until: Option<u64>,
drop_active: bool,
```

`FeedbackState` 只保存逻辑状态；进度环的时间动画由 `now_ms` 现算。

- [ ] **Step 2: 动作落地改为动画目标**

`apply_actions` 的 `Show { rect }`：

- 根据 `state.machine.visual()` / `anchor()` 生成目标 `VisualFrame`：
  - Ball：`radii = capsule(size/2)`，`opacity = 0.76`，`icon_alpha = 1`，`grip_alpha = 0`。
  - Strip：`radii = docked_strip(edge, thickness)`，`opacity = 0.646`，`icon_alpha = 0`，`icon_scale = 0.35`，`grip_alpha = 0.42`。
- 调 `start_transition(target, 420ms, curve)`：
  - 球↔胶囊用 `Curve::Spring`；自由拖动/尺寸变化用 `Curve::Standard`。
- 启动 `TIMER_ANIM`（16ms）。
- `state.position = rect.x,y`；`state.size` 保持 `base_size`。

- [ ] **Step 3: 帧循环与呈现**

```rust
fn on_anim_tick(hwnd: HWND, now: u64) {
    let Some(done) = with_state_mut(hwnd, |state| {
        if let Some(t) = &state.transition {
            let (frame, finished) = t.sample(now);
            state.frame = frame;
            if finished { state.transition = None; }
            Some(finished)
        } else { None }
    }) else { return; };

    render_current(hwnd);
    if done == Some(true) {
        let _ = unsafe { KillTimer(Some(hwnd), TIMER_ANIM) };
    }
}
```

`render_current`：计算 `window_rect(frame.rect, anchor, drop_active, metrics)`，拼 `PaintPayload`（图标/气泡/进度/光环角度），调用 `render_ball`。

- [ ] **Step 4: 窗口矩形与命中**

- `press_ball` 继续记录光标与当前视觉左上角；`drag_ball` 首次越过阈值时发送 `BallEvent::DragStarted`。
- 拖动中直接 `state.position = target` 并立即渲染（无过渡）。
- 松开后 `release_ball` 用视觉位置与 `size` 夹取、上报，再交给状态机。
- 窗口过程新增：

```rust
WM_NCHITTEST => {
    let mut point = POINT::default();
    let _ = unsafe { GetCursorPos(&mut point) };
    let (visual, anchor) = with_state(hwnd, |s| (s.frame.rect, s.machine.anchor()))
        .unwrap_or((PhysicalRect::new(0, 0, 1, 1), None));
    let m = BallMetrics::new(with_state(hwnd, |s| s.base_size).unwrap_or(48));
    let hit = hits_visual(visual, anchor, m, PhysicalPoint { x: point.x, y: point.y });
    LRESULT(if hit { HTCLIENT as isize } else { HTTRANSPARENT as isize })
}
```

- [ ] **Step 5: 拖入、反馈、主题**

- `refresh_drop_visual` 改为读取 `DropVisual` 后更新 `drop_active`、目标 `scale`（1.14/1.0）、`halo_alpha`、气泡文案；启动动画与光环定时器。
- `WM_BALL_FEEDBACK`：解包后设置 `feedback`；Busy 启动帧定时器；Success/Failure 设置 `feedback_until = now + 720/600` 并启动 `TIMER_FEEDBACK`。
- `TIMER_FEEDBACK` 到点复位为 Idle 并重绘。
- `WM_SETTINGCHANGE`：`state.theme = system_theme()` 并重绘。
- OLE `Drop` 时立即把 `feedback` 置为 `Busy { progress: None }`，不等待 app-core。
- 抓帧隐藏（`WM_BALL_SET_VISIBLE`）保持同步 `ShowWindow(SW_HIDE)`；隐藏时停掉动画定时器。

- [ ] **Step 6: 删除旧拖入缩放逻辑**

删除 `DROP_SCALE`、`drop_visual_bounds` 及其两个测试（由 Task 1 的几何测试替代）。`refresh_drop_visual` 的新实现不再改变 `state.size` / `state.position` 的语义。

- [ ] **Step 7: 编译与测试**

Run: `cargo test -p visrecall-shell`  
Expected: 全部 PASS。

---

### Task 8: 全量验证与文档

**Files:**
- Modify: `docs/superpowers/specs/2026-11-20-floating-ball-prototype-fidelity-design.md`（状态改为已实现）
- 可能修改：`docs/superpowers/plans/2026-11-20-floating-ball-prototype-fidelity.md`（勾选步骤）

- [ ] **Step 1: 全量自动化验证**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets
cargo test --workspace
pnpm typecheck
pnpm lint
pnpm test
```

Expected: 全部通过。

- [ ] **Step 2: 实机手测**

Run: `pnpm tauri dev`

逐项确认：

- 四边吸附、800ms 收起、悬停立即展开、移开 800ms 回缩
- 拖动跟手、1.08 放大、松手吸附
- 拖入光环/气泡、拒绝文案、灰色球体
- 多文件导入进度环推进、完成对勾
- 单击截图、抓帧时球隐藏、全屏隐藏、重启位置恢复
- 浅色/深色系统主题切换
- 40/48/56 三档比例
- 150% DPI 与多显示器

- [ ] **Step 3: 更新文档状态**

把 Spec 顶部状态改为「已实现（自动化测试通过；实机视觉验收结果见下）」，补充手测结果。

- [ ] **Step 4: 等待提交**

不自动 commit；用户确认后再按仓库约定提交。
