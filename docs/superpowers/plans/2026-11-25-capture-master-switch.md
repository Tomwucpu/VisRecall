# 截图总开关实施计划

**目标**　用户通过「设置 → 捕获 → 启用截图」控制 VisRecall 的所有屏幕截图入口。

**架构**　应用配置保存一个布尔值，Rust 截图流程用它判断是否接受请求。React 复用设置 IPC 和 TanStack Query。原生快捷键与悬浮球单击继续共用截图流程，悬浮球拖放继续走导入流程。

**技术栈**　Rust / Serde、Win32、Tauri 2 / tauri-specta、React / TypeScript、Vitest。

## 行为

| 场景 | 预期 |
| --- | --- |
| 首次安装或旧版升级，配置中没有开关字段 | 默认关闭，用户手动打开后才启动截图 |
| 打开开关 | 立即启用全局截图快捷键和可见悬浮球的单击截图，保存选择 |
| 关闭开关 | 停止接受新的截图请求，释放全局截图热键。取消尚未提交的选区并恢复窗口，已经开始保存的截图正常收尾 |
| 关闭时单击悬浮球 | 不弹出截图覆盖层，也不弹出错误提示。球仍可移动和接收拖放 |
| 关闭时修改快捷键 | 可录入并保存组合键，保存时校验格式，启用时检查占用 |
| 用户开启时快捷键被占用，或开关保存失败 | 显示原因，保留切换前的状态。组合键被占用时可换键重试 |
| 重启 | 加载上次保存的状态。若已启用但启动时出现热键冲突，沿用现有冲突提示，可继续单击可见的悬浮球截图并在设置中换键 |

开关放在捕获页顶部，标签「启用截图」，说明「开启后可通过快捷键或单击悬浮球截图」。现有「悬浮球显示」仍用于显示或隐藏球。文件、目录和文本导入独立运行，悬浮球可见时仍可拖放入库。当前有效截图入口有全局热键和悬浮球单击，今后增加主界面截图按钮时复用同一后端判定。这样静默启动和关闭主窗口后的常驻状态也能正确执行开关。

## 实施顺序

### 1. 配置和设置补丁

- [x] 在 `crates/library/src/config.rs` 的 `CaptureConfig` 增加 `enabled: bool`，默认 `false`。复用已有 JSON 配置和原子写入，旧配置保留已保存的快捷键与 `realtime_fallback`。
- [x] 在 `crates/app-core/src/app.rs` 的 `SettingsPatchInput` 增加 `capture_enabled: Option<bool>`，沿用 `update_settings` 的合并和原子保存。补丁省略该字段时保留原状态，落盘失败时保持内存和运行时状态。
- [x] 在 `crates/library/tests/config_paths.rs` 覆盖旧配置缺少 `capture` 对象、只有旧字段、开关为真或为假的往返。运行 `cargo test -p visrecall-library --test config_paths`。

### 2. 连接原生入口和运行时

- [x] 在 `crates/app-core/src/app.rs` 和 `crates/app-core/src/capture_flow.rs` 接入开关，使 `CaptureCoordinator::start` 在隐藏窗口和抓帧之前统一检查。关闭时静默忽略原生请求，降级热键消费者也遵守开关。保留迁移闸门和悬浮球移动、拖放路径的独立处理。
- [x] 更新 `AppCore::start_hotkey` 和设置热更新路径，冷启动和库迁移后的恢复都遵守开关。开启时先尝试注册当前或本次修改的组合键，成功落盘后放行截图。落盘失败时释放临时注册。关闭时先阻止截图，再注销热键。串行处理开关和换键操作。
- [x] 在 `crates/shell/src/hotkey.rs` 复用现有退出消息和 `UnregisterHotKey`，确认旧线程完成注销后才允许重新注册同一个组合键。等待在后台进行，等待期间释放配置锁。
- [x] 在截图循环复用 `crates/shell/src/overlay/mod.rs` 的取消路径，结束未提交的选区会话并恢复应用窗口。区分程序取消的确认与用户按 Esc 的普通事件，取消完成后才接受新的截图会话，避免迟到的提交或取消影响下一次截图。
- [x] 在 `crates/app-core/tests/settings_hot_reload.rs`、`crates/app-core/src/capture_flow.rs` 和迁移相关测试中覆盖双入口禁用、开启恢复、热键占用、落盘失败、快速关闭后重开、取消选区和关闭时拖放入库。需要真实注册热键的既有测试夹具显式启用截图。运行 `cargo test -p visrecall-shell` 和 `cargo test -p visrecall-app-core`。

### 3. IPC 和设置界面

- [x] 在 `src-tauri/src/ipc.rs` 扩展 `SettingsDto` 与 `SettingsPatch`，字段序列化为 `captureEnabled`。复用 `get_settings` 和 `update_settings`，测试显式 `false` 与省略字段的区别。运行 `cargo test -p visrecall --bin visrecall -- --ignored refresh_committed_bindings` 重新生成 `src/lib/bindings.ts`。
- [x] 在 `src/features/settings/SettingsDialog.tsx` 复用现有开关样式、`api.updateSettings` 和 `queryKeys.settings`。使用 `role="switch"` 与 `aria-checked`。读取或保存期间禁用开关，保存成功后用后端返回值更新缓存，失败时保留原值并说明原因。
- [x] 更新 `src/features/settings/SettingsDialog.test.tsx` 和现有设置 DTO 测试夹具，覆盖后台状态、提交字段、避免重复提交、保存失败和关闭时依然可修改快捷键。
- [x] 更新 `docs/VisRecall-技术架构设计.md` 的截图触发规则及配置示例。运行 `pnpm typecheck`、`pnpm lint`、`pnpm test`、`cargo test --workspace`。

## Windows 桌面验收

- [ ] 用 `pnpm tauri dev` 启动一份缺少开关字段的测试配置。分别按原快捷键和单击悬浮球，确认无覆盖层、无新增截图。
- [ ] 打开开关，分别用快捷键和单击悬浮球截图，确认均可框选并入库。隐藏悬浮球后验证快捷键仍然可用。
- [ ] 关闭开关，验证拖放图片仍可入库，其他程序可注册原快捷键。释放热键后尝试连续关闭、开启，确认始终可以重新注册。
- [ ] 让其他程序占用组合键，验证开启失败时仍显示关闭，换键可以重试。分别在开和关的状态下退出托盘进程并重新启动，核对状态和入口行为。

实现时使用 UTF-8 和简体中文代码注释，Git 由用户自行提交。
