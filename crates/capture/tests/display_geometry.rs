//! 虚拟屏幕几何的纯逻辑测试：不触碰 Win32，可在任意平台跑。

use visrecall_capture::display::{display_for_point, normalize_position, virtual_bounds};
use visrecall_domain::capture::DisplayInfo;
use visrecall_domain::geometry::{PhysicalPoint, PhysicalRect};

fn display(id: &str, x: i32, y: i32, w: u32, h: u32) -> DisplayInfo {
    DisplayInfo {
        id: id.into(),
        bounds: PhysicalRect::new(x, y, w, h),
        work_area: PhysicalRect::new(x, y, w, h),
        dpi: 96,
        primary: id == "DISPLAY1",
    }
}

fn two_displays() -> Vec<DisplayInfo> {
    vec![
        display("DISPLAY1", 0, 0, 1920, 1080),
        display("DISPLAY2", -2560, 0, 2560, 1440),
    ]
}

#[test]
fn finds_display_with_negative_origin() {
    let ds = two_displays();
    assert_eq!(
        display_for_point(&ds, PhysicalPoint { x: -10, y: 10 })
            .unwrap()
            .id,
        "DISPLAY2"
    );
    assert_eq!(
        display_for_point(&ds, PhysicalPoint { x: 10, y: 10 })
            .unwrap()
            .id,
        "DISPLAY1"
    );
}

#[test]
fn display_for_point_uses_half_open_interval() {
    let ds = vec![
        display("LEFT", -100, 0, 100, 100),
        display("RIGHT", 0, 0, 100, 100),
    ];
    // 右/下边界属于相邻矩形，不属于本矩形。
    assert_eq!(
        display_for_point(&ds, PhysicalPoint { x: 0, y: 0 })
            .unwrap()
            .id,
        "RIGHT"
    );
    assert_eq!(
        display_for_point(&ds, PhysicalPoint { x: 99, y: 99 })
            .unwrap()
            .id,
        "RIGHT"
    );
    assert!(display_for_point(&ds, PhysicalPoint { x: 100, y: 0 }).is_none());
    assert!(display_for_point(&ds, PhysicalPoint { x: 0, y: 100 }).is_none());
}

#[test]
fn display_for_point_ignores_gap_between_displays() {
    let ds = vec![
        display("LEFT", -200, 0, 100, 100),
        display("RIGHT", 0, 0, 100, 100),
    ];
    assert!(display_for_point(&ds, PhysicalPoint { x: -50, y: 50 }).is_none());
}

#[test]
fn display_for_point_returns_none_for_empty_list() {
    assert!(display_for_point(&[], PhysicalPoint { x: 0, y: 0 }).is_none());
}

#[test]
fn virtual_bounds_cover_all_displays() {
    let ds = two_displays();
    assert_eq!(virtual_bounds(&ds), PhysicalRect::new(-2560, 0, 4480, 1440));
}

#[test]
fn virtual_bounds_cover_vertically_stacked_displays() {
    let ds = vec![
        display("TOP", 0, -1200, 1600, 1200),
        display("BOTTOM", 0, 0, 1920, 1080),
    ];
    assert_eq!(virtual_bounds(&ds), PhysicalRect::new(0, -1200, 1920, 2280));
}

#[test]
fn virtual_bounds_of_empty_list_is_default_rect() {
    assert_eq!(virtual_bounds(&[]), PhysicalRect::default());
}

#[test]
fn normalize_position_is_relative_to_display_origin() {
    let d = display("DISPLAY2", -2560, -100, 2560, 1440);
    let (nx, ny) = normalize_position(&d, PhysicalPoint { x: -1280, y: 620 });
    assert!((nx - 0.5).abs() < 1e-6, "unexpected nx: {nx}");
    assert!((ny - 0.5).abs() < 1e-6, "unexpected ny: {ny}");

    // 原点归一化为 0。
    assert_eq!(
        normalize_position(&d, PhysicalPoint { x: -2560, y: -100 }),
        (0.0, 0.0)
    );
}

#[test]
fn normalize_position_clamps_nothing_but_stays_finite_for_zero_size() {
    let d = display("EMPTY", 100, 200, 0, 0);
    let (nx, ny) = normalize_position(&d, PhysicalPoint { x: 150, y: 250 });
    assert_eq!((nx, ny), (0.0, 0.0));
    assert!(!nx.is_nan() && !ny.is_nan());
}
