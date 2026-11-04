//! 窗口候选过滤的纯逻辑测试：不触碰 Win32，可在任意平台跑。

use visrecall_capture::window::{filter_candidates, is_desktop_class, WindowInfo};
use visrecall_domain::geometry::PhysicalRect;

fn w(title: &str, tool: bool, cloaked: bool) -> WindowInfo {
    WindowInfo {
        hwnd: 1,
        title: title.into(),
        class_name: "Chrome_WidgetWin_1".into(),
        process_name: "chrome.exe".into(),
        bounds: PhysicalRect::new(0, 0, 100, 100),
        tool_window: tool,
        cloaked,
    }
}

#[test]
fn filters_tool_and_cloaked_windows() {
    let ws = vec![
        w("visible", false, false),
        w("tool", true, false),
        w("cloaked", false, true),
    ];
    let out = filter_candidates(&ws);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].title, "visible");
}

#[test]
fn recognizes_desktop_classes() {
    assert!(is_desktop_class("Progman"));
    assert!(is_desktop_class("WorkerW"));
    assert!(is_desktop_class("Shell_TrayWnd"));
    assert!(!is_desktop_class("Chrome_WidgetWin_1"));
}

#[test]
fn filters_empty_and_blank_titles() {
    let ws = vec![
        w("", false, false),
        w("   ", false, false),
        w("记事本", false, false),
    ];
    let out = filter_candidates(&ws);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].title, "记事本");
}

#[test]
fn keeps_window_that_trips_no_rule() {
    // 多个条件同时成立时仍应整体排除：这里确认只有单个条件时不会误伤。
    let ws = vec![w("settings", false, false)];
    let out = filter_candidates(&ws);
    assert_eq!(out.len(), 1);
}

#[test]
fn filters_when_multiple_rules_apply() {
    let ws = vec![w("", true, true), w("normal", false, false)];
    let out = filter_candidates(&ws);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].title, "normal");
}

#[test]
fn desktop_class_match_is_exact() {
    assert!(!is_desktop_class("progman"));
    assert!(!is_desktop_class("WorkerWX"));
    assert!(!is_desktop_class(""));
}
