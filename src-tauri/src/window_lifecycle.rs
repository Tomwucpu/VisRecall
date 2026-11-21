//! 主窗口生命周期：按启动偏好或用户唤醒创建；关闭后隐藏并在 60s 后销毁
//! WebView2（需求 P1-R01，架构 §3.3）。
//!
//! 「隐藏 + 安排销毁」与「显示 + 按需重建」必须整体串行，否则旧计时器可能在
//! 用户刚唤醒窗口之后才读到状态并把它销毁。这里用进程级互斥锁把两段逻辑放进
//! 同一临界区，并用 `GENERATION` 代号让过期计时器失效。
//!
//! 所有临界区都只在主线程执行：Tauri 的窗口 getter/dispatcher 在非主线程调用会
//! 阻塞等待主线程应答，持锁等待主线程就会互锁。因此计时线程只把「检查 + 销毁」
//! 投递到主线程，临界区内不做 sleep、也不做跨线程等待。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use tauri::{App, AppHandle, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use visrecall_app_core::WindowVisibility;
use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_HIDE, SW_SHOWNOACTIVATE};

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

/// 显示主窗口；窗口不存在则按需创建。冷启动、单实例回调与托盘共用此入口。
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
            if let Err(err) = window.show() {
                tracing::warn!(error = %err, "failed to show newly created main window");
            } else if let Some(start) = crate::PROCESS_START.get() {
                // 与托盘就绪分开打点；不要把 WebView2 初始化算进静默启动耗时。
                tracing::info!(
                    "process start to main window visible: {}ms",
                    start.elapsed().as_millis()
                );
            }
            let _ = window.set_focus();
        }
        Err(err) => tracing::error!(error = %err, "failed to create main window"),
    }
}

fn create_main_window(app: &AppHandle) -> tauri::Result<WebviewWindow> {
    // 开发态由 Tauri 把 App("index.html") 解析到 devUrl，生产态解析到打包资源。
    //
    // 窗口形态是 D-UI-1 的一部分：主窗口由这里按需创建（`tauri.conf.json` 不声明
    // `app.windows`，否则静默启动也会创建 WebView2），所以
    // `decorations/shadow/resizable` 只能落在 builder 上，改配置不生效。
    // 自绘标题栏需要无系统边框；阴影与缩放保留；Win10 无系统圆角，方角观感已接受。
    WebviewWindowBuilder::new(app, MAIN_LABEL, WebviewUrl::App("index.html".into()))
        .title(MAIN_TITLE)
        .inner_size(MAIN_WIDTH, MAIN_HEIGHT)
        .min_inner_size(MAIN_MIN_WIDTH, MAIN_MIN_HEIGHT)
        .decorations(false)
        .shadow(true)
        .resizable(true)
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

/// 等待主线程完成窗口可见性操作的超时。
///
/// 正常情况下主线程消息循环很快应答；超时只用于兜底（例如主线程正在等本线程），
/// 避免一次捕获永久卡在「隐藏窗口」上。
const VISIBILITY_TIMEOUT: Duration = Duration::from_secs(2);

/// 改变主窗口可见性：`visible == false` → `SW_HIDE`，`visible == true` →
/// `SW_SHOWNOACTIVATE`（显示但不激活）。返回是否成功取到窗口句柄。
///
/// 为什么绕过 `WebviewWindow::hide/show`：
/// - `show()` 内部是 `ShowWindow(SW_SHOW)`，会**激活**主窗口，抢走覆盖层刚还给
///   来源窗口的焦点——抓帧结束后主窗口即使恢复，也不该成为前台窗口。
/// - tao 只在「缓存的可见标志发生变化」时才真正调用 `ShowWindow`，因此隐藏与恢复必须
///   成对使用同一套原生调用；`is_visible()` 读的是 `IsWindowVisible`，不受 tao 缓存影响。
///
/// 只在主线程调用（与 `run_on_main` 一致）。
fn set_window_visible(window: &WebviewWindow, visible: bool) -> bool {
    let hwnd = match window.hwnd() {
        Ok(hwnd) => hwnd,
        Err(err) => {
            tracing::warn!(error = %err, "cannot read the main window handle");
            return false;
        }
    };
    // SAFETY: `hwnd` 是本进程主线程上的主窗口句柄；命令只改变可见性，不会销毁窗口。
    let _ = unsafe { ShowWindow(hwnd, if visible { SW_SHOWNOACTIVATE } else { SW_HIDE }) };
    true
}

/// 抓帧期间隐藏主窗口的 [`WindowVisibility`] 实现（design §5 步骤 2）。
///
/// - 隐藏时记录「抓帧前主窗口是否可见」，只隐藏可见窗口；抓帧结束后仅当之前可见
///   且窗口仍然存在时恢复显示（不重建，重建由 [`show_main_window`] 负责）。
/// - 隐藏/恢复都用 [`set_window_visible`]（原生 `ShowWindow`）：恢复走
///   `SW_SHOWNOACTIVATE`，不把焦点从覆盖层刚还给的前台窗口抢走。
/// - 窗口操作统一投递到主线程并**等待应答**，确保 `DwmFlush`/抓帧开始时窗口已隐藏。
/// - 只捕获 `AppHandle`，不捕获窗口克隆：事件监听器存放在窗口内部，捕获克隆会形成
///   引用环，销毁窗口后 WebView2 进程无法退出。
pub struct MainWindowVisibility {
    app: AppHandle,
    was_visible: AtomicBool,
}

impl MainWindowVisibility {
    pub fn new(app: AppHandle) -> Self {
        Self {
            app,
            was_visible: AtomicBool::new(false),
        }
    }
}

impl WindowVisibility for MainWindowVisibility {
    fn hide_own_windows(&self) {
        let visible = run_on_main(&self.app, |app| {
            let Some(window) = app.get_webview_window(MAIN_LABEL) else {
                return false;
            };
            match window.is_visible() {
                Ok(visible) => {
                    if visible {
                        if !set_window_visible(&window, false) {
                            tracing::warn!("failed to hide main window for capture");
                            return false;
                        }
                        tracing::info!("main window hidden for capture");
                    } else {
                        tracing::debug!("main window already hidden; nothing to hide for capture");
                    }
                    visible
                }
                Err(err) => {
                    tracing::warn!(error = %err, "cannot read main window visibility; not hiding");
                    false
                }
            }
        });
        // 只有原本可见时才标记，避免重复隐藏把「需要恢复」抹掉。
        if visible == Some(true) {
            self.was_visible.store(true, Ordering::SeqCst);
        }
    }

    fn show_own_windows(&self) {
        // 抓帧前不可见（冷启动托盘态/用户已关闭）时保持不可见。
        if !self.was_visible.swap(false, Ordering::SeqCst) {
            return;
        }
        // 失败（超时/投递失败）已在 `run_on_main` 内部记录；这里不改变捕获流程。
        let _ = run_on_main(&self.app, |app| {
            match app.get_webview_window(MAIN_LABEL) {
                Some(window) => {
                    // `SW_SHOWNOACTIVATE`：恢复可见但不抢覆盖层刚还给来源窗口的焦点。
                    if !set_window_visible(&window, true) {
                        tracing::warn!("failed to restore main window after capture");
                    } else {
                        tracing::info!("main window restored after capture (without activation)");
                    }
                }
                // 窗口在隐藏期间被销毁（60s 定时销毁）：不重建，用户从托盘打开。
                None => tracing::debug!("main window gone after capture; not recreating"),
            }
        });
    }
}

/// 在主线程执行 `work` 并等待其完成，返回闭包结果。
///
/// Tauri 的窗口 getter 在非主线程调用会阻塞等待主线程应答，因此窗口操作必须在
/// 主线程内完成；这里用 `run_on_main_thread` + 单值通道把结果同步带回调用线程。
fn run_on_main<T, F>(app: &AppHandle, work: F) -> Option<T>
where
    T: Send + 'static,
    F: FnOnce(&AppHandle) -> T + Send + 'static,
{
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let handle = app.clone();
    if let Err(err) = app.run_on_main_thread(move || {
        let value = work(&handle);
        // 接收端可能因超时提前离开；发送失败无需处理。
        let _ = tx.send(value);
    }) {
        tracing::warn!(error = %err, "failed to dispatch window visibility change to main thread");
        return None;
    }

    match rx.recv_timeout(VISIBILITY_TIMEOUT) {
        Ok(value) => Some(value),
        Err(err) => {
            tracing::warn!(error = %err, "timed out waiting for main thread window visibility change");
            None
        }
    }
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
