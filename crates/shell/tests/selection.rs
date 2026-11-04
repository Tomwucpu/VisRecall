//! 选区状态机与纯几何函数的集成测试（design §8、§6）。

use proptest::prelude::*;
use visrecall_domain::capture::DisplayInfo;
use visrecall_domain::geometry::{PhysicalPoint, PhysicalRect};
use visrecall_shell::selection::{
    clamp_to_virtual_screen, drag_rect, hit_test_handle, snap_rect, Handle, Key, Modifiers,
    SelectionPhase, SelectionState,
};

fn p(x: i32, y: i32) -> PhysicalPoint {
    PhysicalPoint { x, y }
}

fn r(x: i32, y: i32, width: u32, height: u32) -> PhysicalRect {
    PhysicalRect::new(x, y, width, height)
}

fn monitor(bounds: PhysicalRect) -> DisplayInfo {
    DisplayInfo {
        bounds,
        work_area: bounds,
        dpi: 96,
        ..DisplayInfo::default()
    }
}

// ---- drag_rect ----

#[test]
fn move_keeps_size() {
    let start = r(10, 10, 100, 100);
    let out = drag_rect(Handle::Move, start, p(20, -5), 10);
    assert_eq!(out, r(30, 5, 100, 100));
}

#[test]
fn resize_never_goes_below_min() {
    let start = r(10, 10, 100, 100);
    let out = drag_rect(Handle::East, start, p(-500, 0), 10);
    assert!(out.width >= 10);
    assert_eq!(out, r(10, 10, 10, 100));
}

#[test]
fn northwest_drag_moves_origin_and_shrinks() {
    let start = r(10, 10, 100, 100);
    assert_eq!(
        drag_rect(Handle::NorthWest, start, p(20, 30), 10),
        r(30, 40, 80, 70)
    );
}

#[test]
fn resize_cannot_cross_opposite_edge() {
    let start = r(10, 10, 100, 100);
    // 左/上边越过对边时被钳在 `对边 - min` 处，而不是交换宽高。
    assert_eq!(
        drag_rect(Handle::West, start, p(500, 0), 10),
        r(100, 10, 10, 100)
    );
    assert_eq!(
        drag_rect(Handle::North, start, p(0, 500), 10),
        r(10, 100, 100, 10)
    );
}

proptest! {
    #[test]
    fn resized_rect_never_negative(w in 0u32..2000, h in 0u32..2000, dx in -2000i32..2000, dy in -2000i32..2000) {
        let start = PhysicalRect::new(100, 100, w.max(10), h.max(10));
        let out = drag_rect(Handle::SouthEast, start, PhysicalPoint { x: dx, y: dy }, 10);
        prop_assert!(out.width >= 10 && out.height >= 10);
    }
}

// ---- hit_test_handle ----

#[test]
fn hit_test_prefers_handles_then_move() {
    let rect = r(10, 10, 100, 100);
    assert_eq!(hit_test_handle(rect, p(60, 60), 96), Some(Handle::Move));
    assert_eq!(
        hit_test_handle(rect, p(10, 10), 96),
        Some(Handle::NorthWest)
    );
    assert_eq!(hit_test_handle(rect, p(110, 60), 96), Some(Handle::East));
    assert_eq!(hit_test_handle(rect, p(60, 110), 96), Some(Handle::South));
    assert_eq!(hit_test_handle(rect, p(300, 300), 96), None);
}

#[test]
fn hit_test_of_empty_rect_is_none() {
    assert_eq!(hit_test_handle(PhysicalRect::default(), p(0, 0), 96), None);
    assert_eq!(hit_test_handle(PhysicalRect::default(), p(0, 0), 192), None);
}

#[test]
fn hit_test_radius_scales_with_dpi() {
    let rect = r(0, 0, 100, 100);
    // West 手柄中心在 (0, 50)，测试点在它左侧 8px。
    let just_outside = p(-8, 50);
    assert_eq!(hit_test_handle(rect, just_outside, 96), None);
    assert_eq!(hit_test_handle(rect, just_outside, 192), Some(Handle::West));
}

// ---- snap_rect ----

#[test]
fn snap_rect_aligns_edges_within_threshold() {
    let rect = r(12, 12, 100, 100);
    let candidates = [r(10, 10, 200, 200)];
    // 左/上边吸附到 10；右/下边离候选边太远，保持 112（宽高随之变成 102）。
    assert_eq!(snap_rect(rect, &candidates, 8), r(10, 10, 102, 102));
}

#[test]
fn snap_rect_ignores_far_edges() {
    let rect = r(100, 100, 50, 50);
    let candidates = [r(0, 0, 10, 10)];
    assert_eq!(snap_rect(rect, &candidates, 8), rect);
}

#[test]
fn snap_rect_drops_direction_that_would_shrink_below_min() {
    // 左右两边都吸到同一条候选边（x=8）：宽会变成 0，因此整个横向吸附被放弃。
    let rect = r(0, 0, 12, 40);
    let candidates = [r(8, 200, 0, 0)];
    assert_eq!(snap_rect(rect, &candidates, 8), rect);
}

// ---- clamp_to_virtual_screen ----

#[test]
fn clamp_intersects_virtual_screen() {
    let displays = [
        monitor(r(0, 0, 1920, 1080)),
        monitor(r(1920, 0, 1920, 1080)),
    ];
    assert_eq!(
        clamp_to_virtual_screen(r(-100, -100, 500, 500), &displays),
        r(0, 0, 400, 400)
    );
    // 跨屏选区不被单屏裁剪。
    let spanning = r(1800, 100, 400, 300);
    assert_eq!(clamp_to_virtual_screen(spanning, &displays), spanning);
    // 完全在屏幕外：位置保留、尺寸归零。
    assert_eq!(
        clamp_to_virtual_screen(r(5000, 5000, 100, 100), &displays),
        r(5000, 5000, 0, 0)
    );
}

#[test]
fn clamp_returns_rect_when_no_displays() {
    let rect = r(-50, -50, 100, 100);
    assert_eq!(clamp_to_virtual_screen(rect, &[]), rect);
}

// ---- SelectionState：拖拽与生命周期 ----

#[test]
fn drag_creates_rect_from_anchor() {
    let mut s = SelectionState::new();
    s.on_mouse_down(p(10, 10), None);
    assert_eq!(s.phase, SelectionPhase::Dragging);
    assert_eq!(s.rect, r(10, 10, 0, 0));
    s.on_mouse_move(p(110, 60), Modifiers::default(), &[]);
    assert_eq!(s.rect, r(10, 10, 100, 50));
}

#[test]
fn mouse_up_keeps_selection_until_enter() {
    let mut s = SelectionState::new();
    s.on_mouse_down(p(10, 10), None);
    s.on_mouse_move(p(110, 60), Modifiers::default(), &[]);
    s.on_mouse_up();
    assert_eq!(s.phase, SelectionPhase::HoverWindow);
    assert_eq!(s.active, None);
    assert_eq!(s.rect, r(10, 10, 100, 50));

    s.on_key(Key::Enter, false, &[]);
    assert_eq!(s.phase, SelectionPhase::Committed);
    assert_eq!(s.rect, r(10, 10, 100, 50));
}

#[test]
fn escape_cancels_selection() {
    let mut s = SelectionState::new();
    s.on_mouse_down(p(10, 10), None);
    s.on_mouse_move(p(110, 60), Default::default(), &[]);
    s.cancel();
    assert_eq!(s.commit(), None);
}

#[test]
fn escape_key_cancels_and_blocks_commit() {
    let mut s = SelectionState::new();
    s.on_mouse_down(p(10, 10), None);
    s.on_mouse_move(p(110, 60), Modifiers::default(), &[]);
    s.on_mouse_up();
    s.on_key(Key::Escape, false, &[]);
    assert_eq!(s.phase, SelectionPhase::Cancelled);
    assert_eq!(s.active, None);
    assert_eq!(s.commit(), None);
}

#[test]
fn cancel_clears_selection_and_hover() {
    let mut s = SelectionState::new();
    s.on_mouse_move(p(50, 50), Modifiers::default(), &[r(0, 0, 100, 100)]);
    assert_eq!(s.hovered_rect(), Some(r(0, 0, 100, 100)));

    s.on_mouse_down(p(60, 60), None);
    s.on_mouse_move(p(160, 120), Modifiers::default(), &[r(0, 0, 100, 100)]);
    s.on_mouse_up();
    assert_eq!(s.rect, r(60, 60, 100, 60));

    s.cancel();
    assert_eq!(s.phase, SelectionPhase::Cancelled);
    assert_eq!(s.rect, PhysicalRect::default());
    assert_eq!(s.hover_rect, PhysicalRect::default());
    assert_eq!(s.hovered_rect(), None);
    assert_eq!(s.active, None);
    assert_eq!(s.commit(), None);
}

#[test]
fn commit_rejects_selection_below_min_size() {
    let mut s = SelectionState::new();
    s.on_mouse_down(p(10, 10), None);
    s.on_mouse_move(p(15, 15), Modifiers::default(), &[]);
    s.on_mouse_up();
    assert_eq!(s.commit(), None);
    assert_eq!(s.phase, SelectionPhase::Cancelled);
}

// ---- SelectionState：调整已有选区 ----

#[test]
fn move_handle_translates_selection() {
    let mut s = SelectionState::new();
    s.rect = r(10, 10, 100, 50);
    s.on_mouse_down(p(60, 30), Some(Handle::Move));
    assert_eq!(s.phase, SelectionPhase::Adjusting);
    assert_eq!(s.active, Some(Handle::Move));
    assert_eq!(s.start_rect, r(10, 10, 100, 50));
    s.on_mouse_move(p(70, 25), Modifiers::default(), &[]);
    assert_eq!(s.rect, r(20, 5, 100, 50));
    s.on_mouse_up();
    assert_eq!(s.phase, SelectionPhase::HoverWindow);
    assert_eq!(s.active, None);
}

#[test]
fn east_handle_resizes_from_start_rect() {
    let mut s = SelectionState::new();
    s.rect = r(10, 10, 100, 100);
    s.on_mouse_down(p(110, 60), Some(Handle::East));
    s.on_mouse_move(p(150, 60), Modifiers::default(), &[]);
    assert_eq!(s.rect, r(10, 10, 140, 100));
}

#[test]
fn ctrl_disables_snapping() {
    // 候选窗口边在 x/y = 0 与 112；拖到 (110, 108) 时右/下边分别离 112 只有 2/4 px。
    let candidates = [r(0, 0, 112, 112)];

    let mut snapped = SelectionState::new();
    snapped.on_mouse_down(p(10, 10), None);
    snapped.on_mouse_move(p(110, 108), Modifiers::default(), &candidates);
    assert_eq!(snapped.rect, r(10, 10, 102, 102));

    let mut free = SelectionState::new();
    free.on_mouse_down(p(10, 10), None);
    free.on_mouse_move(
        p(110, 108),
        Modifiers {
            shift: false,
            ctrl: true,
        },
        &candidates,
    );
    assert_eq!(free.rect, r(10, 10, 100, 98));
}

// ---- SelectionState：吸附变体 ----

#[test]
fn drag_snap_keeps_anchor_fixed() {
    let mut s = SelectionState::new();
    s.on_mouse_down(p(10, 10), None);
    s.on_mouse_move(p(110, 108), Modifiers::default(), &[r(0, 0, 112, 112)]);
    // 只有随指针移动的右/下边吸附，起点 (10, 10) 不动。
    assert_eq!(s.rect, r(10, 10, 102, 102));
    assert_eq!(s.anchor, p(10, 10));
}

#[test]
fn move_snap_keeps_size() {
    let mut s = SelectionState::new();
    s.rect = r(14, 14, 100, 50);
    s.on_mouse_down(p(60, 40), Some(Handle::Move));
    s.on_mouse_move(p(60, 40), Modifiers::default(), &[r(10, 10, 300, 300)]);
    // 整框平移到候选边，宽高保持 100×50。
    assert_eq!(s.rect, r(10, 10, 100, 50));
    assert_eq!(s.rect.width, 100);
    assert_eq!(s.rect.height, 50);
}

#[test]
fn east_handle_snap_keeps_opposite_edge() {
    let mut s = SelectionState::new();
    s.rect = r(10, 10, 100, 100);
    s.on_mouse_down(p(110, 60), Some(Handle::East));
    // 拖到 117；候选右边在 120（距离 3），只需吸被拖动的右边。
    s.on_mouse_move(p(117, 60), Modifiers::default(), &[r(0, 0, 120, 400)]);
    assert_eq!(s.rect, r(10, 10, 110, 100));
    assert_eq!(s.rect.x, 10);
    assert_eq!(s.rect.y, 10);
    assert_eq!(s.rect.height, 100);
}

#[test]
fn west_handle_snap_keeps_opposite_edge() {
    let mut s = SelectionState::new();
    s.rect = r(12, 10, 100, 100);
    s.on_mouse_down(p(12, 60), Some(Handle::West));
    // 拖到 18；候选左边在 20（距离 2），右边 112 不得移动。
    s.on_mouse_move(p(18, 60), Modifiers::default(), &[r(20, 0, 100, 400)]);
    assert_eq!(s.rect, r(20, 10, 92, 100));
    assert_eq!(s.rect.right(), 112);
    assert_eq!(s.rect.y, 10);
    assert_eq!(s.rect.height, 100);
}

#[test]
fn snap_abandons_edge_that_breaks_min_size() {
    let mut s = SelectionState::new();
    s.rect = r(10, 10, 100, 100);
    s.on_mouse_down(p(110, 60), Some(Handle::East));
    // 左拖到最小宽度 10；候选边 15 会把宽度吸成 5，因此放弃这条边的吸附。
    s.on_mouse_move(p(-390, 60), Modifiers::default(), &[r(0, 0, 15, 400)]);
    assert_eq!(s.rect, r(10, 10, 10, 100));
}

// ---- SelectionState：悬停、滚轮与微调 ----

#[test]
fn hover_highlights_candidate_window() {
    let mut s = SelectionState::new();
    s.on_mouse_move(
        p(50, 50),
        Modifiers::default(),
        &[r(0, 0, 100, 100), r(200, 0, 100, 100)],
    );
    assert_eq!(s.phase, SelectionPhase::HoverWindow);
    assert_eq!(s.hover_rect, r(0, 0, 100, 100));
    assert_eq!(s.hovered_rect(), Some(r(0, 0, 100, 100)));
    // 悬停只写 hover_rect，用户选区仍为空。
    assert!(s.rect.is_empty());
}

#[test]
fn hover_follows_pointer_across_windows() {
    let candidates = [r(0, 0, 100, 100), r(200, 0, 100, 100)];
    let mut s = SelectionState::new();

    s.on_mouse_move(p(50, 50), Modifiers::default(), &candidates);
    assert_eq!(s.hovered_rect(), Some(candidates[0]));
    assert!(s.rect.is_empty());

    // 移到第二个窗口：高亮必须跟着更新，而用户选区仍为空。
    s.on_mouse_move(p(250, 50), Modifiers::default(), &candidates);
    assert_eq!(s.hovered_rect(), Some(candidates[1]));
    assert_eq!(s.hover_rect, candidates[1]);
    assert_eq!(s.rect, PhysicalRect::default());
    assert_eq!(s.phase, SelectionPhase::HoverWindow);

    // 移到候选之外的空白：悬停高亮清空。
    s.on_mouse_move(p(900, 900), Modifiers::default(), &candidates);
    assert_eq!(s.hovered_rect(), None);
    assert_eq!(s.hover_rect, PhysicalRect::default());
}

#[test]
fn hover_never_overwrites_existing_selection() {
    let mut s = SelectionState::new();
    s.on_mouse_down(p(10, 10), None);
    s.on_mouse_move(p(110, 60), Modifiers::default(), &[]);
    s.on_mouse_up();

    s.on_mouse_move(p(50, 50), Modifiers::default(), &[r(0, 0, 400, 400)]);
    assert_eq!(s.rect, r(10, 10, 100, 50));
    // 悬停高亮照样跟随指针，只是不碰 `rect`。
    assert_eq!(s.hovered_rect(), Some(r(0, 0, 400, 400)));
}

#[test]
fn click_adopts_hovered_window() {
    let candidates = [r(0, 0, 100, 100), r(200, 0, 100, 100)];
    let mut s = SelectionState::new();
    s.on_mouse_move(p(250, 50), Modifiers::default(), &candidates);
    assert!(s.rect.is_empty());

    // 按下抬起之间没有位移：采用悬停窗口边界。
    s.on_mouse_down(p(250, 50), None);
    s.on_mouse_up();
    assert_eq!(s.rect, candidates[1]);
    assert_eq!(s.phase, SelectionPhase::HoverWindow);
    assert_eq!(s.commit(), Some(candidates[1]));
}

#[test]
fn click_without_hover_leaves_selection_empty() {
    let mut s = SelectionState::new();
    s.on_mouse_down(p(50, 50), None);
    s.on_mouse_up();
    assert!(s.rect.is_empty());
    assert_eq!(s.commit(), None);
}

#[test]
fn wheel_cycles_hover_rect() {
    let candidates = [r(0, 0, 100, 100), r(200, 0, 100, 100), r(400, 0, 100, 100)];
    let mut s = SelectionState::new();
    // 指针在候选之外的空白处：进入 HoverWindow 但不产生悬停高亮。
    s.on_mouse_move(p(50, 400), Modifiers::default(), &candidates);
    assert_eq!(s.phase, SelectionPhase::HoverWindow);
    assert!(s.rect.is_empty());
    assert_eq!(s.hovered_rect(), None);

    // 索引从 0 向前一位，回绕到最后一个候选。
    s.on_wheel(-1);
    assert_eq!(s.hovered_rect(), Some(candidates[2]));

    // 继续向前：回到第一个候选。
    s.on_wheel(1);
    assert_eq!(s.hovered_rect(), Some(candidates[0]));

    // 滚轮不改变用户选区。
    assert!(s.rect.is_empty());
}

#[test]
fn wheel_keeps_user_selection() {
    let mut s = SelectionState::new();
    s.on_mouse_down(p(10, 10), None);
    s.on_mouse_move(p(110, 60), Modifiers::default(), &[]);
    s.on_mouse_up();

    let candidates = [r(0, 0, 100, 100), r(200, 0, 100, 100)];
    // 记录候选链但不产生悬停高亮，且不影响已有选区。
    s.on_mouse_move(p(50, 400), Modifiers::default(), &candidates);
    assert_eq!(s.rect, r(10, 10, 100, 50));

    s.on_wheel(1);
    assert_eq!(s.hovered_rect(), Some(candidates[1]));
    assert_eq!(s.rect, r(10, 10, 100, 50));
}

#[test]
fn wheel_without_candidates_is_noop() {
    let mut s = SelectionState::new();
    s.on_mouse_move(p(10, 10), Modifiers::default(), &[]);
    s.on_wheel(3);
    assert_eq!(s.rect, PhysicalRect::default());
    assert_eq!(s.phase, SelectionPhase::Idle);
}

#[test]
fn arrow_keys_nudge_without_snapping() {
    let mut s = SelectionState::new();
    s.on_mouse_down(p(10, 10), None);
    s.on_mouse_move(p(110, 60), Modifiers::default(), &[]);
    s.on_mouse_up();

    let candidates = [r(0, 0, 400, 400)];
    s.on_key(Key::Left, false, &candidates);
    assert_eq!(s.rect, r(9, 10, 100, 50));
    // 微调后基准同步到新位置，且不再有活动手柄。
    assert_eq!(s.start_rect, r(9, 10, 100, 50));
    assert_eq!(s.active, None);
    s.on_key(Key::Down, true, &candidates);
    assert_eq!(s.rect, r(9, 20, 100, 50));
    assert_eq!(s.start_rect, r(9, 20, 100, 50));
    // 微调后不回吸到候选边（x 仍是 9 而不是 0），后续鼠标移动也不会按旧基准覆盖。
    assert_ne!(s.rect, r(0, 20, 100, 50));
    s.on_mouse_move(p(50, 400), Modifiers::default(), &candidates);
    assert_eq!(s.rect, r(9, 20, 100, 50));

    s.on_key(Key::Escape, false, &[]);
    assert_eq!(s.phase, SelectionPhase::Cancelled);
}

// ---- SelectionState：复审回归（状态泄漏边界） ----

#[test]
fn wheel_is_ignored_while_dragging() {
    // 候选窗口远离拖拽区域，避免吸附干扰断言。
    let candidates = [r(900, 900, 10, 10), r(950, 950, 10, 10)];
    let mut s = SelectionState::new();
    s.on_mouse_move(p(700, 700), Modifiers::default(), &candidates);
    assert_eq!(s.phase, SelectionPhase::HoverWindow);
    assert_eq!(s.hovered_rect(), None);

    s.on_mouse_down(p(10, 10), None);
    s.on_mouse_move(p(60, 40), Modifiers::default(), &candidates);
    assert_eq!(s.phase, SelectionPhase::Dragging);
    assert_eq!(s.rect, r(10, 10, 50, 30));

    // Dragging 期间的滚轮必须完全忽略：不换 hover、不改 phase/active/rect。
    s.on_wheel(1);
    assert_eq!(s.phase, SelectionPhase::Dragging);
    assert_eq!(s.active, None);
    assert_eq!(s.hovered_rect(), None);
    assert_eq!(s.rect, r(10, 10, 50, 30));

    // 后续鼠标移动仍按拖拽更新选区。
    s.on_mouse_move(p(110, 60), Modifiers::default(), &candidates);
    assert_eq!(s.phase, SelectionPhase::Dragging);
    assert_eq!(s.rect, r(10, 10, 100, 50));

    s.on_mouse_up();
    assert_eq!(s.phase, SelectionPhase::HoverWindow);
    assert_eq!(s.rect, r(10, 10, 100, 50));
}

#[test]
fn failed_commit_does_not_adopt_stale_hover() {
    let candidates = [r(0, 0, 100, 100)];
    let mut s = SelectionState::new();
    s.on_mouse_move(p(50, 50), Modifiers::default(), &candidates);
    assert_eq!(s.hovered_rect(), Some(candidates[0]));

    // 空选区按 Enter：提交失败并清空悬停与选区。
    s.on_key(Key::Enter, false, &candidates);
    assert_eq!(s.phase, SelectionPhase::Cancelled);
    assert_eq!(s.hovered_rect(), None);
    assert_eq!(s.rect, PhysicalRect::default());

    // 随后“空白点击 + 抬起”不得把陈旧候选采用为选区（仍是空矩形）。
    s.on_mouse_down(p(500, 500), None);
    s.on_mouse_up();
    assert!(s.rect.is_empty());
    assert_eq!(s.hovered_rect(), None);
    assert_eq!(s.commit(), None);
}

#[test]
fn horizontal_drag_still_snaps_x() {
    let candidates = [r(0, 0, 112, 400)];
    let mut s = SelectionState::new();
    s.on_mouse_down(p(10, 10), None);
    // 纯水平拖动（height == 0）：x 轴仍必须吸附，anchor 不动。
    s.on_mouse_move(p(110, 10), Modifiers::default(), &candidates);
    assert_eq!(s.rect, r(10, 10, 102, 0));
    assert_eq!(s.anchor, p(10, 10));
}

#[test]
fn arrow_key_during_drag_ends_drag() {
    let mut s = SelectionState::new();
    s.on_mouse_down(p(10, 10), None);
    s.on_mouse_move(p(110, 60), Modifiers::default(), &[]);
    assert_eq!(s.phase, SelectionPhase::Dragging);

    // 方向键微调即使在 Dragging 中也结束拖拽，基准同步到新位置。
    s.on_key(Key::Right, false, &[]);
    assert_eq!(s.phase, SelectionPhase::HoverWindow);
    assert_eq!(s.active, None);
    assert_eq!(s.rect, r(11, 10, 100, 50));
    assert_eq!(s.start_rect, r(11, 10, 100, 50));

    // 随后的鼠标移动不得按旧 anchor 覆盖微调结果。
    s.on_mouse_move(p(300, 300), Modifiers::default(), &[]);
    assert_eq!(s.rect, r(11, 10, 100, 50));
    assert_eq!(s.phase, SelectionPhase::HoverWindow);
}
