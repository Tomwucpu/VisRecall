# AGENTS.md · VisRecall 代码导航

给后续 Agent / 开发者的快速上手说明。目标：**十分钟内知道某个功能改哪里、依赖方向是什么、怎么验证**。

---

## 0. 先读什么

按以下顺序阅读即可建立完整心智模型：

1. [docs/architecture.md](docs/architecture.md) —— 唯一技术架构真源（进程线程、真实依赖、17 表 Schema、AI/检索/IPC/缓存契约）
2. `src-tauri/src/main.rs` —— 真实启动顺序（DPI 感知、单实例、安装器注册表选库交接、托盘、`silent` / `show_window` 启动分支）
3. `crates/app-core/src/app.rs` —— 应用编排核心（`AppCore` 持有库、配置、捕获运行时、缩略图池、管线宿主与事件总线）
4. `src/app/App.tsx` —— 前端唯一状态中枢（列表 / 搜索 / 筛选 / 详情 / 多选 / 导入与设置弹窗均在此接线）
5. 具体功能目录 `src/features/<域>/`

内部文档索引（注：`docs/` 为独立本地 Git 仓库 `docs/.git`，已被根 `.gitignore` 忽略）：

| 文件 | 内容 |
| --- | --- |
| `docs/architecture.md` | 完整技术架构、真实 SQLite 表与配置键、底层线程/事务/缓存不变量 |
| `docs/release.md` | Windows NSIS 轻量/离线双安装包构建、签名、干净虚拟机验收与发布流程 |
| `docs/prototype/index.html` | 界面水墨令牌与交互视觉原型（样式取值真源；内含未实现的 V0.5 收件箱/合集等占位，功能范围以源码为准） |
| `docs/eval/queries.template.jsonl` | `visrecall-eval` 检索评测集模板（`crates/eval/tests/smoke.rs` 单测直接读取此路径，勿删改路径） |

---

## 1. 技术栈与常用命令

- **桌面壳**：Tauri 2（Rust + WebView2）+ 原生 Win32/D3D11 覆盖层与悬浮球，仅支持 Windows
- **前端**：React 19 + TypeScript 5.8 + Vite 7 + TanStack Query 5 + Vitest 3，无路由、无第三方 UI 库，手写 CSS
- **后端**：Rust workspace（edition 2021，stable + `x86_64-pc-windows-msvc`）
- **存储**：SQLite（WAL，bundled，FTS5）+ 库目录托管文件 + 外部目录只读引用（`AssetReader`）
- **AI**：本地 PP-OCRv4 + Windows OCR 兜底、本地 `bge-small-zh-v1.5-int8`（`ort` 动态加载 `onnxruntime.dll`）+ 可选云端四协议模型（`openai` / `anthropic` / `ollama` / `rest`）

```powershell
pnpm install

# 完整桌面应用（推荐；debug 构建启动时自动刷新 src/lib/bindings.ts）
pnpm tauri dev

# 只起 Vite（无 Tauri IPC，仅看纯样式/组件）
pnpm dev

pnpm typecheck        # tsc --noEmit
pnpm lint
pnpm test             # vitest run
pnpm build            # 新克隆仓库跑 cargo build / cargo test 前必须先执行一次以生成 dist/

cargo test --workspace
cargo clippy --workspace --all-targets
cargo fmt --all

# 需要真实模型文件的集成测试（发版前必跑）
cargo test -p visrecall-ai -- --ignored

# 模型清单哈希校验 / OCR 字符准确率 / 检索评测 / 构建发布安装包
powershell -File scripts/models-check.ps1 -Dir models
powershell -File scripts/ocr-accuracy.ps1 -Ocr <ocr输出目录> -Annotations <标注目录>
cargo run -p visrecall-eval -- --db <库目录> --queries docs/eval/queries.template.jsonl --report <输出.md>
pnpm release:build
```

PR 前最低验证：`pnpm typecheck && pnpm lint && pnpm test`，以及 `cargo test --workspace`。

---

## 2. 代码地图

### 前端 `src/`

```text
src/
  main.tsx                 React 根：QueryClientProvider + ErrorBoundary
  app/
    App.tsx                全局状态中枢；新增面板/弹窗均在此接线
    ErrorBoundary.tsx
  features/<域>/           按业务域组织，组件 + hook + 同目录 *.test.ts(x)
    shell/                 TitleBar（无边框自定义标题栏）、Sidebar、StatusBar、ToastViewport、usePipelineState
    library/               ItemGrid、ItemCard、SelectionBar、RenameItemDialog、EmptyState、useItems、useSelection
    search/                SearchBar、FilterBar、QueryChips、ActiveFilterChips、ResultGrid、ExplainBar、useSearch、useItemFilters、hitToCard
    detail/                DetailPanel、ImageOverlay
    import/                ImportDialog、ImportDropZone、ImportFileList、DirectoryImportPanel、TextImportForm、ImportTaskPanel
    settings/              SettingsDialog（5 个分栏：general/capture/sources/storage/models）、StartupSettingsPane、ImportSourcesPane、ModelSettingsPane、ModelForm
    common/                ConfirmDialog
  lib/
    api.ts                 前端 IPC 统一包装出口
    bindings.ts            tauri-specta 自动生成物（59 命令），严禁手改
    queryClient.ts         queryKeys 唯一来源 + 默认 Query 配置
    events.ts              后端事件 → Query 局部 patch / remove / invalidate 唯一映射
    itemCache.ts           分页缓存按 id 原地 patch / remove（严格保留 pageParams）
    itemDto.ts             ItemDto → ItemCardData 映射
    itemText.ts            标题显示优先级回退与长度上限常量
    filters.ts             列表筛选状态 ↔ IPC ItemFilters ↔ 时间预设区间换算
    modelConfig.ts         四协议模型配置表单草稿转换与校验
    searchTypes.ts         SearchResponse / 筛选 chip 类型
    errors.ts              ApiError { code, message } 读取约定
    toast.ts, theme.ts, notifications.ts, format.ts, hue.ts
  styles/
    globals.css            样式总入口（@import 顺序即级联顺序）
    tokens.css             水墨设计令牌（对齐 docs/prototype/index.html）
    base.css, layout.css, shell.css, sidebar.css, library.css,
    detail.css, dialogs.css, search.css, models.css, import.css
  test/setup.ts            Vitest + jest-dom
```

前端无全局 store：服务端状态走 TanStack Query，UI 状态放在 `App.tsx` 或对应 `useXxx` hook。

### Rust workspace `crates/` + `src-tauri/`

依赖方向单向无环，**严禁反向引用**：

```text
src-tauri → app-core → { shell → capture, pipeline → search → library, ai } → domain
（注：src-tauri 另直连 capture/library/ai/domain 做壳层胶水；eval → { search, ai, library, domain } 独立于桌面壳）
```

| Crate | 职责 | 关键入口 |
| --- | --- | --- |
| `crates/domain` | 纯类型、`AppError`、Provider/VectorIndex trait、几何与量化计算；零 IO 零 Tauri | `lib.rs`、`providers.rs`、`item.rs`、`insight.rs`、`search.rs`、`error.rs` |
| `crates/library` | 配置/路径、SQLite 迁移、FTS5/int8 向量/insights/标签/云端门控/引用源/清理 | `db.rs`、`migrations/`、`config.rs`、`paths.rs`、`ai_policy.rs`、`items.rs`、`fts.rs`、`vectors.rs`、`import_sources.rs`、`import_tasks.rs`、`snippets.rs` |
| `crates/capture` | 显示器与窗口枚举、Per-Monitor V2 DPI、GDI 抓帧与裁剪（Win32） | `lib.rs`、`grab_gdi.rs`、`window.rs`、`display.rs` |
| `crates/shell` | 全局热键、MonoDrop 悬浮球、D3D11/D2D 覆盖层、剪贴板、OLE 拖放 | `hotkey.rs`、`ball/`、`overlay/`、`clipboard.rs`、`drop_target.rs`、`html_snippet.rs` |
| `crates/ai` | `ort` 动态加载、PP-OCRv4 + Windows OCR 链、BGE 嵌入、四协议云端模型、凭据、模型清单 | `runtime.rs`、`ocr/`、`embedder.rs`、`vlm/`、`keyring.rs`、`models.rs` |
| `crates/search` | 非对称分词、查询解析、FTS 构造、三路召回、RRF 融合、命中解释；**不依赖 `ai`** | `parser.rs`、`filters.rs`、`fts_query.rs`、`execute.rs`、`rank.rs`、`tokenizer.rs` |
| `crates/pipeline` | `jobs` 驱动的两阶段索引与向量蓝绿重建；**不依赖 `app-core` 与 `ai`** | `worker.rs`、`handlers/{ocr,index,vlm,rebuild}.rs`、`lifecycle.rs`、`testing.rs` |
| `crates/app-core` | 核心编排：捕获、导入、检索、详情、WebP 缩略图、存储迁移、事件总线、管线宿主 | `app.rs`、`capture_flow.rs`、`import_flow.rs`、`directory_import.rs`、`search_flow.rs`、`pipeline_host.rs`、`thumbnail.rs`、`events.rs`、`detail.rs` |
| `crates/eval` | 独立检索评测 CLI（`visrecall-eval`） | `src/main.rs`、`src/lib.rs` |
| `src-tauri` | 启动与安装器选库交接、托盘、主窗口生命周期、59 个 IPC 命令、资源协议 | `main.rs`、`installer_selection.rs`、`ipc.rs`、`import_ipc.rs`、`protocol.rs`、`tray.rs`、`window_lifecycle.rs` |

---

## 3. 四条核心数据流

### 3.1 捕获入库

```text
快捷键 / 悬浮球单击（需 AppConfig.capture.enabled == true，默认 false；拖放入库不受此开关限制）
  → crates/shell 投递 ShellEvent
  → crates/app-core/app.rs 主循环（受 CaptureGate 闸门保护）→ capture_flow.rs
  → 原图 PNG 原子落盘 + WebP 缩略图入池 + items(status=parsing, storage_mode=managed) → 发 item_created
  → pipeline 接力入队：图片先入 ocr，ocr 完成后入 index（仅当 Library::cloud_parse_allowed 且已配模型时再入 vlm）
  → 阶段一 index 单事务写 insights + items_fts + 向量 → 发 item_indexed（未开云端则直接置 status=ready）
  → 阶段二 vlm 完成重算 index → 置 ready 并发 item_enriched（失败或 401/403 置 pending，永不自动重试，仅手动 item_reparse 触发）
```

### 3.2 文件 / 目录 / 文本导入

```text
ImportDialog → api.ts → src-tauri/import_ipc.rs（生成选择令牌，真实路径留在 Rust 侧）
  → 文件/文本导入：storage_mode=managed，复制到 originals/ 或 texts/
  → 目录导入：storage_mode=reference，只写 import_sources + external_assets，原文件留原处由 AssetReader 读取
  → 运行期写 import_tasks 并发 import_task_progress；终态发完 import_task_finished 快照后立即删行（不留历史表）
  → 每条新素材继续走 3.1 的索引管线
```

### 3.3 检索

```text
SearchBar → useSearch.ts（250ms 防抖 + 请求序号防乱序）
  → api.search() → ipc.rs::search_items → app-core/search_flow.rs
  → crates/search：
      硬条件（时间、app:X、#标签、引号精确词组、FilterBar）→ 候选集 SQL WHERE（不参与打分，不解析「来自X」）
      软条件（jieba 词边界识别颜色/图表/类型/公式，不从语义词剥离）→ 第三路 facet_rowids
      三路召回（FTS5 BM25 + BGE int8 余弦[0.5 相对阈值] + facet 软命中）→ RRF(k=60, 权重 [1.0, 1.0, 0.6])
  → SearchResponse → hitToCard.ts → ResultGrid / ExplainBar
```

### 3.4 前端状态刷新

后端事件只带 ID / 最小字段 → `src/lib/events.ts` 统一分发：**列表查询严禁调用 `resetQueries`**；单条更新/重索引回执走 `itemCache.ts` 按 `id` 原地 `patchItemEverywhere`（保留 `pageParams`），删除走 `removeItemsEverywhere`，新建/清理走保留式 `invalidateQueries`（同时覆盖 `["items"]` 与 `["items-filtered"]`）。

---

## 4. 常见改动落点速查

| 想改什么 | 主要落点 |
| --- | --- |
| 列表卡片 / 网格 / 多选 / 重命名 | `features/library/{ItemGrid,ItemCard,SelectionBar,RenameItemDialog}.tsx`、`features/library/{useItems,useSelection}.ts`、`lib/{itemDto,itemCache}.ts` |
| 搜索语法 / 排序 / 筛选 / 命中解释 | `crates/search/{parser,filters,rank,execute,tokenizer}.rs`、`features/search/*`、`lib/filters.ts` |
| 详情面板 / 四类显式编辑 / 原图浮层 | `features/detail/{DetailPanel,ImageOverlay}.tsx`、`crates/app-core/detail.rs`、`ipc.rs::get_item_ext`、`library/{insights,snippets}.rs` |
| 导入流程 / 外部引用目录 / 进度 | `features/import/*`、`features/settings/ImportSourcesPane.tsx`、`src-tauri/import_ipc.rs`、`app-core/{import_flow,import_service,directory_import}.rs`、`library/{assets,import_sources,import_tasks}.rs` |
| 设置项（启动 / 捕获 / 引用源 / 存储 / 模型） | `features/settings/*`、`app-core/app.rs`、`library/{config,settings_kv,ai_policy,storage,relocate}.rs` |
| 捕获 / 热键 / 悬浮球 / 覆盖层 / 剪贴板 | `crates/shell/*`、`crates/capture/*`、`app-core/capture_flow.rs`（纯原生线程，不经过 WebView） |
| WebP 缩略图 / 原图资源协议 | `app-core/thumbnail.rs`、`src-tauri/protocol.rs`（`visrecall://thumb\|original/<id>`） |
| 索引管线 / OCR / 嵌入 / 四协议 VLM | `crates/pipeline/{worker.rs,lifecycle.rs,handlers/*}`、`crates/ai/*`、`app-core/pipeline_host.rs` |
| 新增后端事件 | `app-core/events.rs` → `src-tauri/ipc.rs`（`TauriEventSink`）或 `import_ipc.rs` → `src/lib/events.ts` |

### 4.1 新增一个 IPC 命令（标准路径）

1. 新领域类型先加 `crates/domain`（纯数据，不引 Tauri）。
2. 需要持久化则加 `crates/library` 查询或追加迁移（见 4.2）。
3. 在 `crates/app-core` 实现编排方法并补集成测试。
4. 在 `src-tauri/src/ipc.rs`（导入域放 `import_ipc.rs`）：定义 DTO（`#[derive(Serialize, specta::Type)]` + `#[serde(rename_all = "camelCase")]`），编写 `#[tauri::command] #[specta::specta]` 函数，并**务必加入 `specta_builder()` 的 `collect_commands![...]`**。
5. 运行 `pnpm tauri dev` 自动导出 `src/lib/bindings.ts`（禁止手改）。
6. 在 `src/lib/api.ts` 封装调用；有状态副作用则在 `events.ts` 补缓存更新策略，在 `queryClient.ts` 维护 `queryKeys`。
7. 补齐前端同目录 `*.test.ts(x)` 与 Rust `crates/<crate>/tests/` 测试。

### 4.2 新增数据库字段 / 表

1. 在 `crates/library/migrations/` 追加 `00NN_<name>.sql`（当前已到 `0010`），**严禁修改已有迁移**；升级前 `migrations.rs` 会自动备份到 `backups/`。
2. 更新 `crates/library` 对应读写模块与 IPC DTO。
3. 索引相关改动必须遵守「`insights` + `items_fts` + 活跃向量表」单事务同写（参考 `pipeline/src/handlers/index.rs` 的 `write_index_tx`）。
4. `managed` 资产一律存库内相对路径；`reference` 资产 `original_path` 置空，统一走 `AssetReader` 读取。

---

## 5. 编码约定与纪律

- 注释、文档、UI 文案用中文；标识符与 DTO 字段用英文（Rust `snake_case`，IPC 序列化 `camelCase`）。
- IPC 默认统一从 `src/lib/api.ts` 出口。现有直接调 `commands.*` 的例外仅有 3 处：`features/library/useItems.ts`（分页查询）、`lib/itemCache.ts`（单条 `getItem`）、`app/App.tsx`（`restartApp`）；新代码不要再扩大例外。
- 错误统一返回 `ApiError { code, message }`：前端按稳定 `code` 分支，不要把读取失败吞成空数据。
- SQLite 单写连接事务要短，**模型推理与网络请求期间严禁持有数据库锁**。
- 详情可编辑文本（摘要/OCR/正文）必须尊重 `summary_user_edited` / `ocr_user_edited` 写保护位，且在 `>20000` 字截断或 `reference` 模式下禁止写回。
- 云端解析默认关闭，唯一判定入口是 `Library::cloud_parse_allowed`；API Key 只存 Windows 凭据管理器（`vlm-<id>`），绝不落盘、不打印、不回传前端。
- 图片二进制一律走 `visrecall://thumb|original/<id>` 资源协议（Windows 实际为 `http://visrecall.localhost/...`），禁止塞进 IPC JSON。

---

## 6. 已知坑

1. **新克隆首次 Cargo 构建失败**：`src-tauri` 默认启用 `custom-protocol` 内嵌 `dist/`，干净环境直接跑 `cargo build` 或 `cargo test --workspace` 前必须先执行一次 `pnpm build`。
2. **裸跑 `pnpm dev` 报错**：浏览器环境没有 Tauri IPC，联调请用 `pnpm tauri dev`。
3. **新增命令前端找不到**：检查是否漏加进 `ipc.rs` 的 `collect_commands!`，以及是否跑过一次 debug 启动刷新 `bindings.ts`。
4. **窗口与启动模式**：进程默认托盘静默启动（无窗口），但用户可在设置切换为 `show_window`；关闭主窗口只是隐藏并在 60s 后销毁 WebView2，不退出进程。
5. **Rust 无前端热更新**：`vite.config.ts` 忽略 `crates/`、`docs/`、`target/`，改 Rust 后需重启 `pnpm tauri dev`。
6. **Windows DPI**：`main.rs` 第一行必须是 `set_per_monitor_v2()`，否则 150%/200% 缩放下选区坐标错位。
7. **缩略图缓存与检索本地状态**：`thumbUrl` 暂复用 `items.revision`（改备注也会换 URL 重取单图）；`useSearch.ts` 的 `hits` 是本地 state 而非 Query 缓存，改列表事件时留意这两处边界。
8. **`App.tsx` 状态收敛**：新增面板/弹窗优先抽独立 `useXxx` hook，但保持在 `App.tsx` 单点接线，避免各组件各自维护重复全局状态。
