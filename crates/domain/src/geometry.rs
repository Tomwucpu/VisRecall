//! 物理像素几何类型。
//!
//! 全部坐标为虚拟屏幕物理像素（Per-Monitor V2 DPI 感知），原点为主显示器左上角，
//! 副屏可产生负坐标。矩形一律使用半开区间 `[x, x + width)` / `[y, y + height)`，
//! 因此相邻矩形不会重叠，`contains` 不含右/下边界。

use serde::{Deserialize, Serialize};

/// 物理像素坐标点。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhysicalPoint {
    pub x: i32,
    pub y: i32,
}

/// 物理像素矩形，半开区间 `[x, x + width)`。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhysicalRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl PhysicalRect {
    pub const fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// 由拖拽的两个对角点构造，自动归一化负宽高（从右下拖到左上同样成立）。
    pub fn from_points(a: PhysicalPoint, b: PhysicalPoint) -> Self {
        Self {
            x: a.x.min(b.x),
            y: a.y.min(b.y),
            width: a.x.abs_diff(b.x),
            height: a.y.abs_diff(b.y),
        }
    }

    /// 右边界（不含），饱和到 `i32` 范围。
    pub fn right(&self) -> i32 {
        clamp_i32(self.x as i64 + self.width as i64)
    }

    /// 下边界（不含），饱和到 `i32` 范围。
    pub fn bottom(&self) -> i32 {
        clamp_i32(self.y as i64 + self.height as i64)
    }

    /// 宽或高为 0 即视为空矩形。
    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// 半开区间判定：右/下边界不属于本矩形。
    pub fn contains(&self, p: PhysicalPoint) -> bool {
        let (x0, y0, x1, y1) = self.edges();
        (p.x as i64) >= x0 && (p.x as i64) < x1 && (p.y as i64) >= y0 && (p.y as i64) < y1
    }

    /// 求交集；任一维度为 0 或无重叠时返回 `None`。
    pub fn intersect(&self, other: &Self) -> Option<Self> {
        let (ax0, ay0, ax1, ay1) = self.edges();
        let (bx0, by0, bx1, by1) = other.edges();
        let x0 = ax0.max(bx0);
        let y0 = ay0.max(by0);
        let x1 = ax1.min(bx1);
        let y1 = ay1.min(by1);
        if x1 <= x0 || y1 <= y0 {
            return None;
        }
        Some(Self::from_edges(x0, y0, x1, y1))
    }

    /// 求并集；空矩形不参与扩张。
    pub fn union(&self, other: &Self) -> Self {
        if self.is_empty() {
            return *other;
        }
        if other.is_empty() {
            return *self;
        }
        let (ax0, ay0, ax1, ay1) = self.edges();
        let (bx0, by0, bx1, by1) = other.edges();
        Self::from_edges(ax0.min(bx0), ay0.min(by0), ax1.max(bx1), ay1.max(by1))
    }

    /// 四边各向外扩张 `dx`/`dy`（负值内缩，宽高饱和到 0）。
    pub fn inflate(&self, dx: i32, dy: i32) -> Self {
        let (x0, y0, x1, y1) = self.edges();
        let dx = dx as i64;
        let dy = dy as i64;
        let nx0 = (x0 - dx).max(i32::MIN as i64);
        let ny0 = (y0 - dy).max(i32::MIN as i64);
        let nx1 = (x1 + dx).min(i32::MAX as i64);
        let ny1 = (y1 + dy).min(i32::MAX as i64);
        if nx1 <= nx0 || ny1 <= ny0 {
            return Self::new(clamp_i32(nx0), clamp_i32(ny0), 0, 0);
        }
        Self::from_edges(nx0, ny0, nx1, ny1)
    }

    /// 半开区间下的四条边：`(left, top, right, bottom)`，用 `i64` 避免溢出。
    fn edges(&self) -> (i64, i64, i64, i64) {
        let x0 = self.x as i64;
        let y0 = self.y as i64;
        (x0, y0, x0 + self.width as i64, y0 + self.height as i64)
    }

    /// 由 `i64` 边构造，调用方保证 `x1 >= x0`、`y1 >= y0`。
    fn from_edges(x0: i64, y0: i64, x1: i64, y1: i64) -> Self {
        Self {
            x: clamp_i32(x0),
            y: clamp_i32(y0),
            width: clamp_u32(x1 - x0),
            height: clamp_u32(y1 - y0),
        }
    }
}

fn clamp_i32(v: i64) -> i32 {
    v.clamp(i32::MIN as i64, i32::MAX as i64) as i32
}

fn clamp_u32(v: i64) -> u32 {
    v.clamp(0, u32::MAX as i64) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_rect_has_no_area() {
        assert!(PhysicalRect::new(5, 5, 0, 10).is_empty());
        assert!(PhysicalRect::new(5, 5, 10, 0).is_empty());
    }

    #[test]
    fn intersect_none_when_one_side_touches() {
        let a = PhysicalRect::new(0, 0, 10, 10);
        let b = PhysicalRect::new(10, 0, 10, 10);
        assert_eq!(a.intersect(&b), None);
    }

    #[test]
    fn intersect_none_for_zero_size() {
        let a = PhysicalRect::new(0, 0, 0, 10);
        let b = PhysicalRect::new(0, 0, 10, 10);
        assert_eq!(a.intersect(&b), None);
    }

    #[test]
    fn union_covers_both() {
        let a = PhysicalRect::new(0, 0, 10, 10);
        let b = PhysicalRect::new(-5, 5, 10, 10);
        assert_eq!(a.union(&b), PhysicalRect::new(-5, 0, 15, 15));
    }

    #[test]
    fn union_ignores_empty() {
        let a = PhysicalRect::new(0, 0, 10, 10);
        let empty = PhysicalRect::new(100, 100, 0, 0);
        assert_eq!(a.union(&empty), a);
        assert_eq!(empty.union(&a), a);
    }

    #[test]
    fn inflate_grows_and_shrinks() {
        let r = PhysicalRect::new(10, 20, 30, 40);
        assert_eq!(r.inflate(5, 5), PhysicalRect::new(5, 15, 40, 50));
        assert_eq!(r.inflate(-5, -5), PhysicalRect::new(15, 25, 20, 30));
        assert_eq!(r.inflate(-100, -100), PhysicalRect::new(110, 120, 0, 0));
    }

    #[test]
    fn right_and_bottom_saturate() {
        let r = PhysicalRect::new(i32::MAX - 1, i32::MAX - 1, 10, 10);
        assert_eq!(r.right(), i32::MAX);
        assert_eq!(r.bottom(), i32::MAX);
    }

    #[test]
    fn from_points_handles_negative_coordinates() {
        let r = PhysicalRect::from_points(
            PhysicalPoint { x: -200, y: -100 },
            PhysicalPoint { x: -100, y: 100 },
        );
        assert_eq!(r, PhysicalRect::new(-200, -100, 100, 200));
    }

    #[test]
    fn from_points_same_point_is_empty() {
        let p = PhysicalPoint { x: 7, y: 9 };
        let r = PhysicalRect::from_points(p, p);
        assert!(r.is_empty());
    }
}
