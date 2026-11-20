//! 悬浮球视觉度量、四边几何与缓动（纯逻辑，不触碰窗口）。
//!
//! 原型以 56px 球径为基准，这里把相关常量按 `size / 56` 缩放，保证 40/48/56
//! 三档观感一致。窗口矩形与命中矩形分开计算：窗口包含阴影/气泡留白，
//! 命中只覆盖球体或贴边后的胶囊（含外扩留白）。

use visrecall_domain::geometry::{PhysicalPoint, PhysicalRect};

use crate::ball::state::BallEdge;

/// 原型 56px 基准下的视觉度量。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BallMetrics {
    pub size: u32,
    /// 距工作区边缘多少像素内松手视为贴边。
    pub threshold: i32,
    /// 收起胶囊的厚度。
    pub strip_thickness: u32,
    /// 收起胶囊的长度（球径 + 6）。
    pub strip_length: u32,
    /// 贴边状态下向工作区外侧扩展的命中外扩。
    pub hit_padding: u32,
    /// 展开态距工作区边缘的内缩量。
    pub edge_offset: u32,
    /// 视觉范围外的透明留白（拖入光环、1.14 放大与命中）。
    pub shadow_margin: u32,
    /// 拖入时在窗口顶部预留的气泡高度。
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

/// 四角独立圆角；胶囊条只圆屏幕一侧。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CornerRadii {
    pub tl: f32,
    pub tr: f32,
    pub br: f32,
    pub bl: f32,
}

impl CornerRadii {
    pub fn capsule(radius: f32) -> Self {
        Self {
            tl: radius,
            tr: radius,
            br: radius,
            bl: radius,
        }
    }

    pub fn docked_strip(edge: BallEdge, thickness: f32) -> Self {
        let r = thickness / 2.0;
        match edge {
            BallEdge::Left => Self {
                tl: 0.0,
                tr: r,
                br: r,
                bl: 0.0,
            },
            BallEdge::Right => Self {
                tl: r,
                tr: 0.0,
                br: 0.0,
                bl: r,
            },
            BallEdge::Top => Self {
                tl: 0.0,
                tr: 0.0,
                br: r,
                bl: r,
            },
            BallEdge::Bottom => Self {
                tl: r,
                tr: r,
                br: 0.0,
                bl: 0.0,
            },
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

/// 窗口矩形：视觉范围 + 阴影留白；贴边时外侧扩展命中留白；拖入时顶部预留气泡。
pub fn window_rect(
    visual: PhysicalRect,
    anchor: Option<BallEdge>,
    drop_active: bool,
    metrics: BallMetrics,
) -> PhysicalRect {
    let shadow = metrics.shadow_margin as i32;
    let padding = metrics.hit_padding as i32;
    let mut left = visual.x - shadow;
    let mut top = visual.y - shadow;
    let mut right = visual.right() + shadow;
    let mut bottom = visual.bottom() + shadow;

    if let Some(edge) = anchor {
        match edge {
            BallEdge::Left => left = visual.x - padding,
            BallEdge::Right => right = visual.right() + padding,
            BallEdge::Top => top = visual.y - padding,
            BallEdge::Bottom => bottom = visual.bottom() + padding,
        }
    }
    if drop_active {
        top -= metrics.tooltip_space as i32;
    }

    PhysicalRect::new(
        left,
        top,
        (right - left).max(1) as u32,
        (bottom - top).max(1) as u32,
    )
}

/// 命中测试：自由态只命中圆；贴边态命中矩形 + 四周 14px 留白。
///
/// 贴边态保留外扩命中，是为了让展开后停在边缘的鼠标仍算「悬停」，
/// 不会立刻触发 `WM_MOUSELEAVE` 导致收起。
pub fn hits_visual(
    visual: PhysicalRect,
    anchor: Option<BallEdge>,
    metrics: BallMetrics,
    point: PhysicalPoint,
) -> bool {
    if anchor.is_some() {
        let padding = metrics.hit_padding as i32;
        return point.x >= visual.x - padding
            && point.x < visual.right() + padding
            && point.y >= visual.y - padding
            && point.y < visual.bottom() + padding;
    }

    let center_x = visual.x as f32 + visual.width as f32 / 2.0;
    let center_y = visual.y as f32 + visual.height as f32 / 2.0;
    let dx = point.x as f32 - center_x;
    let dy = point.y as f32 - center_y;
    let radius = visual.width as f32 / 2.0;
    dx * dx + dy * dy <= radius * radius
}

/// 气泡宽度估算（与 [`tooltip_rect`] 共用，窗口按它加宽）。
pub fn tooltip_width(text_chars: usize, metrics: BallMetrics) -> u32 {
    let font = 11.0 * metrics.factor();
    let padding_x = 11.0 * metrics.factor();
    (text_chars as f32 * font * 0.95 + padding_x * 2.0)
        .round()
        .max(52.0) as u32
}

/// 气泡矩形：水平居中于视觉范围，夹在工作区内，默认放在球体上方。
///
/// 文案可能比窗口还宽（例如拒绝原因），这里把宽度上限压到工作区宽度，
/// 并保证 `clamp` 的上下界有序——否则会触发 `min <= max` 断言并让窗口线程崩溃。
pub fn tooltip_rect(
    text_chars: usize,
    metrics: BallMetrics,
    visual: PhysicalRect,
    work_area: PhysicalRect,
) -> PhysicalRect {
    let font = 11.0 * metrics.factor();
    let height = (font + 10.0 * metrics.factor()).round().max(18.0) as u32;
    let max_width = (work_area.width as i32 - 8).max(1) as u32;
    let width = tooltip_width(text_chars, metrics).min(max_width);
    let gap = (14.0 * metrics.factor()).round() as i32;

    let center = visual.x + visual.width as i32 / 2;
    let min_x = work_area.x + 4;
    let max_x = (work_area.right() - width as i32 - 4).max(min_x);
    let x = (center - width as i32 / 2).clamp(min_x, max_x);
    let y = (visual.y - gap - height as i32).max(work_area.y + 4);
    PhysicalRect::new(x, y, width, height)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Curve {
    Standard,
    Spring,
    Linear,
}

/// 求解 cubic-bezier 的 y(x)：先二分反解控制参数 u，再取 y(u)。
fn cubic_bezier(x1: f32, y1: f32, x2: f32, y2: f32, t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    let sample = |u: f32, a: f32, b: f32| {
        let v = 1.0 - u;
        3.0 * v * v * u * a + 3.0 * v * u * u * b + u * u * u
    };

    let mut low = 0.0;
    let mut high = 1.0;
    for _ in 0..24 {
        let mid = (low + high) / 2.0;
        if sample(mid, x1, x2) < t {
            low = mid;
        } else {
            high = mid;
        }
    }
    sample((low + high) / 2.0, y1, y2)
}

pub fn curve_value(curve: Curve, t: f32) -> f32 {
    match curve {
        Curve::Linear => t.clamp(0.0, 1.0),
        Curve::Standard => cubic_bezier(0.22, 1.0, 0.36, 1.0, t),
        Curve::Spring => cubic_bezier(0.34, 1.38, 0.64, 1.0, t),
    }
}

/// 可插值的视觉帧；图标种类、气泡文案、进度值等离散状态不在这里。
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
                mix(self.rect.width as f32, to.rect.width as f32)
                    .round()
                    .max(1.0) as u32,
                mix(self.rect.height as f32, to.rect.height as f32)
                    .round()
                    .max(1.0) as u32,
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

/// 一次形态切换；`sample` 在到点后返回 `true`，调用方据此停表。
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
        let raw = ((now_ms.saturating_sub(self.start_ms)) as f32 / self.duration_ms as f32)
            .clamp(0.0, 1.0);
        if raw >= 1.0 {
            // 到点直接返回目标，避免浮点插值留下 1e-7 级残差。
            return (self.to, true);
        }
        (self.from.lerp(self.to, curve_value(self.curve, raw)), false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transition_reaches_the_target_at_the_deadline() {
        let from = VisualFrame {
            rect: PhysicalRect::new(0, 0, 10, 10),
            radii: CornerRadii::capsule(5.0),
            scale: 1.0,
            opacity: 1.0,
            icon_alpha: 1.0,
            icon_scale: 1.0,
            grip_alpha: 0.0,
            halo_alpha: 0.0,
            tooltip_alpha: 0.0,
        };
        let to = VisualFrame {
            rect: PhysicalRect::new(12, 10, 56, 56),
            radii: CornerRadii::capsule(28.0),
            scale: 1.08,
            opacity: 0.76,
            icon_alpha: 0.0,
            icon_scale: 0.35,
            grip_alpha: 0.42,
            halo_alpha: 1.0,
            tooltip_alpha: 0.0,
        };
        let transition = Transition {
            from,
            to,
            start_ms: 100,
            duration_ms: 400,
            curve: Curve::Spring,
        };

        let (frame, finished) = transition.sample(500);
        assert!(finished);
        assert_eq!(frame, to);

        let (mid, finished) = transition.sample(300);
        assert!(!finished);
        assert!(mid.rect.width >= 10);
    }
}
