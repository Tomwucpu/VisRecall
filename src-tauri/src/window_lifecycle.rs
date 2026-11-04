//! 主窗口生命周期：冷启动不建窗口，按需创建；关闭后隐藏并在 60s 后销毁
//! WebView2（需求 P1-R01，架构 §3.3）。
//!
//! 「隐藏 + 安排销毁」与「显示 + 按需重建」必须整体串行，否则旧计时器可能在
//! 用户刚唤醒窗口之后才读到状态并把它销毁。这里用进程级互斥锁把两段逻辑放进
//! 同一临界区，并用 `GENERATION` 代号让过期计时器失效。
//!
//! 所有临界区都只在主线程执行：Tauri 的窗口 getter/dispatcher 在非主线程调用会
//! 阻塞等待主线程应答，持锁等待主线程就会互锁。因此计时线程只把「检查 + 销毁」
//! 投递到主线程，临界区内不做 sleep、也不做跨线程等待。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
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

/// 生命周期临界区：隐藏排程与显示/重建互斥。
static LIFECYCLE: Mutex<()> = Mutex::new(());

/// 每次隐藏与显示都递增；计时器只在代号未变时销毁窗口。
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// 启动时若已有主窗口（当前配置下不会有）则挂上关闭拦截。
pub fn install(app: &App) {
    if let Some(window) = app.get_webview_window(MAIN_LABEL) {
        attach_close_handler(&window);
    }
}

/// 显示主窗口；窗口已销毁则按需重建。单实例回调与托盘共用此入口。
///
/// 统一切到主线程执行，使窗口操作与代号递增落在同一个线程，不与其他线程
/// 的窗口调用互相等待（可从任意线程安全调用）。
pub fn show_main_window(app: &AppHandle) {
    let handle = app.clone();
    if let Err(err) = app.run_on_main_thread(move || show_main_window_on_main(&handle)) {
        tracing::error!(error = %err, "failed to dispatch show_main_window to main thread");
    }
}

fn show_main_window_on_main(app: &AppHandle) {
    // 递增代号即作废所有在睡眠的销毁计时器；显示与动作在同一临界区里完成。
    let _guard = lock_lifecycle();
    next_generation();

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

            // 事件回调在主线程：临界区内只做「递增代号 + 隐藏」，随后交给计时线程。
            let scheduled = {
                let _guard = lock_lifecycle();
                let generation = next_generation();
                if let Some(target) = app.get_webview_window(MAIN_LABEL) {
                    if let Err(err) = target.hide() {
                        tracing::warn!(error = %err, "failed to hide main window");
                    }
                }
                generation
            };

            schedule_destroy(&app, scheduled);
        }
    });
}

fn schedule_destroy(app: &AppHandle, scheduled: u64) {
    let handle = app.clone();

    std::thread::spawn(move || {
        // 睡眠在锁外：临界区不长时间持锁。
        std::thread::sleep(DESTROY_DELAY);

        let inner = handle.clone();
        if let Err(err) =
            handle.run_on_main_thread(move || destroy_hidden_window(&inner, scheduled))
        {
            tracing::warn!(error = %err, "failed to dispatch window destroy to main thread");
        }
    });
}

/// 计时器到点后的销毁决策；只在主线程执行。
fn destroy_hidden_window(app: &AppHandle, scheduled: u64) {
    let _guard = lock_lifecycle();

    let Some(window) = app.get_webview_window(MAIN_LABEL) else {
        return;
    };
    // 主线程上读取可见性：内联完成，不会跨线程等待，因此不会与临界区互锁。
    let visibility = match window.is_visible() {
        Ok(visible) => Some(visible),
        Err(err) => {
            tracing::warn!(error = %err, "cannot read main window visibility; keeping window");
            None
        }
    };

    if !should_destroy(GENERATION.load(Ordering::SeqCst), scheduled, visibility) {
        tracing::debug!("destroy timer superseded; main window kept");
        return;
    }

    if let Err(err) = window.destroy() {
        tracing::warn!(error = %err, "failed to destroy hidden main window");
    } else {
        tracing::info!("hidden main window destroyed after 60s");
    }
}

/// 销毁判定：代号仍是排程时的那一代，且窗口确实不可见。
///
/// `visibility` 为 `None`（读不到可见性）时保守返回 `false`，避免误杀正在显示的窗口。
fn should_destroy(
    current_generation: u64,
    scheduled_generation: u64,
    visibility: Option<bool>,
) -> bool {
    current_generation == scheduled_generation && visibility == Some(false)
}

fn lock_lifecycle() -> MutexGuard<'static, ()> {
    // 持锁期间不会 panic；被毒化时继续使用，避免窗口再也无法显示或销毁。
    LIFECYCLE
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
}

fn next_generation() -> u64 {
    GENERATION.fetch_add(1, Ordering::SeqCst) + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destroys_only_current_generation_while_hidden() {
        assert!(should_destroy(7, 7, Some(false)));
    }

    #[test]
    fn keeps_window_when_it_was_shown_again() {
        // 关闭后 24s 被唤醒：show_main_window 递增了代号，旧计时器必须作废。
        assert!(!should_destroy(8, 7, Some(false)));
        // 即使可见性读取滞后也不销毁。
        assert!(!should_destroy(8, 7, Some(true)));
    }

    #[test]
    fn keeps_visible_window_of_same_generation() {
        assert!(!should_destroy(7, 7, Some(true)));
    }

    #[test]
    fn keeps_window_when_visibility_cannot_be_read() {
        assert!(!should_destroy(7, 7, None));
    }

    #[test]
    fn generation_increments_on_every_transition() {
        let before = GENERATION.load(Ordering::SeqCst);
        assert_eq!(next_generation(), before + 1);
        assert_eq!(next_generation(), before + 2);
    }
}
