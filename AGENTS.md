# AGENTS.md · VisRecall 代码导航

给后续 agent / 开发者的快速上手说明。目标：**十分钟内知道某个功能改哪里、依赖方向是什么、怎么验证**。

---

## 0. 先读什么

按这个顺序读，基本能建立完整心智模型：

1. `docs/VisRecall-技术架构设计.md` §3（进程/线程/依赖）、§5（存储）、§6（检索）、§7（AI 管线）、§8（IPC 与前端）
2. `src-tauri/src/main.rs` —— 真实启动顺序（托盘常驻、主窗口不随进程创建）
3. `crates/app-core/src/app.rs` —— 应用编排核心（`AppCore` 持有库、配置、捕获运行时、事件）
4. `src/app/App.tsx` —— 前端唯一状态中枢（列表 / 搜索 / 详情 / 多选 / 弹窗都在这里接线）
5. 具体功能目录 `src/features/<域>/`

文档索引：

| 文件 | 内容 |
|---|---|
| `docs/VisRecall-技术架构设计.md` | 总架构、捕获、存储、检索、AI、IPC、测试策略 |
| `docs/VisRecall-阶段三-AI与检索架构设计.md` | OCR / 嵌入 / VLM、两阶段索引、三路召回 + RRF |
| `docs/UI-前置阶段实施规划.md` | UI 阶段边界与原型裁剪 |
| `docs/prototype/index.html` | 视觉与交互原型（样式取值来源） |
| `docs/eval/` | 检索评测查询集与报告 |
| `docs/素材列表刷新闪屏-诊断与改造规划.md` | 列表刷新行为的历史决策 |

---

## 1. 技术栈与常用命令

- 桌面壳：Tauri 2（Rust + WebView2），Windows only
- 前端：React 19 + TypeScript 5.8 + Vite 7 + TanStack Query 5 + Vitest 3，无路由、无 UI 组件库，手写 CSS
- 后端：Rust workspace（edition 2021，stable toolchain，MSVC）
- 存储：SQLite（WAL，bundled，FTS5）+ 库目录普通文件
- AI：本地 PP-OCR / BGE 嵌入（ONNX Runtime）+ 可选云端 OpenAI 兼容 VLM

```bash
pnpm install

# 完整桌面应用（推荐；debug 构建会顺带刷新 src/lib/bindings.ts）
pnpm tauri dev

# 只起 Vite，浏览器里没有 Tauri IPC，只能看纯样式/组件
pnpm dev

pnpm typecheck        # tsc --noEmit
pnpm lint
pnpm test             # vitest run

cargo test --workspace
cargo clippy --workspace --all-targets
cargo fmt --all

# 需要真实模型文件的用例（发版前必跑）
cargo test -p visrecall-ai -- --ignored

# 模型文件校验 / OCR 精度
powershell -File scripts/models-check.ps1
powershell -File scripts/ocr-accuracy.ps1
```

PR 前最低验证：`pnpm typecheck && pnpm lint && pnpm test`，以及 `cargo test --workspace`（能用 `pnpm tauri dev` 手点一遍更好）。

---

## 2. 代码地图

### 前端 `src/`

```
src/
  main.tsx                 React 根：QueryClientProvider + ErrorBoundary
  app/
    App.tsx                全局状态中枢；新增面板/弹窗基本都要在这里接线
    ErrorBoundary.tsx
  features/<域>/           按业务域组织，组件 + hook + 同目录 *.test.tsx
    shell/                 标题栏、侧栏、状态栏、Toast、管线状态
    library/               素材网格、卡片、多选、重命名
    search/                搜索框、筛选、结果网格、命中解释
    detail/                详情面板、图片浮层、AI 增强区
    import/                文件/目录/文本导入与任务进度
    settings/              设置弹窗与各 Pane
    common/                通用确认框
  lib/
    api.ts                 前端唯一 IPC 出口：所有 commands.* 都从这里包装
    bindings.ts            tauri-specta 生成，禁止手改
    queryClient.ts         queryKeys 单一来源 + 默认 query 配置
    events.ts              后端事件 → Query 失效范围的唯一映射
    itemCache.ts           列表缓存的局部 patch/remove（保留 pageParams）
    itemDto.ts             DTO → 卡片数据
    errors.ts              ApiError 的 code/message 读取约定
    searchTypes.ts         SearchResponse / 筛选 chip 类型
    toast.ts, theme.ts, notifications.ts, format.ts, hue.ts
  styles/
    globals.css            样式总入口，@import 顺序即级联顺序
    tokens.css             设计令牌（色、间距、圆角、动效）
    <域>.css               与 features 对应的分文件
  test/setup.ts            Vitest + jest-dom
```

前端没有全局 store：服务端状态交给 TanStack Query，UI 状态放在 `App.tsx` 或对应 `useXxx` hook。

### Rust workspace `crates/` + `src-tauri/`

依赖方向保持单向，**不要反向引用**：

```
src-tauri → app-core → { shell, pipeline } → { capture, library, search, ai } → domain
```

| crate | 职责 | 关键入口 |
|---|---|---|
| `crates/domain` | 纯类型 / 错误码 / Provider trait / 几何计算；不依赖任何 IO 与 Tauri | `lib.rs`、`providers.rs`、`item.rs`、`insight.rs`、`error.rs` |
| `crates/library` | SQLite、迁移、库路径、配置、FTS / 向量 / insights 读写、导入源、清理 | `db.rs`（`Library` 句柄）、`migrations/`、`items.rs`、`fts.rs`、`vectors.rs`、`config.rs`、`paths.rs` |
| `crates/capture` | 屏幕抓帧、显示器/窗口枚举、DPI、裁剪（Win32） | `lib.rs`、`grab_gdi.rs`、`window.rs`、`display.rs` |
| `crates/shell` | 全局热键、悬浮球、覆盖层（D3D）、剪贴板、OLE 拖放 | `hotkey.rs`、`ball/`、`overlay/`、`clipboard.rs`、`drop_target.rs` |
| `crates/ai` | OCR 链、BGE 嵌入、OpenAI 兼容 VLM、密钥、模型清单 | `ocr/`、`embedder.rs`、`vlm/`、`keyring.rs`、`models.rs` |
| `crates/search` | 查询解析、FTS 构造、三路召回、RRF 融合、命中解释；**不依赖 `ai`** | `parser.rs`、`fts_query.rs`、`execute.rs`、`filters.rs`、`rank.rs`、`tokenizer.rs` |
| `crates/pipeline` | jobs 驱动的两阶段索引与生命周期；**不依赖 `app-core`** | `worker.rs`、`handlers/{ocr,index,vlm,rebuild}.rs`、`lifecycle.rs`、`testing.rs` |
| `crates/app-core` | 编排：捕获、导入、检索、详情、存储、事件总线、管线宿主 | `app.rs`、`capture_flow.rs`、`import_flow.rs`、`directory_import.rs`、`search_flow.rs`、`pipeline_host.rs`、`events.rs`、`detail.rs` |
| `crates/eval` | 检索评测 CLI | `src/main.rs` |
| `src-tauri` | Tauri 启动、托盘、窗口生命周期、IPC 命令/DTO、资源协议 | `main.rs`、`ipc.rs`、`protocol.rs`、`tray.rs`、`window_lifecycle.rs`、`import_ipc.rs` |

---

## 3. 四条核心数据流

### 3.1 捕获入库

```
快捷键 / 悬浮球 / UI 按钮
  → crates/shell 投递 ShellEvent
  → crates/app-core/app.rs 主循环 → capture_flow.rs
  → 落盘 + items(status=parsing) → 发 item_created
  → pipeline 入队：ocr → index → vlm
  → 阶段一完成发 item_indexed（三路可检索）
  → 阶段二完成发 item_enriched / item_updated（ready / pending）
  → 前端 events.ts 失效对应 Query
```

### 3.2 文件 / 目录 / 文本导入

```
ImportDialog → api.ts → src-tauri/import_ipc.rs（选择令牌，路径留在 Rust 侧）
  → app-core/import_service.rs、directory_import.rs
  → library/import_tasks.rs + 文件落盘
  → 进度事件 import_task_progress / import_task_finished
  → 每条素材继续走 3.1 的索引管线
```

### 3.3 检索

```
SearchBar → useSearch.ts（250ms 防抖 + 请求序号）
  → api.search() → ipc.rs::search_items
  → app-core/search_flow.rs
  → crates/search: 规则解析 → FTS + 向量 + 软命中三路 → RRF → 过滤
  → SearchResponse → hitToCard.ts → ResultGrid / ExplainBar
```

硬条件（时间、`app:X`、`#tag`）只进 WHERE，不参与打分；查询嵌入由 `app-core` 注入 `EmbeddingProvider`，所以 `search` 不依赖 `ai`。

### 3.4 前端状态刷新

后端事件只带 id / 小载荷 → `src/lib/events.ts` 映射到具体 `queryKeys` → TanStack Query 失效或局部 patch。不要在组件里轮询或手动 `refetch` 全表。

---

## 4. 常见改动落点速查

| 想改什么 | 主要落点 |
|---|---|
| 列表卡片 / 网格 / 多选 | `features/library/{ItemGrid,ItemCard,useItems,useSelection}.tsx`、`lib/itemDto.ts` |
| 搜索语法 / 排序 / 命中解释 | `crates/search/{parser,filters,rank,execute}.rs`、`features/search/*`、`crates/search/tests/*` |
| 详情面板 / AI 描述 / OCR 编辑 | `features/detail/*`、`crates/app-core/detail.rs`、`ipc.rs::get_item_ext`、`library/insights.rs` |
| 导入流程 / 任务进度 | `features/import/*`、`src-tauri/import_ipc.rs`、`app-core/{import_flow,import_service,directory_import}.rs` |
| 设置项（容量 / 保留 / 主题 / 模型） | `features/settings/*`、`app-core/app.rs` 的设置方法、`library/config.rs`、`library/settings_kv.rs` |
| 捕获 / 热键 / 悬浮球 / 覆盖层 | `crates/shell/*`、`crates/capture/*`、`app-core/capture_flow.rs`（原生实现，不在 WebView 里） |
| 缩略图 / 原图访问 | `app-core/thumbnail.rs`、`src-tauri/protocol.rs`、`visrecall://thumb/<id>` |
| 索引管线 / OCR / VLM | `crates/pipeline/handlers/*`、`crates/ai/*`、`app-core/pipeline_host.rs` |
| 新增后端事件 | `app-core/events.rs` → `src-tauri/ipc.rs` 的 `TauriEventSink` → `src/lib/events.ts` |

### 4.1 新增一个 IPC 命令（标准路径）

1. 需要新领域类型：先加 `crates/domain`（纯数据，不引 Tauri）。
2. 需要持久化：加 `crates/library` 查询 / 迁移（见 4.2）。
3. 在 `crates/app-core` 实现编排方法，补集成测试。
4. `src-tauri/src/ipc.rs`：
   - 定义 DTO（`#[derive(Serialize, specta::Type)]`，`rename_all = "camelCase"`）
   - 写 `#[tauri::command] #[specta::specta] fn ...`
   - **务必**把函数名加进 `specta_builder()` 的 `collect_commands![...]`，否则前端看不到
5. `pnpm tauri dev`（debug 构建启动时自动导出 `src/lib/bindings.ts`；不要手改这个文件）。
6. 在 `src/lib/api.ts` 包一层方法；默认通过 `api.*` 调用，不新增对 `commands.*` 的直接引用。
7. 有副作用就发事件，并在 `src/lib/events.ts` 补失效映射；新缓存维度在 `queryClient.ts` 加 `queryKeys`。
8. 前端补 `*.test.tsx`，Rust 补 `crates/<crate>/tests/` 集成测试。

### 4.2 新增数据库字段 / 表

1. 在 `crates/library/migrations/` 追加 `00NN_<name>.sql`，**不要改已发布的迁移**。
2. 迁移由 `migrations.rs` 顺序执行；升级前自动备份到 `backups/`。
3. 更新对应模块（`items.rs` / `insights.rs` / `tags.rs` / `settings_kv.rs` 等）与 DTO。
4. 索引类数据要求「元数据 + FTS + 向量」同事务写入；参考 `app-core` / `library` 现有 `*_tx` 函数。
5. 库内文件路径一律存**相对路径**。

---

## 5. 编码约定与纪律

- 注释、文档、UI 文案用中文；标识符 / DTO 字段用英文。IPC 边界 Rust `snake_case`，序列化为 `camelCase`。
- IPC 默认从 `src/lib/api.ts` 出口。现有直接调用只有 `features/library/useItems.ts` 的分页查询与 `app/App.tsx` 的 `restartApp`；新代码不要再扩大这类例外。
- `src/lib/bindings.ts` 是生成物，禁止手改；Rust 改动后靠 debug 启动刷新。
- 错误统一 `ApiError { code, message }`：前端按稳定 `code` 分支，展示用 `message`；不要把「读失败」伪装成空数据。
- 服务端状态只走 TanStack Query；缓存键集中在 `queryClient.ts`，失效集中在一处。局部更新列表时保留 `pageParams`（参考 `itemCache.ts`）。
- SQLite 单写连接：事务要短，**推理/网络绝不能持库锁**。
- 依赖方向不可反转：`domain` 不依赖任何上层；`search` 不依赖 `ai`；`pipeline` 不依赖 `app-core`。
- 云端解析默认关闭，唯一判定入口是 `Library::cloud_parse_allowed`；API Key 只存 Windows 凭据管理器，不落盘、不回传、不打印。
- 大二进制不走 IPC：图片一律走 `visrecall://thumb|original/<id>` 资源协议（Windows 实际为 `http://visrecall.localhost/...`）。
- 捕获 / 覆盖层 / 悬浮球是原生线程，不经过 WebView；改 UI 不会影响它们。
- 测试：前端测试与组件同目录 `*.test.tsx`；Rust 单元测试内联、集成测试放 `crates/<crate>/tests/`；管线用 `pipeline::testing` 假 Provider，事件用 `RecordingSink`。
- `#[ignore]` 的 AI 用例需要 `models/` 下真实模型（含 `onnxruntime.dll`），发版前用 `--ignored` 必跑。

---

## 6. 已知坑

1. 只跑 `pnpm dev` 时没有 Tauri IPC，界面会报错，需要 `pnpm tauri dev`。
2. 新增 Rust 命令后没跑 debug 构建，`bindings.ts` 不会更新；没加进 `collect_commands!` 则命令不存在。
3. 关闭主窗口不等于退出：进程常驻托盘；单实例插件会唤醒已有实例。
4. 主窗口是延迟创建/销毁的（低内存策略），不要假设启动即有窗口。
5. `vite.config.ts` 刻意忽略 `crates/`、`docs/`、`target/`，Rust 改动没有 HMR，需重启 Tauri。
6. SQLite 迁移是只追加的；直接改旧迁移会导致已有库版本号与内容不一致。
7. Windows DPI：`main.rs` 第一条语句必须是 `set_per_monitor_v2()`，否则 150% 缩放下坐标错误。
8. `src/app/App.tsx` 是状态中枢且体量较大；加功能优先抽 hook，但要保持单一接线点，避免各面板各自造状态。
