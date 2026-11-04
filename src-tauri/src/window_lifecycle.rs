//! 主窗口生命周期：冷启动不建窗口，按需创建；关闭后隐藏并在 60s 后销毁
//! WebView2（需求 P1-R01，架构 §3.3）。
//!
//! 关闭/重新显示用一个 `AtomicU64` 代号消除竞态：用户在 60s 内重新打开窗口时，
//! `show_main_window` 递增代号，旧计时器醒来发现代号过期就不再销毁。

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tauri::{App, AppHandle, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

/// 主窗口 label；与 `capabilities/default.json` 的 `windows` 对应。
pub const MAIN_LABEL: &str = "main";
const MAIN_TITLE: &str = "目记 VisRecall";
const MAIN_WIDTH: f64 = 1100.0;
const MAIN_HEIGHT: f64 = 760.0;
const MAIN_MIN_WIDTH: f64 = 860.0;
const MAIN_MIN_HEIGHT: f64 = 560.0;

/// 主窗口隐藏多久后销毁（让 WebView2 进程退出）。
const DESTROY_DELAY: Duration = Duration::from_secs(60);

/// 每次隐藏/显示都递增；计时器只在代号仍最新时销毁窗口。
static CLOSE_GENERATION: AtomicU64 = AtomicU64::new(0);

/// 启动时若已有主窗口（当前配置下不会有）则挂上关闭拦截。
pub fn install(app: &App) {
    if let Some(window) = app.get_webview_window(MAIN_LABEL) {
        attach_close_handler(&window);
    }
}

/// 显示主窗口；窗口已销毁则按需重建。单实例回调与托盘共用此入口。
pub fn show_main_window(app: &AppHandle) {
    // 取消可能还在睡眠的销毁计时器。
    bump_generation();

    if let Some(window) = app.get_webview_window(MAIN_LABEL) {
        if let Err(err) = window.unminimize() {
            tracing::warn!(error = %err, "failed to unminimize main window");
        }
        let _ = window.show();
        let _ = window.set_focus();
        return;
    }

    match create_main_window(app) {
        Ok(window) => {
            attach_close_handler(&window);
            let _ = window.show();
            let _ = window.set_focus();
            tracing::info!("main window created");
        }
        Err(err) => tracing::error!(error = %err, "failed to create main window"),
    }
}

fn create_main_window(app: &AppHandle) -> tauri::Result<WebviewWindow> {
    // 开发态由 Tauri 把 App("index.html") 解析到 devUrl，生产态解析到打包资源。
    WebviewWindowBuilder::new(app, MAIN_LABEL, WebviewUrl::App("index.html".into()))
        .title(MAIN_TITLE)
        .inner_size(MAIN_WIDTH, MAIN_HEIGHT)
        .min_inner_size(MAIN_MIN_WIDTH, MAIN_MIN_HEIGHT)
        .center()
        .build()
}

/// 拦截关闭：隐藏窗口并安排 60s 后销毁。
///
/// 闭包只捕获 `AppHandle`，不捕获窗口本身：事件监听器存放在窗口内部，
/// 捕获窗口克隆会形成引用环，销毁窗口后 WebView2 进程无法退出。
fn attach_close_handler(window: &WebviewWindow) {
    let app = window.app_handle().clone();

    window.on_window_event(move |event| {
        if let tauri::WindowEvent::CloseRequested { api, .. } = event {
            api.prevent_close();
            if let Some(target) = app.get_webview_window(MAIN_LABEL) {
                if let Err(err) = target.hide() {
                    tracing::warn!(error = %err, "failed to hide main window");
                }
            }
            schedule_destroy(&app);
        }
    });
}

fn bump_generation() -> u64 {
    CLOSE_GENERATION.fetch_add(1, Ordering::SeqCst) + 1
}

fn schedule_destroy(app: &AppHandle) {
    let generation = bump_generation();
    let app = app.clone();

    std::thread::spawn(move || {
        std::thread::sleep(DESTROY_DELAY);

        if CLOSE_GENERATION.load(Ordering::SeqCst) != generation {
            // 期间窗口被重新打开；计时器作废。
            return;
        }

        if let Some(window) = app.get_webview_window(MAIN_LABEL) {
            // 读取可见性失败时保守处理：不销毁。
            if window.is_visible().unwrap_or(true) {
                return;
            }
            if let Err(err) = window.destroy() {
                tracing::warn!(error = %err, "failed to destroy hidden main window");
            } else {
                tracing::info!("hidden main window destroyed after 60s");
            }
        }
    });
}
