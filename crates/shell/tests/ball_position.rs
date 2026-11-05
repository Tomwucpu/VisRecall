//! 悬浮球位置几何的集成测试（Task 15）。
//!
//! 只测纯函数：`normalize_position`（Task 7 的记忆坐标）与 `clamp_ball_position`
//! （本任务新增的工作区夹取）。真实窗口/线程不在这里创建，手工验证见任务报告。

use visrecall_capture::display::normalize_position;
use visrecall_domain::capture::DisplayInfo;
use visrecall_domain::geometry::{PhysicalPoint, PhysicalRect};
use visrecall_shell::ball::clamp_ball_position;

fn display(id: &str, bounds: PhysicalRect, work_area: PhysicalRect, primary: bool) -> DisplayInfo {
    DisplayInfo {
        id: id.into(),
        bounds,
        work_area,
        dpi: 96,
        primary,
    }
}

fn primary_display() -> DisplayInfo {
    display(
        "\\\\.\\DISPLAY1",
        PhysicalRect::new(0, 0, 1920, 1080),
        // 底部 40px 任务栏。
        PhysicalRect::new(0, 0, 1920, 1040),
        true,
    )
}

#[test]
fn normalized_position_roundtrips() {
    let d = DisplayInfo {
        id: "DISPLAY1".into(),
        bounds: PhysicalRect::new(100, 50, 1000, 800),
        work_area: PhysicalRect::new(100, 50, 1000, 800),
        dpi: 96,
        primary: true,
    };
    let (nx, ny) = normalize_position(&d, PhysicalPoint { x: 600, y: 450 });
    assert!((nx - 0.5).abs() < 0.001);
    assert!((ny - 0.5).abs() < 0.001);
}

#[test]
fn normalized_position_handles_negative_origins() {
    let d = display(
        "\\\\.\\DISPLAY2",
        PhysicalRect::new(-1920, 0, 1920, 1080),
        PhysicalRect::new(-1920, 0, 1920, 1080),
        false,
    );
    let (nx, ny) = normalize_position(&d, PhysicalPoint { x: -960, y: 540 });
    assert!((nx - 0.5).abs() < 0.001);
    assert!((ny - 0.5).abs() < 0.001);
}

#[test]
fn inside_the_work_area_is_kept_as_is() {
    let displays = [primary_display()];
    let desired = PhysicalPoint { x: 1200, y: 800 };
    assert_eq!(clamp_ball_position(&displays, desired, 48), desired);
}

#[test]
fn clamps_beyond_the_bottom_right_corner() {
    let displays = [primary_display()];
    assert_eq!(
        clamp_ball_position(&displays, PhysicalPoint { x: 5000, y: 5000 }, 48),
        PhysicalPoint {
            x: 1920 - 48,
            // 任务栏之上：使用 work_area 而不是 bounds。
            y: 1040 - 48,
        }
    );
}

#[test]
fn clamps_negative_coordinates_of_a_secondary_display() {
    let displays = [
        display(
            "\\\\.\\DISPLAY2",
            PhysicalRect::new(-1920, -200, 1920, 1200),
            PhysicalRect::new(-1920, -200, 1920, 1160),
            false,
        ),
        primary_display(),
    ];
    assert_eq!(
        clamp_ball_position(&displays, PhysicalPoint { x: -3000, y: -500 }, 48),
        PhysicalPoint { x: -1920, y: -200 }
    );
}

#[test]
fn clamps_within_the_taskbar_work_area() {
    let displays = [primary_display()];
    let clamped = clamp_ball_position(&displays, PhysicalPoint { x: 100, y: 1030 }, 48);
    assert_eq!(clamped.y, 1040 - 48);
    assert!(clamped.y + 48 <= 1040);
}

#[test]
fn falls_back_to_the_desired_position_without_displays() {
    let desired = PhysicalPoint { x: -7, y: 13 };
    assert_eq!(clamp_ball_position(&[], desired, 48), desired);
}
