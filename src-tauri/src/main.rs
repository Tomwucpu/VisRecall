// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod ipc;
mod tray;
mod window_lifecycle;

use std::sync::Arc;

use tauri::Manager;

fn main() {
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
            app.manage(visrecall_app_core::AppCore::open(resolve_paths(), sink));

            // 托盘是冷启动唯一入口；不创建主窗口（架构 §3.3）。
            tray::create(app)?;
            window_lifecycle::install(app);
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

/// 生成的绑定必须入库；没有跑起来 GUI 时用 `cargo test -p visrecall` 刷新。
#[cfg(all(test, debug_assertions))]
mod bindings_test {
    #[test]
    fn exports_typescript_bindings() {
        let builder = super::ipc::specta_builder();
        super::ipc::export_bindings(&builder).expect("export bindings");
    }
}
