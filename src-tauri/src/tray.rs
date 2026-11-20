//! 托盘图标与菜单。
//!
//! 冷启动不创建主窗口（架构 §3.3），托盘是唯一入口：左键或菜单「打开主窗口」。
//! 图标内嵌 `icons/64x64.png`，异常态保留图标形状并整体映射为告警红。

use tauri::image::Image;
use tauri::menu::MenuBuilder;
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{App, Manager};

use crate::window_lifecycle;

/// 托盘 id，便于后续任务按 id 操作图标（例如退出异常态）。
pub const TRAY_ID: &str = "visrecall-tray";

const MENU_OPEN_MAIN: &str = "tray.open-main";
const MENU_TOGGLE_BALL: &str = "tray.toggle-ball";
const MENU_SETTINGS: &str = "tray.settings";
const MENU_QUIT: &str = "tray.quit";

const TOOLTIP_OK: &str = "目记 VisRecall";
const TOOLTIP_UNAVAILABLE: &str = "目记 VisRecall — 库不可用";

/// 创建托盘；`library_ok = false` 时进入异常态（红色图标 + 提示库不可用）。
///
/// 菜单四项、左键唤醒主窗口在两种状态下一致：库不可用时用户仍要能打开主窗口
/// 看到错误状态与库路径（阶段 02 再接「重试 / 重新定位 / 新建库」恢复页）。
pub fn create(app: &App, library_ok: bool) -> tauri::Result<()> {
    let menu = MenuBuilder::new(app)
        .text(MENU_OPEN_MAIN, "打开主窗口")
        .text(MENU_TOGGLE_BALL, "显示或隐藏悬浮球")
        .text(MENU_SETTINGS, "设置")
        .separator()
        .text(MENU_QUIT, "退出")
        .build()?;

    let normal = tray_icon();
    let icon = if library_ok {
        normal
    } else {
        library_unavailable_icon(&normal)
    };
    let tooltip = if library_ok {
        TOOLTIP_OK
    } else {
        TOOLTIP_UNAVAILABLE
    };

    TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon)
        .tooltip(tooltip)
        .menu(&menu)
        // 左键交给点击事件（唤醒主窗口），右键弹菜单。
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            MENU_OPEN_MAIN => window_lifecycle::show_main_window(app),
            MENU_TOGGLE_BALL => {
                // M-2：翻转要做 `save_atomic` 落盘与同步 `SendMessageW` 切换窗口，
                // 都不能占用 Tauri UI 线程（否则托盘/主窗口会卡顿）。投递到后台线程，
                // 菜单回调立即返回；失败原因照旧在 AppCore 里记日志。
                let app = app.clone();
                let spawned = std::thread::Builder::new()
                    .name("visrecall-tray-toggle-ball".into())
                    .spawn(move || {
                        // 悬浮球可见性由 AppCore 维护：写配置 → 同步线程，返回新状态。
                        let enabled = app.state::<visrecall_app_core::AppCore>().toggle_ball();
                        tracing::info!(enabled, "tray: floating ball visibility toggled");
                    });
                if let Err(err) = spawned {
                    tracing::error!(error = %err, "tray: failed to spawn ball toggle worker");
                }
            }
            MENU_SETTINGS => {
                // 设置页在后续任务实现；阶段 01 先打开主窗口承接。
                tracing::info!("tray: settings page is a placeholder; opening main window");
                window_lifecycle::show_main_window(app);
            }
            MENU_QUIT => {
                tracing::info!("tray: quit requested");
                app.exit(0);
            }
            other => tracing::warn!(menu = other, "unknown tray menu item"),
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                window_lifecycle::show_main_window(tray.app_handle());
            }
        })
        .build(app)?;

    tracing::info!(library_ok, "tray icon created");
    Ok(())
}

/// 内嵌托盘图标（64×64，紧凑裁切，与应用图标同源）。
const TRAY_ICON_PNG: &[u8] = include_bytes!("../icons/tray-64.png");

/// 正常态：直接使用应用图标。
fn tray_icon() -> Image<'static> {
    Image::from_bytes(TRAY_ICON_PNG).expect("bundled tray icon is a valid PNG")
}

/// 异常态：保留图标形状与明暗，整体映射到告警红。
fn library_unavailable_icon(normal: &Image<'_>) -> Image<'static> {
    let mut rgba = normal.rgba().to_vec();
    for pixel in rgba.chunks_exact_mut(4) {
        if pixel[3] == 0 {
            continue;
        }
        let luma = (pixel[0] as u32 * 30 + pixel[1] as u32 * 59 + pixel[2] as u32 * 11) / 100;
        pixel[0] = (40 + luma * 84 / 100) as u8;
        pixel[1] = (10 + luma * 25 / 100) as u8;
        pixel[2] = (12 + luma * 22 / 100) as u8;
    }
    Image::new_owned(rgba, normal.width(), normal.height())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_shape(icon: &Image<'_>) {
        assert_eq!(icon.width(), 64);
        assert_eq!(icon.height(), 64);
        assert_eq!(icon.rgba().len(), 64 * 64 * 4);
        // 圆角外保持透明。
        assert_eq!(&icon.rgba()[0..4], &[0, 0, 0, 0]);
    }

    /// 第一个完全不透明的像素下标；用实际图形做探针，避免依赖中心位置。
    fn first_opaque(icon: &Image<'_>) -> usize {
        icon.rgba()
            .chunks_exact(4)
            .position(|pixel| pixel[3] == 255)
            .expect("icon should contain opaque pixels")
            * 4
    }

    #[test]
    fn bundled_icon_decodes_to_64_square() {
        let icon = tray_icon();
        assert_shape(&icon);
        let visible = icon.rgba().chunks_exact(4).filter(|p| p[3] > 200).count();
        assert!(visible > 1000);
    }

    /// 异常态保持形状与透明度，但图形换成告警红（红 > 蓝）。
    #[test]
    fn unavailable_icon_maps_to_alert_red() {
        let normal = tray_icon();
        let alert = library_unavailable_icon(&normal);
        assert_shape(&alert);

        let probe = first_opaque(&normal);
        assert!(alert.rgba()[probe] > alert.rgba()[probe + 2]);
        assert_ne!(
            &alert.rgba()[probe..probe + 4],
            &normal.rgba()[probe..probe + 4]
        );
        assert_eq!(&alert.rgba()[0..4], &[0, 0, 0, 0]);
    }
}
