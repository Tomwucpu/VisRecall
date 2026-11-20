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
    assert!(hits_visual(
        visual,
        None,
        m,
        PhysicalPoint { x: 128, y: 128 }
    ));
    assert!(!hits_visual(
        visual,
        None,
        m,
        PhysicalPoint { x: 100, y: 100 }
    ));
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
        PhysicalPoint {
            x: 12 + 56 + 15,
            y: 228
        }
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
    assert_eq!(
        left,
        CornerRadii {
            tl: 0.0,
            tr: 5.0,
            br: 5.0,
            bl: 0.0
        }
    );
    let top = CornerRadii::docked_strip(BallEdge::Top, 10.0);
    assert_eq!(
        top,
        CornerRadii {
            tl: 0.0,
            tr: 0.0,
            br: 5.0,
            bl: 5.0
        }
    );
}

#[test]
fn tooltip_rect_survives_a_work_area_narrower_than_the_text() {
    let m = BallMetrics::new(56);
    let work = PhysicalRect::new(0, 0, 84, 300);
    let visual = PhysicalRect::new(14, 100, 56, 56);
    let tip = tooltip_rect(40, m, visual, work);
    assert!(tip.x >= work.x);
    assert!(tip.width <= work.width);
}
