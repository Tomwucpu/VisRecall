# 导入入口与目录引用实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**目标：** 在主窗口提供统一的图片、TXT/Markdown、文字添加入口，并实现不复制原件、可刷新和可恢复的目录引用导入。

**架构：** 库层使用 `import_sources`、`external_assets`、`import_tasks` 和 `import_task_entries` 保存来源、引用及任务状态，通过 `AssetReader` 统一读取库内副本与外部原件。应用核心负责文件和目录编排，Tauri 负责原生选择器、选择令牌、窗口绑定及拖放边界，React 提供统一弹窗、任务反馈和来源管理。

**技术栈：** Rust、rusqlite、Tauri v2、React 19、TypeScript、TanStack Query、Vitest、Cargo Test。

## 全局约束

- 所有文件使用 UTF-8 读写，新增代码注释使用简体中文。
- 保持既有 `image/text/link` 素材类型，不增加独立文档类型。
- 目录导入只保存应用层引用，不复制原件，不创建系统软链接或 Junction。
- 外部原件始终只读；删除、清库、容量清理和迁移不得修改或删除原件。
- 文件和目录选择器由 Rust 侧 dialog 提供，前端不得持久化原生绝对路径作为选择凭据。
- `src/lib/bindings.ts` 只能由 Specta 测试生成，不手工编辑。
- 云端 AI 默认关闭，按来源授权；首版不增加目录 watcher。
- 不自动提交 Git，不创建或合并分支。

---

### 任务 1：引用存储模型与统一资产读取

**文件：**
- 新建：`crates/library/migrations/0004_import_references.sql`
- 新建：`crates/library/src/import_sources.rs`
- 新建：`crates/library/src/import_tasks.rs`
- 新建：`crates/library/src/assets.rs`
- 新建：`crates/library/tests/import_references.rs`
- 修改：`crates/library/src/lib.rs`、`crates/library/src/migrations.rs`、`crates/library/src/items.rs`、`crates/library/src/purge.rs`
- 修改：`crates/domain/src/item.rs`

**接口：**
- `ImportSource`、`ExternalAsset` 和 `SourceStatus` 表示目录来源及外部文件引用。
- `ImportTask`、`ImportTaskEntry` 和 `ImportTaskStatus` 持久化扫描、导入、刷新及取消状态。
- `AssetReader` 根据素材存储归属解析内容；库内路径继续相对素材库解析，引用路径必须经来源 ID 和相对路径解析。
- `validate_relative_path` 拒绝绝对路径、父目录跳转和其他越界路径。

- [x] 增加迁移并登记迁移版本，保存来源根目录、相对路径、文件版本、来源状态、任务进度及跳过原因。
- [x] 为素材补充存储归属和来源信息，保留既有素材 ID 与类型。
- [x] 实现 `AssetReader`，让缩略图、详情、OCR、索引及 VLM 共用同一读取边界。
- [x] 对外部路径执行规范化、库目录重叠、重解析点和云占位属性检查。
- [x] 删除素材、移除来源和清库只删除索引及派生数据，不删除外部原件。
- [x] 添加迁移、读取、失效、删除保护及移除后重新添加测试。
- [x] 运行 `cargo test -p visrecall-library -p visrecall-domain`。

### 任务 2：文件导入、目录扫描与后台任务

**文件：**
- 新建：`crates/app-core/src/import_service.rs`
- 新建：`crates/app-core/src/directory_import.rs`
- 修改：`crates/app-core/src/import_flow.rs`、`crates/app-core/src/app.rs`、`crates/app-core/src/detail.rs`
- 修改：`crates/app-core/src/thumbnail.rs`、`crates/app-core/src/storage_flow.rs`、`crates/app-core/src/events.rs`
- 修改：`crates/pipeline/src/handlers/mod.rs`、`ocr.rs`、`index.rs`、`vlm.rs`

**接口：**
- `ImportService` 提供文件检查和复制入库，复用既有图片标准化、文本解码及文字保存流程。
- `DirectoryImportService` 提供扫描、确认导入、刷新、重新定位、移除来源和条目分页查询。
- `DirectoryScanOptions` 保存排除规则与限制；扫描结果区分可导入、不支持、超限和其他跳过状态。
- 目录与文件任务在创建后立即返回任务 ID，由后台线程执行并持久化进度。

- [x] 文件导入支持既有图片、TXT、Markdown 和文字，明确报告不支持项并防止重复提交。
- [x] 目录扫描不复制原件，默认排除隐藏和系统项，递归扫描上限为 50,000 项、目录深度上限为 32。
- [x] 扫描阶段只读取元数据；完成前不允许确认导入或读取内容预览。
- [x] 支持扫描和导入进度、分页条目、跳过原因、取消、失败重试及关闭弹窗后的任务恢复。
- [x] 刷新按内容版本更新派生数据，移除旧 OCR 结果；取消或失败不会把已有素材误标为缺失。
- [x] 来源重新定位验证目标目录，移除来源时可选择保留或删除索引；保留后重新添加不重复建素材。
- [x] 来源级 AI 授权与全局隐私设置共同约束后台分析，不在重启后自动启动批量云端任务。
- [x] 添加不支持类型、超限、扫描进度、取消、刷新、重新定位和生命周期测试。
- [x] 运行 `cargo test -p visrecall-app-core -p visrecall-pipeline`。

### 任务 3：Tauri 命令、选择令牌与协议

**文件：**
- 新建：`src-tauri/src/import_ipc.rs`
- 修改：`src-tauri/src/ipc.rs`、`src-tauri/src/main.rs`、`src-tauri/src/protocol.rs`
- 生成：`src/lib/bindings.ts`

**接口：**
- `pick_import_files`、`pick_import_directory` 返回短期选择令牌和文件摘要。
- `start_file_import`、`scan_import_directory`、`start_directory_import` 返回任务 DTO。
- `get_import_task`、`list_import_tasks`、`list_import_task_entries`、`cancel_import_task`、`retry_import_task` 提供任务查询与控制。
- `list_import_sources`、`refresh_import_source`、`relocate_import_source`、`remove_import_source` 提供来源管理。
- `open_reference_file` 只允许打开已登记引用；`set_import_drop_target` 限定原生拖放目标。

- [x] 注册全部导入 IPC，并将选择令牌绑定到创建它的窗口。
- [x] 文件与目录任务创建后立即返回，后台完成时发送任务事件并失效相关前端查询。
- [x] 原生拖放仅在统一弹窗的指定放置区生效，弹窗外拖放不触发导入流程。
- [x] `visrecall://` 继续按素材 ID 提供资源，并以内容 revision 刷新缓存，不开放任意路径协议。
- [x] 系统打开或定位原文件前再次验证素材的引用归属。
- [x] 运行 Specta 生成命令：

```powershell
cargo test -p visrecall --no-default-features --bin visrecall -- --ignored refresh_committed_bindings
```

### 任务 4：统一添加弹窗与来源管理界面

**文件：**
- 新建：`src/features/import/ImportDialog.tsx`
- 新建：`src/features/import/ImportDropZone.tsx`
- 新建：`src/features/import/ImportFileList.tsx`
- 新建：`src/features/import/DirectoryImportPanel.tsx`
- 新建：`src/features/import/ImportTaskPanel.tsx`
- 新建：`src/features/import/TextImportForm.tsx`
- 新建：`src/features/import/ImportDialog.test.tsx`
- 新建：`src/features/settings/ImportSourcesPane.tsx`、`ImportSourcesPane.test.tsx`
- 新建：`src/styles/import.css`
- 修改：`src/app/App.tsx`、`src/features/library/EmptyState.tsx`、`src/features/detail/DetailPanel.tsx`
- 修改：`src/features/settings/SettingsDialog.tsx`、`src/features/shell/StatusBar.tsx`
- 修改：`src/lib/api.ts`、`src/lib/events.ts`、`src/lib/queryClient.ts` 及相关样式和测试

- [x] 在搜索框右侧增加常驻“添加”按钮，并接入空库、普通列表和搜索结果入口。
- [x] 使用单一弹窗承载拖放文件、选择文件、选择目录和新建文字，不叠加业务弹窗。
- [x] 拖放与选择文件共用待导入列表，支持移除误选项并展示不支持原因。
- [x] 目录面板明确说明“引用、不复制”，扫描后显示统计、分页预览及跳过原因，再确认导入。
- [x] 任务面板显示实时进度、完成、失败和取消状态；取消后立即更新缓存，不等待下一次轮询。
- [x] 关闭弹窗后可恢复运行中的任务；未提交文字草稿关闭前要求确认，导入不清空当前查询。
- [x] 状态栏显示后台任务反馈，详情面板显示来源信息并提供打开或定位原文件操作。
- [x] 设置页支持刷新、重新定位、来源级 AI 授权及保留/删除索引的来源移除选择。
- [x] 保持既有设计令牌、配色、系统字体、圆角和明暗主题，不引入新的视觉体系。
- [x] 添加统一入口、草稿保护、扫描分页、进度取消、任务恢复、防重复提交及来源管理测试。
- [x] 运行 `pnpm test`、`pnpm typecheck`、`pnpm lint`。

### 任务 5：最终验证与实机验收

- [x] 重新生成并校验 Specta bindings。
- [x] 运行 `pnpm test`，确认全部 Vitest 测试通过。
- [x] 运行 `pnpm typecheck` 和 `pnpm lint`。
- [x] 运行 `pnpm build`，确认前端生产构建成功。
- [x] 运行 `cargo test -p visrecall-library -p visrecall-app-core -p visrecall-pipeline`。
- [x] 运行 `cargo test -p visrecall --no-default-features --bin visrecall --no-run`。
- [x] 运行 `cargo test --workspace`。
- [x] 运行 `git diff --check`，检查补丁空白错误。
- [x] 检查最终 `git status` 和 diff，确认没有无关格式化改动且未提交 Git。

#### 需要 Windows 实机人工验收，不能由自动化测试代替

- [ ] 原生文件和目录拖放，包括放置区外拖放不触发导入。
- [ ] 真实文件/目录选择器及取消选择行为。
- [ ] 系统打开和定位原文件。
- [ ] 拔盘、目录移动及重新定位恢复。
- [ ] 云盘占位文件的跳过行为。
- [ ] Junction 和符号链接的拒绝行为。
- [ ] 超大目录的扫描性能、交互响应和截图能力。
- [ ] 1,000 个引用文件无原件副本，以及 10,000 个小文件的分页和进度体验。
- [ ] 窗口缩放、窄窗口、键盘焦点和明暗主题的实际视觉效果。
