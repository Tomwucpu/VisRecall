// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod ipc;
mod tray;
mod window_lifecycle;

use std::sync::{Arc, OnceLock};
use std::time::Instant;

use tauri::Manager;

/// 进程启动时刻；用于记录「冷启动到托盘可用」耗时。
static PROCESS_START: OnceLock<Instant> = OnceLock::new();

fn main() {
    // 尽量贴近进程启动：在任何初始化之前取时间戳。
    let _ = PROCESS_START.set(Instant::now());
    init_logging();

    let specta_builder = ipc::specta_builder();

    // 绑定只从 debug 构建导出，避免发布产物写源码目录。
    #[cfg(debug_assertions)]
    if let Err(err) = ipc::export_bindings(&specta_builder) {
        tracing::error!(error = %err, "failed to export typescript bindings");
    }

    let app = tauri::Builder::default()
        // 单实例插件必须第一个注册：第二进程在创建任何窗口前退出并唤醒已有实例。
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            tracing::info!("second instance detected; showing main window");
            window_lifecycle::show_main_window(app);
        }))
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(specta_builder.invoke_handler())
        .setup(|app| {
            let sink: Arc<dyn visrecall_app_core::EventSink> =
                Arc::new(ipc::TauriEventSink::new(app.handle().clone()));
            let core = visrecall_app_core::AppCore::open(resolve_paths(), sink);
            let library_ok = core.library_ok();
            app.manage(core);

            // 托盘是冷启动唯一入口；不创建主窗口（架构 §3.3）。
            // 库打不开时托盘进入异常态（红色图标 + 「库不可用」提示），但不阻止驻留。
            tray::create(app, library_ok)?;
            window_lifecycle::install(app);

            // 热键必须在托盘之后启动：注册失败要能立刻把提示挂到托盘上。
            if let Err(err) = app.state::<visrecall_app_core::AppCore>().start_hotkey() {
                tracing::warn!(error = %err, "capture hotkey unavailable; tray stays resident");
            }

            if let Some(start) = PROCESS_START.get() {
                // 冷启动耗时口径：进程启动 → 托盘创建完成（主窗口仍未创建）。
                tracing::info!("cold start to tray: {}ms", start.elapsed().as_millis());
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("tauri build");

    app.run(|_app, event| {
        // 主窗口销毁后进程必须继续驻留托盘；只有显式的 `exit(code)` 才能真正退出。
        if let tauri::RunEvent::ExitRequested { api, code, .. } = event {
            if code.is_none() {
                api.prevent_exit();
            }
        }
    });
}

/// 解析应用路径；环境变量缺失时退回临时目录，保证托盘仍可用并留下日志。
fn resolve_paths() -> visrecall_app_core::AppPaths {
    match visrecall_app_core::AppPaths::resolve() {
        Ok(paths) => paths,
        Err(err) => {
            let fallback = std::env::temp_dir().join("VisRecall");
            tracing::error!(
                error = %err,
                fallback = %fallback.display(),
                "cannot resolve app paths; using temp directory"
            );
            visrecall_app_core::AppPaths {
                config_file: fallback.join("config.json"),
                default_library_root: fallback.join("library"),
            }
        }
    }
}

fn init_logging() {
    use tracing_subscriber::EnvFilter;

    let filter = EnvFilter::try_from_env("VISRECALL_LOG")
        .or_else(|_| EnvFilter::try_new("info"))
        .unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init();
}

/// 生成的绑定必须入库，且必须与当前命令/类型定义一致。
///
/// 测试只导出到临时文件再与受版本控制的文件比对，不会改写工作区文件；
/// 需要刷新时启动 debug 构建（`pnpm tauri dev` 或 `cargo run -p visrecall`），
/// 启动时会把绑定写到真实路径。
#[cfg(all(test, debug_assertions))]
mod bindings_test {
    /// 生成器与仓库文件可能落在不同平台的换行风格上，比较前统一换行。
    fn normalize(text: &str) -> String {
        text.replace("\r\n", "\n")
    }

    #[test]
    fn committed_bindings_are_up_to_date() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let generated_path = dir.path().join("bindings.ts");

        let builder = super::ipc::specta_builder();
        super::ipc::export_bindings_to(&builder, &generated_path).expect("export bindings");

        let generated = std::fs::read_to_string(&generated_path).expect("read generated bindings");
        let committed_path = super::ipc::bindings_path();
        let committed = std::fs::read_to_string(&committed_path).unwrap_or_else(|err| {
            panic!(
                "cannot read {}: {err}; run the app in debug to generate it",
                committed_path.display()
            )
        });

        assert_eq!(
            normalize(&generated),
            normalize(&committed),
            "{} is stale; run the app in debug (pnpm tauri dev) to regenerate it",
            committed_path.display()
        );
    }
}
