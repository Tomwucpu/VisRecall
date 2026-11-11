// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod import_ipc;
mod ipc;
mod protocol;
mod tray;
mod window_lifecycle;

use std::sync::{Arc, OnceLock};
use std::time::Instant;

use tauri::Manager;

/// 进程启动时刻；用于记录「冷启动到托盘可用」耗时。
static PROCESS_START: OnceLock<Instant> = OnceLock::new();

fn main() {
    // 必须是第一条语句：在任何窗口/DC 创建前声明 Per-Monitor V2，否则 150% 屏上的
    // 坐标会被系统虚拟化（design §6）。
    visrecall_capture::set_per_monitor_v2();

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
        .manage(import_ipc::ImportSelectionState::default())
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::DragDrop(event) = event {
                import_ipc::handle_drag_drop(window, event.clone());
            }
        })
        // 单实例插件必须第一个注册：第二进程在创建任何窗口前退出并唤醒已有实例。
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            tracing::info!("second instance detected; showing main window");
            window_lifecycle::show_main_window(app);
        }))
        .plugin(tauri_plugin_opener::init())
        // 目录选择器只在 Rust 侧使用（`pick_directory`），不向前端暴露插件能力。
        .plugin(tauri_plugin_dialog::init())
        // 缩略图资源协议（design §11.3）：Windows 上实际地址为
        // `http://visrecall.localhost/thumb/<id>`，只服务 `thumbs/` 下的文件。
        .register_uri_scheme_protocol(protocol::SCHEME, protocol::handler)
        .invoke_handler(specta_builder.invoke_handler())
        .setup(|app| {
            let sink: Arc<dyn visrecall_app_core::EventSink> =
                Arc::new(ipc::TauriEventSink::new(app.handle().clone()));
            let visibility: Arc<dyn visrecall_app_core::WindowVisibility> = Arc::new(
                window_lifecycle::MainWindowVisibility::new(app.handle().clone()),
            );
            let core = visrecall_app_core::AppCore::open_with_visibility(
                resolve_paths(),
                sink,
                visibility,
            );
            let library_ok = core.library_ok();
            app.manage(core);

            // 托盘是冷启动唯一入口；不创建主窗口（架构 §3.3）。
            // 库打不开时托盘进入异常态（红色图标 + 「库不可用」提示），但不阻止驻留。
            tray::create(app, library_ok)?;
            window_lifecycle::install(app);

            // 覆盖层线程/D3D 设备在启动时预创建；失败只记录，不影响托盘驻留。
            if let Err(err) = app.state::<visrecall_app_core::AppCore>().start_runtime() {
                tracing::warn!(error = %err, "capture runtime unavailable; captures disabled");
            }

            // 热键必须在托盘之后启动：注册失败要能立刻把提示挂到托盘上。
            if let Err(err) = app.state::<visrecall_app_core::AppCore>().start_hotkey() {
                tracing::warn!(error = %err, "capture hotkey unavailable; tray stays resident");
            }

            // 阶段 03：索引管线（worker + 启动自检修复 + pending 重试轮 + 嵌入预热）。
            // 失败不阻塞驻留：检索仍可用（语义未就绪时降级为 FTS）。
            if let Err(err) = app.state::<visrecall_app_core::AppCore>().start_pipeline() {
                tracing::warn!(error = %err, "indexing pipeline unavailable; search degrades to FTS");
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

    // `ort` 开了 tracing feature：加载 dylib、建 session、图优化会打印数百条 info
    // （`GraphTransformer ...`、`Reserving memory ...`），把应用自己的启动日志淹没。
    // 默认只保留它的 warn/error；排障时需要细节用 `VISRECALL_LOG=info,ort=debug`。
    const DEFAULT_FILTER: &str = "info,ort=warn";
    let filter = EnvFilter::try_from_env("VISRECALL_LOG")
        .or_else(|_| EnvFilter::try_new(DEFAULT_FILTER))
        .unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init();
}

/// 生成的绑定必须入库，且必须与当前命令/类型定义一致。
///
/// 测试只导出到临时文件再与受版本控制的文件比对，不会改写工作区文件；
/// 需要刷新时二选一：启动 debug 构建（`pnpm tauri dev` 或 `cargo run -p visrecall`）
/// 会在启动时写真实路径，或者跑
/// `cargo test -p visrecall --bin visrecall -- --ignored refresh_committed_bindings`
/// （不需要拉起窗口，适合只想刷新的场景）。
#[cfg(all(test, debug_assertions))]
mod bindings_test {
    /// 生成器与仓库文件可能落在不同平台的换行风格上，比较前统一换行。
    fn normalize(text: &str) -> String {
        text.replace("\r\n", "\n")
    }

    /// 刷新受版本控制的绑定文件（默认不跑，避免测试改写工作区）。
    ///
    /// 命令与类型的**文档注释**也会进生成文件：改了命令的 doc 就要刷一次，
    /// 否则 `committed_bindings_are_up_to_date` 会失败。
    #[test]
    #[ignore = "writes src/lib/bindings.ts; run explicitly to refresh bindings"]
    fn refresh_committed_bindings() {
        let builder = super::ipc::specta_builder();
        super::ipc::export_bindings(&builder).expect("export bindings");
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
