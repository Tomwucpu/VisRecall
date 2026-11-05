//! 托盘图标与菜单。
//!
//! 冷启动不创建主窗口（架构 §3.3），托盘是唯一入口：左键或菜单「打开主窗口」。
//! 图标用代码生成的 32×32 RGBA 占位，避免阶段 01 引入图片资源。

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

    let (icon, tooltip) = if library_ok {
        (placeholder_icon(), TOOLTIP_OK)
    } else {
        (library_unavailable_icon(), TOOLTIP_UNAVAILABLE)
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

/// 图标配色：正常态品牌蓝，异常态告警红。
#[derive(Clone, Copy)]
enum Palette {
    Normal,
    Unavailable,
}

impl Palette {
    /// (外圈边框色, 圆底色)；中间记录块始终为白色。
    fn colors(self) -> ([u8; 4], [u8; 4]) {
        match self {
            Palette::Normal => ([37, 99, 235, 255], [59, 130, 246, 255]),
            Palette::Unavailable => ([153, 27, 27, 255], [220, 38, 38, 255]),
        }
    }
}

/// 托盘正常态图标。
fn placeholder_icon() -> Image<'static> {
    tray_icon(Palette::Normal)
}

/// 托盘异常态图标：库打不开时使用，形状与正常态一致，只是换成告警红。
fn library_unavailable_icon() -> Image<'static> {
    tray_icon(Palette::Unavailable)
}

/// 32×32 RGBA 占位图标：圆底 + 白色记录块。正式图标在阶段 04 替换。
fn tray_icon(palette: Palette) -> Image<'static> {
    const SIZE: u32 = 32;
    const CENTER: f32 = 16.0;
    const RADIUS: f32 = 15.0;
    const BORDER: f32 = 13.5;

    let (border, fill) = palette.colors();

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
                border
            } else {
                fill
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

    /// 圆底内侧、白色记录块之外的探针像素。
    const PROBE: usize = ((16 * 32) + 5) * 4;

    fn assert_shape(icon: &Image<'_>) {
        assert_eq!(icon.width(), 32);
        assert_eq!(icon.height(), 32);
        assert_eq!(icon.rgba().len(), 32 * 32 * 4);
        // 中心像素是白色记录块，角落保持透明。
        let center = ((16 * 32) + 16) as usize * 4;
        assert_eq!(&icon.rgba()[center..center + 4], &[255, 255, 255, 255]);
        assert_eq!(&icon.rgba()[0..4], &[0, 0, 0, 0]);
    }

    #[test]
    fn placeholder_icon_is_opaque_32_square() {
        let icon = placeholder_icon();
        assert_shape(&icon);
        assert_eq!(&icon.rgba()[PROBE..PROBE + 4], &[59, 130, 246, 255]);
    }

    /// 异常态与正常态形状一致，但圆底换成告警红（红 > 蓝）。
    #[test]
    fn unavailable_icon_uses_alert_red() {
        let icon = library_unavailable_icon();
        assert_shape(&icon);

        let pixel = &icon.rgba()[PROBE..PROBE + 4];
        assert_eq!(pixel, &[220, 38, 38, 255]);
        assert_ne!(pixel, &placeholder_icon().rgba()[PROBE..PROBE + 4]);
    }
}
