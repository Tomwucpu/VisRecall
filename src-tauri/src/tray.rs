//! 托盘图标与菜单。
//!
//! 冷启动不创建主窗口（架构 §3.3），托盘是唯一入口：左键或菜单「打开主窗口」。
//! 图标用代码生成的 32×32 RGBA 占位，避免阶段 01 引入图片资源。

use tauri::image::Image;
use tauri::menu::MenuBuilder;
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::App;

use crate::window_lifecycle;

/// 托盘 id，便于后续任务按 id 操作图标（例如进入异常态）。
pub const TRAY_ID: &str = "visrecall-tray";

const MENU_OPEN_MAIN: &str = "tray.open-main";
const MENU_TOGGLE_BALL: &str = "tray.toggle-ball";
const MENU_SETTINGS: &str = "tray.settings";
const MENU_QUIT: &str = "tray.quit";

/// 创建托盘；图标或菜单构建失败返回错误（由 `setup` 决定是否致命）。
pub fn create(app: &App) -> tauri::Result<()> {
    let menu = MenuBuilder::new(app)
        .text(MENU_OPEN_MAIN, "打开主窗口")
        .text(MENU_TOGGLE_BALL, "显示或隐藏悬浮球")
        .text(MENU_SETTINGS, "设置")
        .separator()
        .text(MENU_QUIT, "退出")
        .build()?;

    TrayIconBuilder::with_id(TRAY_ID)
        .icon(placeholder_icon())
        .tooltip("目记 VisRecall")
        .menu(&menu)
        // 左键交给点击事件（唤醒主窗口），右键弹菜单。
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            MENU_OPEN_MAIN => window_lifecycle::show_main_window(app),
            MENU_TOGGLE_BALL => {
                // 悬浮球在 Task 15 接入；阶段 01 先记录占位日志。
                tracing::info!("tray: toggle ball is not implemented until phase 01 task 15");
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

    tracing::info!("tray icon created");
    Ok(())
}

/// 32×32 RGBA 占位图标：品牌蓝圆底 + 白色记录块。正式图标在阶段 04 替换。
fn placeholder_icon() -> Image<'static> {
    const SIZE: u32 = 32;
    const CENTER: f32 = 16.0;
    const RADIUS: f32 = 15.0;
    const BORDER: f32 = 13.5;

    let mut rgba = vec![0u8; (SIZE * SIZE * 4) as usize];
    for y in 0..SIZE {
        for x in 0..SIZE {
            let dx = x as f32 + 0.5 - CENTER;
            let dy = y as f32 + 0.5 - CENTER;
            if dx * dx + dy * dy > RADIUS * RADIUS {
                continue;
            }
            let index = ((y * SIZE + x) * 4) as usize;
            let color = if dx * dx + dy * dy > BORDER * BORDER {
                [37, 99, 235, 255]
            } else {
                [59, 130, 246, 255]
            };
            rgba[index..index + 4].copy_from_slice(&color);

            if (12..20).contains(&x) && (12..20).contains(&y) {
                rgba[index..index + 4].copy_from_slice(&[255, 255, 255, 255]);
            }
        }
    }
    Image::new_owned(rgba, SIZE, SIZE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholder_icon_is_opaque_32_square() {
        let icon = placeholder_icon();
        assert_eq!(icon.width(), 32);
        assert_eq!(icon.height(), 32);
        assert_eq!(icon.rgba().len(), 32 * 32 * 4);
        // 中心像素是白色记录块，角落保持透明。
        let center = ((16 * 32) + 16) as usize * 4;
        assert_eq!(&icon.rgba()[center..center + 4], &[255, 255, 255, 255]);
        assert_eq!(&icon.rgba()[0..4], &[0, 0, 0, 0]);
    }
}
