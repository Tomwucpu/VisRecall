use proptest::prelude::*;
use visrecall_domain::geometry::{PhysicalPoint, PhysicalRect};

#[test]
fn normalizes_bottom_right_drag() {
    let r = PhysicalRect::from_points(
        PhysicalPoint { x: 110, y: 220 },
        PhysicalPoint { x: 10, y: 20 },
    );
    assert_eq!(r, PhysicalRect::new(10, 20, 100, 200));
}

#[test]
fn intersect_returns_none_for_disjoint() {
    let a = PhysicalRect::new(0, 0, 10, 10);
    let b = PhysicalRect::new(20, 20, 5, 5);
    assert_eq!(a.intersect(&b), None);
}

#[test]
fn intersect_clips_to_overlap() {
    let a = PhysicalRect::new(0, 0, 100, 100);
    let b = PhysicalRect::new(80, 90, 40, 40);
    assert_eq!(a.intersect(&b), Some(PhysicalRect::new(80, 90, 20, 10)));
}

#[test]
fn contains_uses_half_open_edges() {
    let r = PhysicalRect::new(0, 0, 10, 10);
    assert!(r.contains(PhysicalPoint { x: 0, y: 0 }));
    assert!(r.contains(PhysicalPoint { x: 9, y: 9 }));
    assert!(!r.contains(PhysicalPoint { x: 10, y: 9 }));
}

proptest! {
    #[test]
    fn intersect_is_within_both(a in -1000i32..1000, b in -1000i32..1000, w in 0u32..500, h in 0u32..500) {
        let r1 = PhysicalRect::new(a, b, w, h);
        let r2 = PhysicalRect::new(a + 10, b + 10, w, h);
        if let Some(i) = r1.intersect(&r2) {
            prop_assert!(i.width <= r1.width && i.width <= r2.width);
            prop_assert!(i.height <= r1.height && i.height <= r2.height);
        }
    }

    #[test]
    fn intersect_result_is_contained_in_both(
        ax in -2000i32..2000, ay in -2000i32..2000,
        aw in 0u32..800, ah in 0u32..800,
        bx in -2000i32..2000, by in -2000i32..2000,
        bw in 0u32..800, bh in 0u32..800,
    ) {
        let r1 = PhysicalRect::new(ax, ay, aw, ah);
        let r2 = PhysicalRect::new(bx, by, bw, bh);
        let Some(i) = r1.intersect(&r2) else {
            return Ok(());
        };
        prop_assert!(!i.is_empty());
        // 半开区间：交集的左上/右下角点都落在两个矩形内。
        let corners = [
            PhysicalPoint { x: i.x, y: i.y },
            PhysicalPoint { x: i.right() - 1, y: i.bottom() - 1 },
        ];
        for p in corners {
            prop_assert!(r1.contains(p), "corner {:?} not in {:?}", p, r1);
            prop_assert!(r2.contains(p), "corner {:?} not in {:?}", p, r2);
        }
    }

    #[test]
    fn from_points_is_normalized_and_commutative(ax in -2000i32..2000, ay in -2000i32..2000, bx in -2000i32..2000, by in -2000i32..2000) {
        let a = PhysicalPoint { x: ax, y: ay };
        let b = PhysicalPoint { x: bx, y: by };
        let r = PhysicalRect::from_points(a, b);
        prop_assert_eq!(r, PhysicalRect::from_points(b, a));
        prop_assert_eq!(r.width, ax.abs_diff(bx));
        prop_assert_eq!(r.height, ay.abs_diff(by));
        let min_corner = PhysicalPoint { x: ax.min(bx), y: ay.min(by) };
        prop_assert!(r.contains(min_corner) || r.is_empty());
    }
}
