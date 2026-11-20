//! 悬浮球三态状态机的时序测试（纯逻辑，跳时间验证）。
//!
//! 这些用例对应原型复刻的验收点：贴边 64px 收起、65px 不收、悬停立即展开、
//! 离开 800ms 回缩、全屏连续 3 次采样才隐藏、关闭贴边后永不收起、尺寸热更新
//! 保持中心点且不越出工作区。

use visrecall_domain::geometry::{PhysicalPoint, PhysicalRect};
use visrecall_shell::ball::state::{
    BallAction, BallEdge, BallEvent, BallSettings, BallStateMachine, BallVisual, COLLAPSE_DELAY_MS,
    FULLSCREEN_TICKS, RETRACT_DELAY_MS,
};

const SIZE: u32 = 56;
const WORK: PhysicalRect = PhysicalRect {
    x: 0,
    y: 0,
    width: 1920,
    height: 1080,
};

fn settings() -> BallSettings {
    BallSettings {
        size: SIZE,
        edge_hide: true,
        hide_in_fullscreen: true,
    }
}

/// 距左边缘 `offset` 像素处的球左上角。
fn at_left(offset: i32) -> PhysicalPoint {
    PhysicalPoint { x: offset, y: 400 }
}

fn machine() -> BallStateMachine {
    BallStateMachine::new(
        settings(),
        // 起始位置在屏幕中间，远离任何边缘。
        PhysicalPoint { x: 900, y: 500 },
        WORK,
        0,
    )
}

/// 松手并推进到「收起定时器到点」，返回这两个事件产生的全部动作。
fn release_and_settle(
    machine: &mut BallStateMachine,
    position: PhysicalPoint,
    now: u64,
) -> Vec<BallAction> {
    let mut actions = machine.handle(BallEvent::Released {
        position,
        work_area: WORK,
        now_ms: now,
    });
    actions.extend(machine.handle(BallEvent::Tick {
        now_ms: now + COLLAPSE_DELAY_MS,
        fullscreen: false,
    }));
    actions
}

#[test]
fn released_within_64px_collapses_into_strip() {
    let mut m = machine();
    let actions = release_and_settle(&mut m, at_left(64), 1_000);
    assert!(
        actions
            .iter()
            .any(|action| matches!(action, BallAction::ScheduleTimer { .. })),
        "releasing 64px from the edge must schedule the collapse timer"
    );

    assert_eq!(m.visual(), BallVisual::Strip);
    assert_eq!(m.anchor(), Some(BallEdge::Left));

    // 胶囊条 = 10px 厚 × 62px 长，紧贴工作区左边界。
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
    // 只会重画球本身，不会出现胶囊尺寸的 Show。
    assert!(
        actions.iter().all(|action| match action {
            BallAction::Show { rect } => rect.width == SIZE && rect.height == SIZE,
            BallAction::ScheduleTimer { .. } => false,
            _ => true,
        }),
        "no strip transition expected away from the edge, got {actions:?}"
    );
}

#[test]
fn hovering_the_strip_expands_immediately() {
    let mut m = machine();
    release_and_settle(&mut m, at_left(0), 1_000);
    assert_eq!(m.visual(), BallVisual::Strip);

    let actions = m.handle(BallEvent::CursorEnter { now_ms: 2_000 });
    assert!(matches!(actions.as_slice(), [BallAction::Show { .. }]));
    assert_eq!(m.visual(), BallVisual::Ball);

    // 展开态距工作区边缘 12px，窗口/热区仍贴着边缘。
    let rect = m.current_rect();
    assert_eq!(rect.x, WORK.x + 12);
    assert_eq!(rect.width, SIZE);
    assert_eq!(rect.height, SIZE);
}

#[test]
fn leaving_the_expanded_ball_retracts_after_800ms() {
    let mut m = machine();
    release_and_settle(&mut m, at_left(0), 1_000);
    m.handle(BallEvent::CursorEnter { now_ms: 2_000 });
    assert_eq!(m.visual(), BallVisual::Ball);

    let actions = m.handle(BallEvent::CursorLeave { now_ms: 3_000 });
    assert!(matches!(
        actions.as_slice(),
        [BallAction::ScheduleTimer { after_ms }] if *after_ms == RETRACT_DELAY_MS
    ));
    assert_eq!(RETRACT_DELAY_MS, 800);

    let actions = m.handle(BallEvent::Tick {
        now_ms: 3_000 + RETRACT_DELAY_MS,
        fullscreen: false,
    });
    assert!(matches!(actions.as_slice(), [BallAction::Show { .. }]));
    assert_eq!(m.visual(), BallVisual::Strip);
}

#[test]
fn hovering_during_the_collapse_delay_cancels_it() {
    let mut m = machine();
    m.handle(BallEvent::Released {
        position: at_left(0),
        work_area: WORK,
        now_ms: 1_000,
    });
    // 松手后立刻回到球上（热区外扩 14px 也算命中）。
    m.handle(BallEvent::CursorEnter { now_ms: 1_100 });

    let actions = m.handle(BallEvent::Tick {
        now_ms: 1_000 + COLLAPSE_DELAY_MS,
        fullscreen: false,
    });
    assert!(actions.is_empty(), "hovering must keep the ball expanded");
    assert_eq!(m.visual(), BallVisual::Ball);
}

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
        PhysicalPoint {
            x: 900,
            y: WORK.bottom() - SIZE as i32,
        },
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
    let rect = m.current_rect();
    assert_eq!(rect.width, SIZE);
    assert_eq!(rect.height, SIZE);
}

#[test]
fn fullscreen_hides_only_after_three_consecutive_samples() {
    let mut m = machine();
    release_and_settle(&mut m, at_left(0), 1_000);
    assert_eq!(m.visual(), BallVisual::Strip);

    for hit in 1..FULLSCREEN_TICKS {
        let actions = m.handle(BallEvent::Tick {
            now_ms: 5_000 + u64::from(hit),
            fullscreen: true,
        });
        assert!(
            actions.is_empty(),
            "sample {hit} must not hide the ball yet"
        );
        assert_eq!(m.visual(), BallVisual::Strip);
    }

    let actions = m.handle(BallEvent::Tick {
        now_ms: 6_000,
        fullscreen: true,
    });
    assert_eq!(actions, vec![BallAction::Hide]);
    assert_eq!(m.visual(), BallVisual::Hidden);
}

#[test]
fn a_single_non_fullscreen_sample_resets_the_streak() {
    let mut m = machine();
    m.handle(BallEvent::Tick {
        now_ms: 1,
        fullscreen: true,
    });
    m.handle(BallEvent::Tick {
        now_ms: 2,
        fullscreen: true,
    });
    m.handle(BallEvent::Tick {
        now_ms: 3,
        fullscreen: false,
    });
    // 重新计数：再来两次还不够。
    m.handle(BallEvent::Tick {
        now_ms: 4,
        fullscreen: true,
    });
    let actions = m.handle(BallEvent::Tick {
        now_ms: 5,
        fullscreen: true,
    });
    assert!(actions.is_empty());
    assert_eq!(m.visual(), BallVisual::Ball);
}

#[test]
fn leaving_fullscreen_restores_the_anchored_form() {
    let mut m = machine();
    release_and_settle(&mut m, at_left(0), 1_000);
    for tick in 0..FULLSCREEN_TICKS {
        m.handle(BallEvent::Tick {
            now_ms: 5_000 + u64::from(tick),
            fullscreen: true,
        });
    }
    assert_eq!(m.visual(), BallVisual::Hidden);

    let actions = m.handle(BallEvent::Tick {
        now_ms: 9_000,
        fullscreen: false,
    });
    assert!(matches!(actions.as_slice(), [BallAction::Show { .. }]));
    // 仍贴边 → 回到长条，而不是球。
    assert_eq!(m.visual(), BallVisual::Strip);
}

#[test]
fn disabling_edge_hide_never_collapses() {
    let mut m = BallStateMachine::new(
        BallSettings {
            edge_hide: false,
            ..settings()
        },
        PhysicalPoint { x: 900, y: 500 },
        WORK,
        0,
    );

    let actions = m.handle(BallEvent::Released {
        position: at_left(0),
        work_area: WORK,
        now_ms: 1_000,
    });
    assert!(
        !actions
            .iter()
            .any(|action| matches!(action, BallAction::ScheduleTimer { .. })),
        "no collapse timer when edge hide is off"
    );

    m.handle(BallEvent::Tick {
        now_ms: 1_000 + COLLAPSE_DELAY_MS,
        fullscreen: false,
    });
    assert_eq!(m.visual(), BallVisual::Ball);
    assert_eq!(m.anchor(), None);
}

#[test]
fn shrinking_keeps_the_center_and_stays_inside_the_work_area() {
    let mut m = BallStateMachine::new(
        BallSettings {
            size: 56,
            ..settings()
        },
        // 贴着右下角，缩小时中心点仍应留在工作区内。
        PhysicalPoint {
            x: WORK.right() - 30,
            y: WORK.bottom() - 30,
        },
        WORK,
        0,
    );
    let center_before = m.center();

    let actions = m.handle(BallEvent::ConfigChanged {
        settings: BallSettings {
            size: 50,
            ..settings()
        },
    });

    let resize = actions
        .iter()
        .find_map(|action| match action {
            BallAction::Resize { size, center } => Some((*size, *center)),
            _ => None,
        })
        .expect("size change must emit Resize");
    assert_eq!(resize.0, 50);
    assert_eq!(resize.1, m.center());
    // 中心点只会被夹回工作区，不会主动漂移。
    assert!(resize.1.x <= center_before.x.max(resize.1.x));

    let rect = m.current_rect();
    assert!(rect.x >= WORK.x);
    assert!(rect.y >= WORK.y);
    assert!(rect.right() <= WORK.right());
    assert!(rect.bottom() <= WORK.bottom());
}

#[test]
fn size_is_clamped_to_the_supported_range() {
    let mut m = machine();
    for (requested, expected) in [(40, 50), (49, 50), (100, 100), (151, 150), (999, 150)] {
        m.handle(BallEvent::ConfigChanged {
            settings: BallSettings {
                size: requested,
                ..settings()
            },
        });
        assert_eq!(m.size(), expected, "size {requested}");
    }
}

#[test]
fn turning_edge_hide_off_expands_an_existing_strip() {
    let mut m = machine();
    release_and_settle(&mut m, at_left(0), 1_000);
    assert_eq!(m.visual(), BallVisual::Strip);

    let actions = m.handle(BallEvent::ConfigChanged {
        settings: BallSettings {
            edge_hide: false,
            ..settings()
        },
    });

    assert_eq!(m.visual(), BallVisual::Ball);
    assert!(actions
        .iter()
        .any(|action| matches!(action, BallAction::Show { rect } if rect.width == SIZE)));
}

#[test]
fn session_hidden_overrides_visibility_without_changing_the_form() {
    let mut m = machine();
    release_and_settle(&mut m, at_left(0), 1_000);

    assert_eq!(
        m.handle(BallEvent::SessionHidden(true)),
        vec![BallAction::Hide]
    );
    assert_eq!(m.visual(), BallVisual::Strip);

    let actions = m.handle(BallEvent::SessionHidden(false));
    assert_eq!(actions.len(), 1);
    assert_eq!(m.visual(), BallVisual::Strip);
}

#[test]
fn dragging_updates_the_center_without_changing_the_form() {
    let mut m = machine();
    m.handle(BallEvent::Dragged {
        position: PhysicalPoint { x: 100, y: 200 },
    });
    assert_eq!(m.visual(), BallVisual::Ball);
    assert_eq!(
        m.center(),
        PhysicalPoint {
            x: 100 + SIZE as i32 / 2,
            y: 200 + SIZE as i32 / 2
        }
    );
}

#[test]
fn a_work_area_smaller_than_the_strip_does_not_panic() {
    let tiny = PhysicalRect::new(0, 0, 20, 20);
    let mut m = BallStateMachine::new(settings(), PhysicalPoint { x: 10, y: 10 }, tiny, 0);
    m.handle(BallEvent::Released {
        position: PhysicalPoint { x: 0, y: 0 },
        work_area: tiny,
        now_ms: 1_000,
    });
    m.handle(BallEvent::Tick {
        now_ms: 1_000 + COLLAPSE_DELAY_MS,
        fullscreen: false,
    });
    let strip = m.current_rect();
    assert!(strip.width >= 1 && strip.height >= 1);

    m.handle(BallEvent::CursorEnter { now_ms: 2_000 });
    let ball = m.current_rect();
    assert!(ball.width >= 1 && ball.height >= 1);
}
