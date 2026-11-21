# VisRecall 启动方式实施计划

> **For agentic workers:** Use superpowers:executing-plans for checkpointed execution in a separate session. 本次按用户指示在当前会话实施，不自动提交 Git。步骤使用 `- [ ]` 跟踪。

**Goal:** 在「设置 → 通用」保存静默启动或启动时显示主窗口，控制下次冷启动。

**Architecture:** 应用配置持久化单一启动偏好，Tauri 完成初始化后从已加载配置决定是否调用现有窗口入口。React 继续通过现有设置 IPC 和 TanStack Query 读写，不参与冷启动决策。

**Tech Stack:** Rust / Serde、Tauri 2 / tauri-specta、React / TypeScript、TanStack Query、Vitest。

## 全局约束

- 静默默认；不新增 WebView、IPC 命令、DB 字段、启动参数、登录自启或首次运行向导。
- 保留托盘和单实例唤醒、关闭后约 60 秒销毁的行为。读写 UTF-8，代码注释用简体中文，不自动提交 Git。

## 任务 1 · 配置兼容与原子保存

- [x] 在 `crates/library/tests/config_paths.rs` 补两种取值往返、缺省和单字段坏值回退测试；在 `crates/app-core/src/app.rs` 补补丁保留、重开读回、写失败及库不可用测试。在 `crates/app-core/tests/relocate_flow.rs` 验证迁移保留已保存的偏好。
- [x] 在 `crates/library/src/config.rs` 新增 `StartupMode::{Silent, ShowWindow}` 和 `AppConfig::startup_mode`。用仅在磁盘字段生效的反序列化函数把未知值或错误类型回退为 `Silent`；构造默认配置和缺字段均使用 `Silent`。
- [x] 在 `crates/app-core/src/app.rs` 新增 `SettingsPatchInput::startup_mode: Option<StartupMode>`，在现有锁内按「克隆 → 应用补丁 → 原子写入 → 更新内存」处理。热键与悬浮球原有副作用继续在配置锁外运行。`crates/app-core/src/lib.rs` 重导出枚举。
- [x] 验证：`cargo test -p visrecall-library --test config_paths`、`cargo test -p visrecall-app-core startup_mode`。

## 任务 2 · Tauri 冷启动与 IPC 契约

- [x] 在 `src-tauri/src/ipc.rs` 定义严格的 IPC 枚举，补测试验证 `show_window`、缺省补丁与拒绝坏值；给现有 DTO/补丁添加 `startupMode` 并映射核心枚举。
- [x] 在 `src-tauri/src/main.rs` 将托盘打点放到托盘创建成功之后，完成后台启动动作后，仅对 `ShowWindow` 调现有 `show_main_window`；在 `src-tauri/src/window_lifecycle.rs` 更新说明，复用已有窗口显示和日志。
- [x] 运行 `cargo test -p visrecall --bin visrecall -- --ignored refresh_committed_bindings` 生成 `src/lib/bindings.ts`，不手改；再验证 IPC 与绑定一致性。

## 任务 3 · 通用设置、提示与验收

- [x] 更新 `src/features/settings/SettingsDialog.test.tsx` 和其它 `SettingsDto` 测试夹具；覆盖正确选中、只提交该字段、保存期间禁用、失败不切换、读取失败可重试。
- [x] `SettingsDialog.tsx` 增加「通用」但保留默认捕获分区，使用独立的 `StartupSettingsPane.tsx` 渲染受控原生单选和读写状态，复用已有 `patchSettings`；`src/styles/dialogs.css` 沿用行样式和色令牌；`src/app/App.tsx` 仅在迁移横幅补静默提示。
- [x] 同步 `docs/VisRecall-技术架构设计.md` §3.3、§3.4、§5.7、配置示例，更新设计稿状态。运行 `pnpm typecheck`、`pnpm lint`、`pnpm test`、`cargo test --workspace` 和 `git diff --check`；单列非自动化的 Windows 托盘验收结果或待办。

- [ ] Windows 托盘、单实例唤醒与 WebView2 的实机验收由用户自行运行，参见设计稿 §5。
