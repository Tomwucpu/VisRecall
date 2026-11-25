# 目记 VisRecall · 技术架构设计

> 文档版本：v1.2  
> 状态：待评审  
> 日期：2026-11-04  
> 依据：《目记 VisRecall · 产品设计文档 v2.11》（2026-11-03）  
> 范围：Windows 桌面端 MVP 至 V1.0 的技术实现方案；产品行为以 PRD 为准，本文件只回答「怎么实现」。  
> v1.2 修订：V0.1 检索测试素材规模控制在 **100 条以内**，不做万级压测与容量评测，先保证功能闭环落地；检索实现按该规模简化，容量优化上线后按真实数据评估。

---

## 0. 决策摘要

| 决策 | 结论 | 理由 |
|---|---|---|
| 桌面框架 | **Tauri 2.x + Rust** | 低常驻内存、冷启动快；前端框架无关 |
| 主窗口 UI | **React 19 + TypeScript + Vite** | 开发者主栈；组件生态成熟 |
| 悬浮球 / 截屏覆盖层 | **原生 Win32 窗口**（Rust + `windows` crate） | OLE 文本拖放、4K 60fps、低内存，WebView 无法可靠覆盖 |
| 启动方式与 WebView 生命周期 | **默认静默启动，可选启动时显示主窗口；主窗口关闭后隐藏，60 秒未唤醒则销毁** | 静默路径不创建 WebView，打开窗口后仍可释放其资源 |
| 全局快捷键 | `RegisterHotKey` + 独立消息循环线程 | 不做键盘钩子，低侵入、可冲突检测 |
| 冻结帧 | **GDI BitBlt 抓帧 → D3D11 纹理（显存）**；DXGI 作为可选快路径 | 静态画面也能抓；CPU 位图上传后立即释放，全分辨率画面常驻显存 |
| 覆盖层渲染 | D3D11 swap chain + Direct2D（形状/文字） | 4K 多屏 60fps；按显示器 DPI 1:1 |
| 本地存储 | **SQLite（bundled，FTS5）+ `sqlite-vec`**，WAL | 单文件、事务、可导出；全文与向量同库 |
| 全文检索 | jieba 预分词 + FTS5 BM25；P1 增加 trigram 表做字面匹配 | 中文召回质量与精确搜索兼顾 |
| 向量检索 | 本地 bge-small-zh-v1.5（512 维，int8），`ort` 推理 | 离线可用、查询延迟低；模型按库固定 |
| OCR | 主：PP-OCRv5/v4 mobile ONNX；兜底：Windows.Media.Ocr（zh-Hans） | 中文/混排更稳、跨机器一致、带置信度；Windows OCR 零依赖兜底 |
| 视觉理解 | **测试期默认 DeepSeek**，OpenAI 兼容多厂商可插拔 | DeepSeek 视觉 endpoint（官方或第三方托管 DeepSeek-VL2）；Qwen-VL / GLM-4V / 豆包作为备用 |
| 本地模型模式 | **暂不实现本地 VLM**；离线/无 Key 用 OCR 摘录降级 | 无本地模型测试条件；F6.5 推迟，避免把 OCR 当成本地描述 |
| 查询解析 | **V0.1 纯本地规则解析**，不在查询路径调用大模型 | 保证 P95 ≤800ms；大模型只用于入库侧 |
| 索引更新 | **两阶段**：OCR 后先可检索，VLM 结果回来后增强并重建嵌入 | 满足「确认提交到可检索 ≤3s」 |
| 任务执行 | SQLite 持久化任务队列 + 2 路工作线程 + 指数退避 | 崩溃可恢复；不阻塞捕获 |
| 打包 | Tauri NSIS per-user 为主；MSI 可选（需管理员） | 降低安装门槛；ARM64 单独产物 |
| 检索容量验证 | **V0.1 素材测试规模 ≤100 条**，不做万级压测 | 单人项目先保证落地；容量扩展（HNSW/量化）上线后按真实数据评估 |

---

## 1. 目标、约束与边界

### 1.1 产品约束（来自 PRD）

- 仅 Windows：Windows 10 22H2（19045）及以上、Windows 11；x64 与 ARM64。
- 只做主动捕获：无自动采集、无录屏、剪贴板监听默认关闭。
- 数据全本地、明文、用户可见目录；不托管、不隐藏、可整体搬移。
- 默认不联网也能完成捕获、OCR、入库与检索；云端模型是可选增强。
- 失败可见：解析中 / 正常 / 待解析 / 失败四态，提供重试入口。
- 关键性能：捕获响应 ≤150ms；选区交互 60fps；入库可检索 ≤3s（P95）；检索 ≤800ms（P95，素材测试规模 ≤100 条）；托盘常驻 ≤150MB；空闲 CPU <1%；冷启动到托盘 ≤2s。
- 首发范围：V0.1 = 捕获 · 解析 · 检索 · 复制；V0.5 补组织与导出；V1.0 补公式、存储管理、迁移（本地模型 F6.5 推迟）。
- 测试口径：V0.1 关注「功能可用 + 100 条以内素材检索可用」，不以 5 万/10 万条合成库作为验收条件；真实数据增长后再评估索引扩展。

### 1.2 已确认的技术约束

- 单人开发，周期 6 个月以上，按产品级标准交付。
- 开发者以 Web 前端为主；主窗口选择 React。
- AI 能力多厂商可插拔，暂不绑定具体厂商。
- 本地算力暂不确定且无本地模型测试条件：V0.1/V0.5 不实现本地 VLM；离线/无 Key 时用 OCR 摘录降级；F6.5 推迟。
- 内存目标采用 A1 方案：Tauri + 原生悬浮球/覆盖层 + 主窗口按需销毁；托盘常驻目标 40–70MB。

### 1.3 本架构不覆盖

- 具体视觉规范、交互原型与文案。
- 账号体系、订阅、云同步、团队协作。
- 自动更新服务端（V1.0 之后再评估）。
- macOS / Linux 适配。
- 本地 VLM 模式：当前不实现，F6.5 推迟；只保留 `LocalVisionProvider` 接口，不进入排期。
- 上万/十万级素材的容量压测与检索扩展优化（HNSW、量化切换、合成大库基准）：V0.1 不做，上线后按真实数据量评估。
- **与 PRD 的差异：** PRD F6.5 要求「描述与嵌入均在本地完成」；当前决定不实现本地 VLM，仅用 OCR 摘录降级。需回写 PRD 验收口径，否则 V1.0 无法按原文验收。

---

## 2. 技术选型

### 2.1 候选方案对比

| 方案 | 托盘常驻（估算） | 4K 捕获 | 开发效率（Web 背景） | Windows 集成 | 结论 |
|---|---|---|---|---|---|
| **Tauri 2 + 原生 Shell + React** | 40–70MB | 原生 D3D11，可控 | 中（需学 Rust） | 好 | **采用** |
| Tauri 2 + 全 WebView 窗口 | 90–130MB | WebView 渲染 4K 易超预算 | 高 | 中 | 仅用于主窗口/设置 |
| Electron + napi-rs | 180–300MB | 可做，但基础开销大 | 最高 | 中 | 违反常驻 NFR |
| .NET WPF / WinUI 3 + WebView2 | 40–90MB | 原生，最成熟 | 低（需学 C#/XAML） | 最好 | 备选，不作为首选 |
| 全原生 Rust UI（Slint/egui） | 30–50MB | 原生 | 最低 | 好 | 中文输入/富文本/瀑布流风险高 |
| Wails（Go + WebView2） | 60–100MB | 需自行封装 | 中 | 中 | 生态与覆盖层能力弱于 Tauri |

### 2.2 为什么不是 Electron

Electron 的托盘常驻内存通常在 180–300MB，即使隐藏主窗口，主进程与 GPU/Utility 进程仍保留基础开销；冷启动也很难稳定压到 2s 内。产品把「常驻 ≤150MB」写成硬指标，Electron 在起点就不成立。

### 2.3 为什么悬浮球和覆盖层不走 WebView

1. **OLE 拖放**：从 Chrome/Word/微信拖选中文字或链接，Windows 使用 OLE `IDropTarget`，数据格式为 `CF_UNICODETEXT` / `CF_HTML` / `CF_HDROP`。WebView2 的 DOM drop 事件对跨进程文本拖拽不可靠，而这是产品的高频入口。
2. **4K 性能与内存**：覆盖层要显示冻结帧、压暗、八向控制点和实时尺寸标签。WebView2 加载 4K 位图会额外复制，容易突破捕获峰值预算；原生 D3D11 纹理可直接在显存中渲染。
3. **置顶与焦点**：悬浮球必须置顶但不抢焦点、不进任务栏/Alt-Tab；原生 `WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW` 行为可控。

### 2.4 版本与依赖策略

- Rust stable + MSVC 工具链；Tauri 2.x；React 19；Node 22 LTS；pnpm。
- 关键 crate：`windows`、`rusqlite`（bundled）、`sqlite-vec`、`ort`、`reqwest`、`tokio`、`serde`、`image`、`sha2`、`jieba-rs`、`zip`、`tracing`、`keyring`、`global-hotkey`、`tray-icon`。
- 所有依赖锁定 `Cargo.lock` / `pnpm-lock.yaml`；升级前跑完整测试与检索评测。
- 模型与运行时（ONNX Runtime、嵌入模型、PP-OCR 主模型）随安装包分发或首次启动校验下载；许可证在 `THIRD_PARTY_NOTICES` 中列明。

### 2.5 为什么本地存储不用 PostgreSQL

| 维度 | SQLite（采用） | PostgreSQL（不采用） |
|---|---|---|
| 部署 | 嵌入式库，无服务/端口/账号 | 需安装并常驻服务，初始化数据目录 |
| 内存/启动 | 与进程共享，冷启动快 | 独立服务进程，常驻内存高 |
| 分发 | 随应用链接，库是一个文件 | 需处理服务安装、权限、升级、卸载残留 |
| 数据主权 | 库文件路径可见、可整体搬移 | 数据目录 + 服务配置，搬移复杂 |
| 检索 | FTS5 + sqlite-vec，单用户/100 条测试规模足够 | pgvector/全文检索强，但本地单用户过剩 |
| 并发 | 单进程 + WAL 足够 | 多客户端并发强，本产品用不到 |

- 本产品是单用户、单进程、离线优先；没有多客户端并发写入需求。
- 未来若做多设备同步/团队版：服务端用 PostgreSQL 合理；客户端仍保留 SQLite 作为离线缓存，通过同步层合并。不要为了未来可能的需求，把服务端数据库塞进客户端。
- 例外：若产品改为「局域网多人共享一个库」，PostgreSQL 才成立，但这与 PRD 的单机定位冲突。

---

## 3. 总体架构

### 3.1 进程与线程模型

单进程 `visrecall.exe`，避免多进程 IPC 与状态同步成本；线程职责严格隔离。

```
┌────────────────────────────── visrecall.exe ──────────────────────────────┐
│                                                                           │
│  ┌──────────────────┐   ┌────────────────────┐   ┌────────────────────┐  │
│  │ 主线程            │   │ shell-ball 线程     │   │ shell-overlay 线程 │  │
│  │ Tauri/tao 事件循环 │   │ 悬浮球 Win32 窗口   │   │ 每显示器一个覆盖层  │  │
│  │ 托盘 / 主窗口(按需)│   │ 分层窗口 + GDI+     │   │ D3D11 + D2D        │  │
│  └────────┬─────────┘   └─────────┬──────────┘   └─────────┬──────────┘  │
│           │                       │                        │             │
│           │              ┌────────┴────────┐               │             │
│           │              │ 快捷键线程       │               │             │
│           │              │ RegisterHotKey  │               │             │
│           │              └────────┬────────┘               │             │
│           │                       │                        │             │
│           └───────────────┬───────┴────────────┬───────────┘             │
│                           │  AppCore (Arc)     │                         │
│                 ┌─────────┴─────────┐ ┌────────┴─────────┐               │
│                 │ IPC 命令 / 事件    │ │ 事件总线          │               │
│                 └─────────┬─────────┘ └────────┬─────────┘               │
│                           │                    │                         │
│        ┌──────────────────┴─────┐   ┌──────────┴──────────────────┐      │
│        │ library / search       │   │ ai / pipeline               │      │
│        │ SQLite + FTS5 + vec    │   │ OCR / embed / VLM / jobs    │      │
│        └────────────────────────┘   └─────────────────────────────┘      │
│                                                                           │
│  React 19 + WebView2：仅主窗口 / 设置窗口存在时加载                        │
└───────────────────────────────────────────────────────────────────────────┘
```

| 线程 | 职责 | 禁止事项 |
|---|---|---|
| 主线程 | Tauri/tao 事件循环、托盘、按需创建/销毁主窗口与设置窗口 | 不做同步 IO、不跑 OCR、不发云端请求 |
| `shell-ball` | 悬浮球窗口、三态动画、OLE `IDropTarget`、拖放判定 | 不阻塞截屏或搜索 |
| `shell-overlay` | 所有显示器覆盖层窗口、冻结帧渲染、选区输入、裁剪回读 | 不在 UI 消息中做磁盘/网络 IO |
| `hotkey` | 全局快捷键注册与消息循环 | 只投递事件，不执行捕获 |
| 解析工作线程 | 2 路并发：OCR、摘要、嵌入、索引 | 不碰 UI 句柄 |
| Tokio 运行时 | 云端 HTTP、文件 IO、导出、网络状态 | 所有外部请求必须带超时与取消 |
| DB 访问 | 4 个只读连接 + 1 个写连接，WAL | 写操作串行化，读不阻塞 UI |

**COM/初始化**：每个原生 UI 线程独立调用 `CoInitializeEx`；悬浮球线程使用 STA（OLE 拖放要求），覆盖层线程按 Direct2D/COM 要求初始化。D3D11 设备在覆盖层线程创建；swap chain 只在创建它的线程上使用。

**线程间通信**：原生 UI 与核心之间使用 `crossbeam-channel`（结构化消息，不经过 JSON）；React 与核心之间使用 Tauri commands/events；核心内部使用 `tokio::sync` 与事件总线。UI 线程只处理消息，不做重活。

### 3.2 模块划分与依赖

Cargo workspace + 前端目录：

| crate / 目录 | 职责 | 依赖 |
|---|---|---|
| `crates/domain` | 领域模型、ID、错误码、Provider trait、几何类型 | — |
| `crates/capture` | 屏幕采集、窗口枚举、层级、DPI 坐标、裁剪 | domain, Windows API |
| `crates/shell` | 悬浮球、覆盖层、快捷键、OLE 拖放、原生提示 | domain, capture |
| `crates/library` | SQLite、文件布局、迁移、导入导出、保留策略、删除 | domain |
| `crates/search` | 查询解析、FTS、向量、融合、命中解释 | domain, library |
| `crates/ai` | OCR、嵌入、VLM Provider、提示词、JSON 校验 | domain |
| `crates/pipeline` | 任务队列、重试、状态机、崩溃恢复 | domain, library, search, ai |
| `crates/app-core` | 编排、设置、事件总线、生命周期、存储位置切换 | 以上全部 |
| `src-tauri` | Tauri 启动、托盘、IPC 命令、资源协议 | app-core |
| `src/` | React 主窗口与设置页 | 生成的 IPC 客户端 |
| `tests/` | 集成测试、检索评测集、性能基准 | — |

依赖方向单向：`src-tauri → app-core → {shell, pipeline} → {capture, library, search, ai} → domain`。`search` 只依赖 `library` 与 `domain`，不依赖具体厂商；`pipeline` 只依赖 `ai` 的 trait。

### 3.3 启动、空闲与退出

**启动顺序（目标 ≤2s 到托盘可用）：**

1. 设置 Per-Monitor V2 DPI 感知，后由单实例插件检查；第二实例只唤醒现有主窗口并退出。
2. 从 `%APPDATA%/VisRecall/config.json` 加载应用配置；打开 SQLite（WAL、foreign_keys、busy_timeout）并恢复未完成的索引任务。
3. 创建托盘图标并记录托盘就绪时点。库打不开时显示异常托盘，用户仍可打开主窗口。
4. 按现有顺序启动捕获运行时、全局快捷键和索引管线。
5. **静默启动（默认）**不创建主窗口或 WebView2；**启动时显示主窗口**复用手动唤醒入口打开窗口，不等待后台模型或索引任务完成。主窗口可见时点与托盘就绪分别记录。

启动方式可在「设置 → 通用」修改，下次进程冷启动生效；不改变 Windows 登录自启动设置，也不限制托盘或单实例唤醒。

**空闲原则：** 无定时器轮询重活；全屏检测使用 1s 轻量定时器；任务队列用通知 + 下次重试时间唤醒；空闲 CPU <1%。

**退出：** 托盘菜单「退出」→ 停止新任务 → 取消在途 HTTP → checkpoint WAL → 关闭 DB → 销毁原生窗口 → 退出。关闭主窗口不退出进程；托盘常驻。

### 3.4 主窗口生命周期（低内存核心）

```
关闭主窗口
  → 隐藏窗口
  → 启动 60s 计时器
  → 60s 内重新打开：使旧计时器失效，显示原窗口（快）
  → 60s 到：destroy 窗口与 WebView2 进程，释放内存
  → 再次打开：创建窗口 + 加载 React 资源 + 骨架屏
```

- 显示主窗口的冷启动、托盘打开、单实例唤醒均走同一原生入口。静默启动不走该入口，且不在 Tauri 配置中预声明主窗口。
- 托盘不依赖 WebView；设置是主窗口内的弹窗，共用主窗口生命周期。
- 用任务管理器/Process Explorer 验证：主窗口关闭 60s 后，`msedgewebview2.exe` 相关进程应完全退出。
- 重开主窗口的 300–800ms 空白期由原生骨架/加载态遮盖；若实测体验不可接受，可退化为「挂起不销毁」，但常驻内存会升到 90–130MB。

### 3.5 通信模型

- **命令（请求/响应）**：`tauri-specta` 从 Rust 生成 TypeScript 类型与调用封装；前端不手写 payload 类型。
- **事件（推送）**：`item_created`、`item_updated`、`job_progress`、`capture_completed`、`network_status`、`storage_warning`、`provider_error`。事件只带 ID 与变更字段，UI 收到后使 TanStack Query 的缓存失效并重新拉取。
- **原生内部**：`ShellEvent` 枚举走通道；覆盖层返回 `CapturePayload`（各显示器冻结帧句柄、选区、来源窗口信息），不序列化大对象。
- **大二进制**：不通过 IPC 传图片；使用自定义协议 `visrecall://thumb/<item_id>` / `visrecall://original/<item_id>`，由 Rust 校验 ID 后流式返回。

### 3.6 核心数据流

**入库：**

```
快捷键 / 悬浮球点击
  → 记录前台窗口与应用信息
  → 抓帧（GDI 主路径 / DXGI 快路径；每显示器一张 D3D11 纹理）
  → 显示覆盖层，冻结画面
  → 用户框选 / 调整 / Enter
  → GPU 回读选区 → 编码 PNG → 临时文件 → rename 入库
  → items 插入（status=parsing）→ 事件通知 UI
  → 任务队列：缩略图 → 本地 OCR → 临时嵌入 → 可检索
  → 云端 VLM 可用：更新 insight/tags → 重建嵌入与 FTS
  → 云端不可用：OCR 摘录占位摘要（summary_source=ocr），保持 pending，恢复后增强
  → 事件通知 UI（卡片转为正常 / OCR 摘要）
```

**检索：**

```
输入 → 本地查询解析（时间/应用/颜色/图表/类型/语义）
  → 结构化过滤 + FTS(BM25) + 向量 KNN
  → RRF 融合 → 过滤 → 命中解释 → 结果卡片
```

**失败：** 捕获不等待网络；解析失败落「待解析/失败」并保留 OCR 能力；全部降级在 UI 可见。

---

## 4. 捕获子系统

### 4.1 触发与冻结帧

「设置 → 捕获 → 启用截图」控制全局截图快捷键和悬浮球单击。首次安装和旧配置缺少开关时默认关闭，保存后立即生效。关闭时注销截图热键、取消未提交的框选，并继续支持悬浮球移动与拖放导入。

**触发路径：**

1. 快捷键线程收到 `WM_HOTKEY`，或悬浮球收到单击，投递 `CaptureRequest { trigger }`。
2. 覆盖层线程在显示任何窗口**之前**记录前台窗口：`GetForegroundWindow` → PID → 进程名/显示名、窗口标题、DWM 扩展边框、所在显示器。
3. 隐藏所有应用自有窗口（悬浮球、提示气泡、捕获浮层）并用 `DwmFlush` 等待合成，再抓取每个显示器的一帧，冻结为 `FrozenFrameSet`。
4. 预创建（或复用）覆盖层窗口，绑定纹理，显示并聚焦；画面冻结，选区外压暗。

**抓帧实现（GDI 主路径，DXGI 为可选快路径）：**

- 主路径：对每个显示器用 `GetDC(NULL)` + `BitBlt` 抓到 DIB section；抓完一块立即 `UpdateSubresource` 上传到该显示器的 D3D11 纹理，随后释放 CPU 位图。顺序处理，CPU 峰值只保留一个显示器的原始位图（4K 约 33MB）。
- 内存更紧时可按 1024px 高的条带抓取：每条带约 16MB，上传到纹理对应区域后立即释放；整屏画面仍只在显存中完整存在，CPU 峰值进一步下降。条带高度在 spike 中按「150ms 响应」与内存预算调参。
- 冻结帧以 D3D11 纹理常驻显存（双 4K 约 66MB VRAM），覆盖层直接采样渲染；提交时只把选区回读到 staging texture（典型 ≤8MB），编码 PNG 落盘。
- GDI 对静态画面没有「等新帧」问题，兼容性最好；对 DRM/独占全屏可能返回黑屏：DRM 直接丢弃，独占全屏尝试 DXGI，仍失败则进入实时模式或提示无法截取（见 4.10）。
- 可选快路径：若 `IDXGIOutputDuplication` 可用且能在超时内取到帧，则优先用 DXGI 纹理，减少一次 CPU 拷贝；取不到就回退 GDI。两种路径输出相同的 `FrozenFrameSet`。
- 受 DRM 保护的内容：全黑帧或 `ProtectedContentMaskedOut` → 丢弃并提示「该窗口无法截取」。
- HDR 屏幕按 SDR 路径处理，颜色可能偏差；V0.1 不做 HDR 色调映射。
- `WM_DISPLAYCHANGE` / `WM_DPICHANGED` 时重建抓帧资源与覆盖层。

**为什么 GDI 能满足内存预算：** 4K 双屏 BGRA 原始位图约 66MB，但顺序抓取并在上传显存后立即释放 CPU 位图，进程峰值只增加约一个显示器的 33MB；按条带抓取时可降到约 16MB/条带。全分辨率画面常驻显存，CPU 只处理选区。DXGI 快路径进一步省掉这次 CPU 拷贝，但不是功能前提。

**响应目标：** GDI 顺序抓帧 + 立即上传 + 预创建覆盖层，目标「快捷键到冻结帧显示 ≤150ms」；若双 4K 实测超标，优先启用 DXGI 快路径或降低覆盖层渲染分辨率。

### 4.2 窗口识别与层级

**识别：**

- 进程声明 Per-Monitor V2 DPI 感知（在 Tauri 自定义 Windows manifest 中设置）；所有几何统一使用**虚拟屏幕物理像素**坐标。
- 枚举顶层窗口：`EnumWindows`，过滤不可见、`DwmGetWindowAttribute(DWMWA_CLOAKED)`、工具窗口（按需）。
- 精确边界：`DwmGetWindowAttribute(DWMWA_EXTENDED_FRAME_BOUNDS)`，避免 `GetWindowRect` 的不可见边框。
- 鼠标命中：`WindowFromPoint` → 递归 `RealChildWindowFromPoint` 构建从根到子的窗口链；去重后用于滚轮层级切换。
- 桌面空白（`Progman` / `WorkerW` / `Shell_TrayWnd`）→ 整屏。
- 标签显示：窗口标题、类名、进程显示名、宽高。

**层级下钻：** 滚轮在指针下的父/子窗口间循环；每次切换重新计算高亮矩形与标签。UWP/ApplicationFrameWindow 使用扩展边框；无边框/独占全屏识别失败时不报错，直接允许拖拽框选。

**已知限制：** 提权窗口、部分游戏反作弊/独占全屏可能无法高亮；截屏仍可能成功。作为兼容性测试项，不请求管理员权限。

### 4.3 覆盖层

- **每显示器一个无边框置顶窗口**，`WS_POPUP`，尺寸 = 该显示器物理像素；不改变分辨率，不抢任务栏层级之外的控制。
- 一个覆盖层线程拥有全部覆盖层窗口与共享选区状态；鼠标在起始窗口 `SetCapture`，`WM_MOUSEMOVE` 用 `GetCursorPos` 取全局坐标，更新共享选区并刷新所有窗口。
- 渲染：D3D11 swap chain 采样冻结纹理；Direct2D 绘制压暗层、选区边框、八个控制点、尺寸标签、角标。
- 单显示器 4K 用原生分辨率；集显机器可将覆盖层渲染分辨率降为 0.5×（提交仍从全分辨率纹理裁剪）。
- 键盘：Esc 取消、Enter 提交、方向键 1px / Shift+方向键 10px；右键取消。
- 聚焦：`SetForegroundWindow` + `SetFocus`；失败时用 `AttachThreadInput` 兜底；关闭后把焦点还给原窗口。
- 所有应用自有窗口（悬浮球、提示气泡、捕获浮层）在抓帧前隐藏，覆盖层退出后恢复；避免自家 UI 进入截图。
- 冻结角标「已冻结」显示 3s 后淡出；抓帧失败时角标改为「实时模式」。
- 实时模式下覆盖层改为透明分层窗口，不显示冻结帧；提交时现场抓取选区内容。
- 提交后覆盖层立即关闭，落盘/入库/解析异步进行；右下角短暂显示原生「已存入」提示（V0.1 不做备注浮层）。

### 4.4 选区几何与 DPI

所有选区计算在**虚拟屏物理像素**中完成，纯函数化，便于单元测试：

- 八向控制点命中区域按 DPI 缩放；拖内部整体移动；拖拽时距窗口边缘 8px 内吸附；按住 Ctrl 临时关闭吸附。
- 最小选区 10×10px，提交时小于该值直接取消并提示。
- 跨屏：起始点落在某屏内即可跨屏拖拽；选区按各显示器可见范围裁剪；每个覆盖层只绘制与自己相交的部分。
- 多屏/混合缩放：覆盖层窗口按各自 DPI 创建，位置与尺寸均为物理像素；`WM_DPICHANGED` 时只做位置校正，不缩放内容。
- 显示器拔插/分辨率变化：`WM_DISPLAYCHANGE` 后重建覆盖层与抓帧资源；悬浮球校正到最近可见区域。

### 4.5 悬浮球

原生分层窗口（`UpdateLayeredWindow`，32 位 ARGB），GDI+ 绘制圆/长条/阴影/图标；动画通过缓存位图与定时器在 150ms 内完成。窗口样式：置顶、`WS_EX_TOOLWINDOW`（不进任务栏/Alt-Tab）、`WS_EX_NOACTIVATE`（不抢焦点）。

| 状态 | 触发 | 行为 |
|---|---|---|
| 球形态 | 默认；或关闭贴边收起 | 40/48/56px 三档；单击截屏；长按 400ms 菜单；按住拖动 |
| 贴边收起 | 距边缘 ≤12px 松手后 800ms 无交互 | 5×48px 长条；必须有 1px 描边或轻阴影 |
| 悬停展开 | 鼠标进入长条外扩 6px 热区 | 150ms 内滑出；离开 600ms 后回缩 |
| 拖拽接收 | 拖拽物进入球体/热区 | 放大 1.15×；上方浮出动作提示；松手入库 |
| 停用 | 设置关闭；或全屏应用且开启自动隐藏 | 不渲染；快捷键仍可用 |

- 位置按显示器分别记忆（设备名 + 归一化坐标）；重启恢复；任务栏避让（工作区 + 自动隐藏检测，间距 12px）。
- 收起态额外维护一个不可见的 6px 外扩热区窗口承接 hover/drag 命中；长条本身只作视觉锚点，避免 5px 宽度无法命中拖放。
- 交互判定：按下后移动超过 4px 视为拖动，否则为点击；长按 400ms 触发快捷菜单。
- 全屏检测：`SHQueryUserNotificationState`（`QUNS_BUSY` / `QUNS_RUNNING_D3D_FULL_SCREEN` / `QUNS_PRESENTATION_MODE`）+ 1s 定时器；退出全屏恢复。
- 首次运行引导气泡只显示一次；设置项：大小、贴边收起、全屏隐藏、整体关闭。
- 截图已启用时，隐藏悬浮球仍可使用截图快捷键。

### 4.6 拖放判定（OLE）

悬浮球窗口注册 `IDropTarget`（`OleInitialize` + `RegisterDragDrop`），在 `DragEnter/DragOver` 时根据可用格式决定动作并显示提示气泡，松手后读取数据入队。

| 数据格式 | 判定 | 入库 |
|---|---|---|
| `CF_HDROP`（图片文件） | PNG/JPG/WebP/GIF | 每张一条图片素材；按文件名写来源备注 |
| `CF_HDROP`（文本文件） | .txt/.md | 每份一条文本素材；标题取文件名或首个一级标题 |
| `CF_UNICODETEXT` / `CF_HTML` | 非 URL | 文本片段；剥离格式；≤10000 字符，超出拒绝 |
| 单个 URL | 链接 | 抓取页面标题/站点名；抓取失败则标题 = URL |
| `CF_DIB` / 注册的 PNG 格式 | 图片 | 图片素材；走完整视觉管线 |
| 混合图片 + 文本 | 图片优先 | 文本写入备注，不单独建条目 |
| 其他（文件夹/快捷方式/压缩包/PDF/Office） | 拒绝 | 气泡列出支持格式，不产生记录 |

- 拖拽过程中按 Esc 或移出球体 → 取消，不产生记录。
- 文件批量 ≤20；单图 ≤20MB；文本文件 ≤500KB；逐文件校验，部分失败不影响其他文件。
- 图片长边 >8000px：用 WIC 解码到目标尺寸（避免先解码整张超大图）后入库，不提示。
- 拖放来源应用：用松手瞬间的前台窗口记录（悬浮球不激活，前台仍是源应用）。
- 文本文件编码：UTF-8（含 BOM）优先，失败回退 GB18030；统一转 UTF-8 存储，避免中文乱码。
- GIF/WebP 动图：原文件原样保存；缩略图与 VLM 取首帧，详情页可播放原图。
- 链接标题抓取超时 5s；失败时标题回退为 URL，不阻塞入库。
- 提权应用向非提权悬浮球拖放可能被 UIPI 阻止，作为已知限制；不通过提权规避。
- 若拖拽源只提供图片 URL 而无法取得二进制，降级为链接素材并提示「未取得原图」。

### 4.7 快捷键、托盘、全屏与单实例

- **快捷键**：`RegisterHotKey` + `MOD_NOREPEAT`；默认捕获 `Ctrl+Shift+S`，打开主窗口另设（默认 `Ctrl+Shift+Space`）；设置中修改时先试注册，冲突则提示占用。快捷键线程拥有消息循环。
- **托盘**：Tauri tray；菜单「打开主窗口 / 显示或隐藏悬浮球 / 存储占用 / 设置 / 退出」；正常/异常两态，异常悬停显示原因并可跳转设置；左键打开主窗口。
- **全屏**：全屏应用自动隐藏悬浮球，退出恢复；覆盖层仍按快捷键出现（独占全屏可能无法覆盖，作为已知限制）。
- **单实例**：Tauri single-instance 插件；第二实例唤醒主窗口并退出。
- **开机自启**：注册表 `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`，默认关闭，设置中可开；不请求管理员。

### 4.8 剪贴板捕获（P1）

- 默认完全不注册监听、不读取剪贴板。
- 启用后使用 `AddClipboardFormatListener` + 消息专用窗口；只检查 `CF_DIB`/PNG 是否存在，**不读取任何文本格式**。
- 同一图片 10 秒内不重复提示；提示走托盘，不弹窗。
- 关闭开关立即 `RemoveClipboardFormatListener` 并清空已读取的缓存。
- 数据流向页明确写「启用后会读取你复制到剪贴板的图片」。

### 4.9 内存策略

| 场景 | 目标 | 手段 |
|---|---|---|
| 托盘空闲 | 40–70MB | 无 WebView；原生窗口；模型不加载 |
| 主窗口打开 | 160–280MB（含嵌入模型） | WebView2 仅在窗口存在时；关闭 60s 后销毁 |
| 4K 双屏捕获 | 进程工作集增量 ≤100MB | GDI 顺序/条带抓取、上传显存后释放 CPU 位图；冻结帧在 VRAM；只回读选区 |
| 图片导入 | 峰值可控 | WIC 缩放解码；拒绝/降采样超大像素；逐张处理 |
| 解析 | 模型懒加载 | PP-OCR 主路径 ~100–200MB；Windows OCR 兜底 ~10–30MB；嵌入 int8 ~30–60MB；空闲 5 分钟释放 |
| 检索 | 不整库载入内存 | FTS/向量在磁盘；mmap + page cache；分页 |
| 嵌入模型生命周期 | 主窗口打开时保持，销毁时释放（≤60s） | 主窗口打开时后台预热；托盘空闲不占模型内存 |

> 注意：集显机器的「显存」与系统内存共享，4K 纹理与 swap chain 会占用物理内存，需在目标机器实测；不达标时降低覆盖层渲染分辨率或改用 GDI+CPU 代理路径。
> PP-OCR 主路径在解析期间占用约 100–200MB，解析结束且空闲 5 分钟后释放；托盘空闲仍按 40–70MB 目标。

### 4.10 失败与降级

| 场景 | 行为 |
|---|---|
| GDI 抓帧失败 | 重试一次；仍失败 → 实时模式（角标提示） |
| DXGI 快路径失败 | 直接回退 GDI，不影响功能 |
| 实时模式下提交 | 先隐藏覆盖层（避免把选区框截进画面），再用当前可用抓帧路径（GDI/DXGI）截取选区当时内容 |
| 受 DRM 保护/全黑帧 | 丢弃，提示「该窗口无法截取」 |
| 窗口识别失败 | 不高亮；保持拖拽框选可用，不报错 |
| 选区 <10×10 | 取消并提示「选区过小」 |
| 跨屏超出可见区域 | 按可见范围裁剪，尺寸标签显示实际可截取区域 |
| 存储 <2GB | 拒绝新捕获，保留检索与导出；提供清理/更换目录 |
| 显示器拔插 | 重建抓帧资源/覆盖层；悬浮球回到最近可见区域 |
| 覆盖层聚焦失败 | `AttachThreadInput` 兜底；仍失败则提示用户点击一次 |

---

## 5. 数据与存储

### 5.1 库目录

所有内容存放在用户可见的普通文件夹；路径可在设置中查看、打开、更换。默认位置：

```
%USERPROFILE%\Documents\VisRecall\
  library.json                 # 库标识 + 格式版本
  visrecall.db                 # SQLite（WAL）
  visrecall.db-wal
  originals/
    images/2026/11/<uuid>.png
  texts/<uuid>.txt             # 文本原文（UTF-8）
  thumbs/<uuid>.webp           # 480px 长边
  tmp/                          # 临时文件，启动时清理
  backups/                      # 迁移前备份（保留最近 2 份）
```

- `library.json`：`{ "library_id": "b3f1c2d4-5e6f-4a7b-8c9d-0e1f2a3b4c5d", "format_version": 1, "created_at": "2026-11-04T08:00:00+08:00" }`；格式版本高于客户端时拒绝打开并提示升级。
- 数据库中的文件路径一律存**相对路径**，便于整体搬移。
- `%APPDATA%/VisRecall/config.json` 存非敏感配置；API Key 存 Windows 凭据管理器，键名 `VisRecall/<provider_id>`。
- 不在库内放私有加密格式；原图、文本、SQLite 均可用标准工具读取。

### 5.2 数据库

SQLite（bundled，含 FTS5），WAL，`foreign_keys=ON`。示意 schema（最终以迁移脚本为准）：

```sql
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;

CREATE TABLE items (
  rowid INTEGER PRIMARY KEY AUTOINCREMENT,
  id TEXT NOT NULL UNIQUE,
  kind TEXT NOT NULL CHECK (kind IN ('image','text','link')),
  status TEXT NOT NULL CHECK (status IN ('parsing','ready','pending','failed')),
  captured_at INTEGER NOT NULL,
  source_app TEXT,
  source_title TEXT,
  source_url TEXT,
  width INTEGER,
  height INTEGER,
  char_count INTEGER,
  original_path TEXT,
  thumb_path TEXT,
  note TEXT NOT NULL DEFAULT '',
  content_hash TEXT NOT NULL,
  is_starred INTEGER NOT NULL DEFAULT 0,
  exported_at INTEGER,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE INDEX idx_items_captured ON items(captured_at DESC);
CREATE INDEX idx_items_status ON items(status);
CREATE INDEX idx_items_app ON items(source_app);
CREATE INDEX idx_items_hash ON items(content_hash);

CREATE TABLE insights (
  item_id TEXT PRIMARY KEY REFERENCES items(id) ON DELETE CASCADE,
  summary TEXT,
  summary_source TEXT NOT NULL DEFAULT 'none' CHECK (summary_source IN ('none','vlm','ocr','extractive','user')),
  content_type TEXT,
  primary_color TEXT,
  chart_type TEXT,
  ui_type TEXT,
  entities_json TEXT,
  ocr_text TEXT,
  ocr_boxes_json TEXT,
  ocr_engine TEXT,
  ocr_confidence REAL,
  latex TEXT,
  has_formula INTEGER NOT NULL DEFAULT 0,
  model_provider TEXT,
  model_name TEXT,
  confidence REAL,
  summary_user_edited INTEGER NOT NULL DEFAULT 0,
  tags_user_edited INTEGER NOT NULL DEFAULT 0,
  updated_at INTEGER NOT NULL
);
CREATE INDEX idx_insights_color ON insights(primary_color);
CREATE INDEX idx_insights_chart ON insights(chart_type);
CREATE INDEX idx_insights_type ON insights(content_type);

CREATE TABLE tags (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL UNIQUE,
  source TEXT NOT NULL CHECK (source IN ('auto','manual')),
  created_at INTEGER NOT NULL
);
CREATE TABLE item_tags (
  item_id TEXT NOT NULL REFERENCES items(id) ON DELETE CASCADE,
  tag_id TEXT NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
  PRIMARY KEY (item_id, tag_id)
);
CREATE INDEX idx_item_tags_tag ON item_tags(tag_id);

CREATE TABLE tag_suppressions (
  item_id TEXT NOT NULL REFERENCES items(id) ON DELETE CASCADE,
  tag_name TEXT NOT NULL,
  PRIMARY KEY (item_id, tag_name)
);

CREATE TABLE collections (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL UNIQUE,
  description TEXT,
  cover_item_id TEXT,
  sort_mode TEXT NOT NULL DEFAULT 'created_desc',
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE TABLE item_collections (
  item_id TEXT NOT NULL REFERENCES items(id) ON DELETE CASCADE,
  collection_id TEXT NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
  sort_order INTEGER,
  PRIMARY KEY (item_id, collection_id)
);
CREATE INDEX idx_item_collections_coll ON item_collections(collection_id);

CREATE TABLE queries (
  id TEXT PRIMARY KEY,
  raw TEXT NOT NULL,
  parsed_json TEXT,
  result_ids_json TEXT,
  feedback TEXT,
  created_at INTEGER NOT NULL
);

CREATE TABLE jobs (
  id TEXT PRIMARY KEY,
  item_id TEXT NOT NULL REFERENCES items(id) ON DELETE CASCADE,
  stage TEXT NOT NULL,
  revision INTEGER NOT NULL,
  state TEXT NOT NULL CHECK (state IN ('queued','running','succeeded','failed','cancelled')),
  priority INTEGER NOT NULL DEFAULT 1,
  attempts INTEGER NOT NULL DEFAULT 0,
  next_run_at INTEGER,
  last_error TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  UNIQUE (item_id, stage, revision)
);
CREATE INDEX idx_jobs_due ON jobs(state, next_run_at, priority);

CREATE TABLE settings (
  key TEXT PRIMARY KEY,
  value_json TEXT NOT NULL
);

CREATE TABLE schema_migrations (
  version INTEGER PRIMARY KEY,
  applied_at INTEGER NOT NULL
);
```

**FTS5（概念）：**

```sql
CREATE VIRTUAL TABLE items_fts USING fts5(
  title, summary, ocr_text, note, tags, entities, original_text,
  tokenize = 'unicode61 remove_diacritics 2'
);
-- rowid 与 items.rowid 对齐；写入前用 jieba 分词并以空格连接
```

- 写入/更新时由应用层重建该条 FTS：`INSERT INTO items_fts(rowid, title, summary, ocr_text, note, tags, entities, original_text) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)`；删除使用 FTS5 删除指令。
- 使用普通 FTS5 表（保存正文），以支持 `snippet()` / `highlight()`；存储成本换命中高亮能力。
- 查询词同样用 jieba 分词后构造 `MATCH`；短语/引号搜索在 P1 增加独立的 trigram FTS 表。

**向量（概念）：**

```sql
-- V0.1 主路径：普通表存 int8 向量，按候选 rowid 暴力余弦（≤100 条测试规模足够）
CREATE TABLE item_vectors_int8 (
  item_rowid INTEGER PRIMARY KEY REFERENCES items(rowid) ON DELETE CASCADE,
  embedding BLOB NOT NULL,
  dim INTEGER NOT NULL,
  model_id TEXT NOT NULL
);

-- 可选快路径：无结构化过滤时用 vec0 KNN；缺失或不可用不影响检索
CREATE VIRTUAL TABLE item_vectors USING vec0(
  item_rowid INTEGER PRIMARY KEY,
  embedding FLOAT[512]
);
```

- V0.1 以 `item_vectors_int8` 暴力检索为主，实现简单、结果可复现；`vec0` 只作可选快路径，不阻塞落地。
- 维度与模型 ID 记录在 `settings`；同一库内固定。切换嵌入模型时新建 `item_vectors_v2`，后台重建完成后原子切换。
- 优先使用 float16/int8 量化存储（以 `sqlite-vec` 实际版本能力为准）；`VectorIndex` trait 保留，便于上线后替换为 HNSW。
- 向量写入与 FTS、insight 更新在同一事务内完成，保证「可检索」与元数据一致。

**迁移与备份：** 数据库结构升级前备份到 `backups/visrecall.db.bak-<version>`，保留最近 2 份；迁移在事务中执行，失败自动回退并提示。

### 5.3 索引写入与一致性

- 每条素材有 `search_text`（标题、摘要、OCR、标签、实体、备注、原文的组合）；分词后写 FTS。
- 文本/标签/备注变化 → 递增 `revision` → 合并入队 `embed` 与 `fts` 任务；同一素材只保留最新 revision 的任务。
- 标签重命名/合并：事务内更新 `tags.name`，批量重建受影响素材的 FTS；嵌入任务合并入队。
- 普通捕获不做去重，重复画面各自入库；`content_hash` 只用于迁移合并与可选的 insight 缓存。
- 迁移包导入按 `content_hash` 去重：命中则合并标签/备注（并集），不覆盖已有内容；未命中则新增。
- 崩溃恢复：启动时 `running → queued`；FTS/向量/DB 同事务，避免半更新。

### 5.4 文件一致性

- 图片：写入 `tmp/<uuid>.tmp` → `fsync` → `rename` 到 `originals/images/YYYY/MM/<uuid>.<ext>` → 插入 DB；任一步失败清理临时文件。
- 删除：DB 事务删除素材及派生数据 → 提交 → 删除文件；文件删除失败进入 `gc` 重试队列；启动时扫描孤儿文件。
- 缩略图可重建；缺失时按需生成，不阻塞列表。
- 导出与迁移只读库，不修改原始数据；导入先写临时目录，校验后再合并。

### 5.5 存储位置迁移与导出

**迁移存储位置（F6.1）：**

1. 停止解析任务与新捕获；checkpoint WAL。
2. 目标目录写入 `migration.json`（源、目标、总数、已完成、校验和）。
3. 逐文件复制 + 校验；支持中断续传。
4. 打开目标库做完整性与行数校验。
5. 原子更新 `config.json` 中的库路径；重启服务。
6. 原位置保留（用户可选保留天数），到期后清理；**校验失败绝不删除源**。

**单条导出 Markdown（F5.1，P1）：**

- YAML front matter：标题、时间、来源、标签；正文含摘要与原始内容；图片复制为同目录相对路径文件；文本导出完整原文；链接导出 URL 与标题；**OCR 文本不写入**。
- 文件名：`YYYY-MM-DD-<摘要前 20 字>-<短 ID>.md`；短 ID 用于稳定覆盖，Windows 非法字符与保留名清洗；重复导出按稳定文件名覆盖，不产生副本。

**合集导出（F5.2，P1）：** 一份合集 `.md`（目录锚点、按时间或手动排序）+ `images/` 文件夹。

**完整数据导出（F7.6，P1）：** ZIP，含 `README.md`、`metadata.json`、每条素材的 `.md`、原图；不依赖本产品即可读取。

**迁移包（F7.7，P1）：** 单个 `.visrecall.zip`，含 `manifest.json`（格式版本、应用版本、库 ID、计数、校验和）、`visrecall.db`、`originals/`、`texts/`、`thumbs/`；不含密钥。导入支持「合并」或「新建库」；高版本包拒绝。

### 5.6 保留策略与彻底删除

- 默认永久保留；可选 30/90/180 天或 7–3650 天自定义；切换前显示预估删除数量并二次确认；清理前 3 天预告；星标与已导出素材可豁免。
- 删除单条：删除 items/insights/tags 关联/合集关联/查询结果引用/FTS/向量/文件/缩略图/任务；事务 + 文件 GC。
- 删除全库：清空 items 及派生数据、文件、FTS、向量、任务；标签/合集/查询历史按「删除全部数据」一并清空；设置与密钥保留（另有「重置应用」入口）。
- 明文存储下不承诺 SSD 物理不可恢复；数据流向页如实说明。

### 5.7 配置与密钥

- `config.json`：库路径、快捷键、悬浮球设置、保留策略、Provider 配置（不含 Key）、嵌入模型 ID、窗口状态、语言。
- 启动方式使用应用级 `startup_mode`：`silent`（默认）或 `show_window`。它不随素材库位置改变；缺少字段或该字段损坏时只回落此字段，不重置其它有效设置。
- 屏幕截图偏好保存为应用级 `capture.enabled`，默认 `false`，不随素材库路径改变。关闭期间编辑组合键会保存设置，开启时尝试注册。
- 原子写入（临时文件 + rename）；整体读取失败沿用默认值与现有日志/备份逻辑。
- API Key：Windows 凭据管理器；日志与崩溃信息中脱敏；导出/迁移包不包含 Key。
- 库路径不可用（如移动硬盘拔出）：静默启动不强制显示窗口，托盘提示库不可用；打开主窗口后展示现有的错误状态。

### 5.8 日志与隐私

- `tracing` 滚动日志（本地，保留 7 天 / 20MB）：记录 ID、耗时、错误码；**不记录 API Key、截图内容、OCR 全文**。
- 无遥测上传；度量数据只在本机聚合，用户可主动导出。
- 崩溃转储仅本地可选生成，不上传。
- 数据流向页由 Provider 配置生成：本地保留项、库路径、发送给哪家模型的数据类型（图片/文本）与端点；路径可直接打开文件夹。

---

## 6. 检索

### 6.1 查询解析（V0.1 本地规则）

输入一句话，解析为：

```ts
type ParsedQuery = {
  filters: {
    time?: { from: number; to: number; label: string };
    app?: string[];
    color?: string[];        // 12 色枚举
    chart?: string[];
    contentType?: string[];
    hasFormula?: boolean;
  };
  semanticText: string;      // 剩余语义部分
  lexicalTerms: string[];    // FTS 词
  phrases: string[];         // 引号内字面短语（P1）
};
```

解析规则（确定性、可测试）。`filters` 中的合集/标签由 UI 筛选器直接提供，不从自然语言解析；其余字段由文本解析：

- **时间**：今天/昨天/前天/本周/上周/本月/上月/最近 N 天/最近 N 周/`YYYY-MM`/`YYYY-MM-DD`/周X；统一转本地时区时间戳区间。
- **颜色**：12 色枚举 + 同义词（橙/橘/橙色/橘色、蓝/天蓝/深蓝等）；输出色名而非色值。
- **图表**：折线/柱状/条形/饼图/散点/漏斗/表格/热力/雷达。
- **类型/界面**：截图/图片/文本/链接/公式/界面/定价页/仪表盘/登录页/表单/白板/论文等。
- **应用**：匹配已知进程显示名与库内出现过的来源应用。
- **引号**：P1 作为字面短语，走 trigram FTS；无结果时提示可放宽为语义搜索。
- **剩余文本**：作为语义查询；若为空则只做结构化过滤。
- **chip 回显**：每个条件带 label 与删除动作；用户删除 chip 后后端按剩余条件重算；**V0.1 不在查询路径调用大模型**，避免延迟与不可解释性。

### 6.2 召回

1. **结构化过滤**：时间/应用/颜色/图表/类型/合集/标签 → SQL 条件，命中主键集合。
2. **FTS**：`lexicalTerms` 分词后查 FTS5，BM25 取前 200。
3. **向量**：`semanticText` 本地嵌入 → 向量检索前 200。
   - V0.1 不做分场景 KNN/暴力切换：素材测试规模 ≤100 条，统一在普通表的 int8 向量副本上暴力余弦，逻辑最简单、结果可复现。
   - `sqlite-vec` vec0 保留为无过滤场景的 KNN 快路径；其版本是否支持候选集过滤不影响暴力路径。
   - 「KNN 取 1000 再过滤回填」等大候选集分支不在 V0.1 实现，避免为未验证的容量提前增加复杂度。
4. **字面短语（P1）**：trigram FTS 过滤 OCR/标题/备注/原文，结果并入候选。
5. **纯结构化查询**：按 `captured_at DESC` 分页，不走向量。

### 6.3 融合与排序

- **RRF**（Reciprocal Rank Fusion）：`score = Σ w_i / (60 + rank_i)`，FTS 与向量默认等权；权重写入配置，便于用评测集调参。
- 同分时按 `captured_at DESC` 打破平局；`failed` 素材仍可被 OCR 命中，但卡片明确标注状态。
- 不引入时间衰减作为默认为主排序项，避免「越新越相关」破坏语义检索；是否加权由评测集决定。
- P2 相似推荐直接复用向量近邻（排除自身），不新增存储。

### 6.4 命中解释

```ts
type Explanation = {
  filters: string[];    // 命中的结构化条件，如 ["橙色", "漏斗图"]
  terms: string[];      // FTS 实际命中的词
  semantic: boolean;    // 是否仅语义命中
};
```

- 解释字段只来自**实际命中的索引项**：结构化条件来自 item 的属性；词项来自 FTS `highlight()`/分词交集；标签/实体交集可补充。
- 仅向量命中且无语义词交集时，显示「语义相关」，**不编造**具体原因。
- OCR 命中词通过 `ocr_boxes_json` 映射到预览图上的高亮框；无法映射时退化为文字高亮。
- OCR 降级素材没有颜色/图表/视觉属性；相关筛选器不会命中，解释中也不得出现未识别的视觉属性。

### 6.5 延迟预算（V0.1 测试规模 ≤100 条，P95 目标）

以下预算按 100 条以内素材估算，用于保证交互流畅，不以上万条压测作为当前目标。各阶段为上限参考，该规模下实际耗时通常远低于预算，整体满足 ≤800ms 即可。

| 阶段 | 预算 |
|---|---|
| 查询解析 | ≤5ms |
| 查询嵌入（本地，int8，缓存命中） | 30–80ms |
| FTS BM25 | 20–60ms |
| 向量检索 | 30–100ms |
| 融合 + 过滤 + 解释 | ≤15ms |
| DB 取详情 + 序列化 | ≤20ms |
| **合计** | **约 100–280ms，留足 800ms 余量** |

- 查询嵌入做 LRU（256 条）；重复搜索直接复用。
- 结果先返回卡片所需字段，详情按需拉取；缩略图走自定义协议懒加载。
- V1.0 如加入云端重排，单独预算 ≤1.5s，不改变本地路径。

### 6.6 容量与扩展（不在 V0.1 验收范围）

- V0.1 只验证功能闭环与 **100 条以内素材**的检索可用性，**不构建万级合成库，不做检索容量压测**。
- 架构上保留基本防护，避免把库整库载入内存：FTS/向量存磁盘、top-K 上限、keyset 分页、列表虚拟滚动。
- 上线积累真实数据后，如检索延迟明显退化，再评估 int8/float16 量化与 HNSW（保留 `VectorIndex` trait，不改业务层）。
- FTS `optimize` 与「库过大提示归档」等容量策略推迟到 V1.x，不作为当前排期与验收项。

### 6.7 离线与降级

- 查询解析、FTS、向量嵌入、向量检索全部本地，离线检索可用。
- 云端 VLM 不可用时，已入库素材仍可通过 OCR/临时嵌入检索；未解析素材标记「待解析」。
- 无本地嵌入模型（首次未加载）时，检索退化为 FTS + 结构化过滤，并在状态栏标注。

### 6.8 检索评测

- 维护 50 条真实查询 + 标注相关素材的评测集（私有，不入库分发）。
- 每次排序/分词/嵌入变更后跑 Top-3 命中率；V0.1 退出条件 ≥80%。
- 同时记录查询耗时、命中原因分布、用户「不是这个」反馈，形成自有评测数据。

---

## 7. AI 解析管线

### 7.1 Provider 抽象

```rust
#[async_trait]
pub trait VisionProvider: Send + Sync {
    async fn describe(&self, image: ImageInput, ctx: ItemContext) -> Result<InsightDraft>;
}

#[async_trait]
pub trait TextProvider: Send + Sync {
    async fn summarize(&self, text: &str, ctx: ItemContext) -> Result<InsightDraft>;
}

#[async_trait]
pub trait OcrProvider: Send + Sync {
    async fn recognize(&self, image: &ImageRef) -> Result<OcrResult>;
}

#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    fn id(&self) -> &str;
    fn dim(&self) -> usize;
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
}
```

- **云端 VLM**：OpenAI 兼容 `/v1/chat/completions`，`image_url` 传 base64 data URL；`response_format=json_object`（支持的厂商）；温度 0.1；超时 60s；连接超时 5s。
- **本地 VLM**：预留 `LocalVisionProvider` 实现位；当前不实现、不排期；接入 llama.cpp/Ollama 或 ONNX 时不改业务层。
- **能力矩阵**：每个 Provider 声明支持 `vision` / `text` / `embedding` / `ocr`；UI 按能力选择，避免把嵌入模型配给 VLM 槽位。
- **主备**：每种能力可配主 Provider 与备用 Provider；401/403 不无限重试，标记配置错误并通知托盘。
- **密钥**：只在 Rust 侧读取凭据管理器；请求日志脱敏；前端永远拿不到 Key。

**测试期 Provider 配置（2026-11）：**

| 能力 | 默认 | 备用 | 说明 |
|---|---|---|---|
| 视觉理解 | DeepSeek 视觉 endpoint（DeepSeek-VL2；官方或第三方托管） | SiliconFlow Qwen-VL / 智谱 GLM-4V / 豆包 | 必须通过图像能力探测；不接受图像则标记 text-only 并切换备用 |
| 文本摘要 | DeepSeek `deepseek-chat` | Qwen / GLM | 只用于入库侧摘要；查询路径不调用 |
| 嵌入 | 本地 bge-small-zh-v1.5 | — | 不接云端嵌入；换模型需重建向量 |
| OCR | 本地 PP-OCR ONNX（主） / Windows.Media.Ocr（兜底） | — | 不接云端 OCR；PP-OCR 带置信度 |

- **DeepSeek 视觉接入注意**：DeepSeek 官方 API 以文本模型为主；是否提供视觉模型、模型名与 endpoint 以你账号当前可用能力为准。若官方 endpoint 不支持 `image_url`，把 `base_url` 指向第三方托管的 DeepSeek-VL2（如 SiliconFlow），或直接使用 Qwen-VL/GLM-4V。
- **能力探测**：保存配置或点击「测试连通性」时，视觉 Provider 必须发送一张小图并校验返回；只测文本不算通过。失败返回 `vision_unsupported`，UI 提示切换模型。
- **JSON 模式**：支持 `response_format=json_object` 的厂商直接用；不支持的走提示词约束 + JSON 修复；DeepSeek 文本/视觉模型均按此规则。
- **不要混用嵌入空间**：DeepSeek/Qwen 的文本模型不能替代本地嵌入；嵌入模型固定为库级配置。

### 7.2 OCR（PP-OCR 主，Windows OCR 兜底）

- **主路径 PP-OCR ONNX**：PP-OCRv5/v4 mobile 的检测 + 识别 + 方向分类模型，`ort` CPU 推理；模型随包分发或首次校验下载，约 10–20MB；懒加载，空闲释放；输出行文本、包围盒与识别置信度。
- **兜底 Windows.Media.Ocr**：PP-OCR 模型缺失、加载失败或推理异常时使用；优先 `zh-Hans` 识别器，不可用时回退用户语言/英文；零模型下载、快，但无置信度且依赖系统 OCR 语言包。
- **低置信度处理**：PP-OCR 平均置信度 <0.6 或有效行过少时，保留索引并标记低置信度（`ocr_confidence`），详情页提示「文字识别质量较低」；默认不自动改用 Windows OCR（后者无置信度、未必更好），可提供「用 Windows OCR 重试」。
- **输入**：截取后的原分辨率图片；超大图先按 8000px 长边降采样；不送云端 OCR，避免隐私与延迟。
- **精度目标**：PP-OCR 主路径在清晰截图上字符准确率 ≥97%；用 100–200 张真实截图验证。公式不靠 OCR，P1 由 VLM 输出 LaTeX。
- **文本素材**：不经过 OCR；直接全文索引。
- **链接素材**：抓取页面标题/站点名；正文不抓取（产品只要求标题与 URL）。

#### OCR 选型对比与验证

| 引擎 | 中文/混排 | 小字/密集 | 表格结构 | 公式/手写 | 离线 | 体积/依赖 | 速度 | 置信度 |
|---|---|---|---|---|---|---|---|---|
| Windows.Media.Ocr | 中–良 | 中 | 无 | 弱 | 是 | 系统语言包，零模型 | 快 | 无 |
| PP-OCR ONNX（v5/v4 mobile，RapidOCR 模型） | 良–优 | 良 | 需 PP-Structure | 弱（需公式模型） | 是 | 模型约 15MB + ONNX Runtime | 中 | 有 |
| Tesseract 5 | 中 | 中–差 | 弱 | 弱 | 是 | 小 | 中–慢 | 有 |
| EasyOCR | 良 | 中 | 弱 | 弱 | 是 | PyTorch，重 | 慢 | 有 |
| 云端 OCR（百度/腾讯/Azure 等） | 优 | 优 | 优 | 中 | 否 | 无本地模型 | 网络延迟 | 有 |
| VLM OCR（Qwen-VL/DeepSeek-VL） | 优 | 优 | 良 | 良 | 云端；本地需 GPU | 高成本/大模型 | 慢 | 无/低 |

- **Windows OCR**：零依赖、低内存、快，适合清晰 UI/网页截图；依赖系统 zh-Hans OCR 语言包，密集排版、低对比、表格/公式较弱，且无逐词置信度。
- **PP-OCR ONNX**：中文与混排更稳，置信度可用于质量门控，跨机器一致；代价是模型 + ONNX Runtime、CPU/内存与实现复杂度。RapidOCR 只是 PP-OCR 模型的部署封装，不是另一套引擎。
- **不推荐**：Tesseract（中文截图与密集排版弱）、EasyOCR（PyTorch 运行时过重）。
- **云端 OCR / VLM OCR**：精度更高，但违反「本地优先、离线可用」，只能作为用户显式开启的高精度选项；公式/表格优先交给 P1/P2 的专用能力，不让 OCR 引擎硬扛。
- **决定（2026-11）**：**PP-OCRv5/v4 mobile ONNX 为主路径，Windows.Media.Ocr 为兜底**。A/B 用于验证 97% 目标、延迟与内存，不再决定主备顺序；若 PP-OCR 在目标机器上不达标，回退顺序或启用云端高精度选项需重新评审。

### 7.3 嵌入

- **模型**：bge-small-zh-v1.5，512 维，int8 量化，`ort` CPU 推理；目标单条 <30ms，批量 <200ms。
- **模型随包分发**：约 30–60MB，保证首次启动离线可用；版本与校验和写入配置。
- **文档构造**：标题 > 摘要 > 标签 > 实体 > 备注 > 原文/OCR 前 N 字；按 token 预算截断（约 512 tokens）。
- **切换模型**：新建向量表后台重建，完成后原子切换；重建期间检索继续用旧表；失败保留旧表并提示。
- **查询嵌入**：与文档同一模型；LRU 缓存；离线可用。
- **文本/标签/备注变更**：只重嵌入，不重跑 VLM。

#### 文本素材的索引范围

文本素材（拖入的片段、.txt/.md 文件、链接）不走 OCR，建立三层索引：

1. **全文索引（必有）**：原文完整写入 FTS5，jieba 分词；片段 ≤10,000 字符、文件 ≤500KB 都全量入索引，不截断。这是关键词、数字、专有名词、精确短语的主通道。
2. **向量索引（必有）**：把「标题 + 摘要 + 标签 + 实体 + 备注 + 原文（按 token 预算截断）」拼成 search document，用本地 bge-small-zh 生成一条文档级向量。摘要/标签只是加权字段，**不是索引的唯一依据**。
3. **结构化字段**：来源应用、捕获时间、类型、合集、标签；链接额外有 URL、页面标题、站点名。

- **没有云端 Key 也能索引**：摘要和标签需要云端模型，但全文索引与本地嵌入不依赖它们；文本素材入库后即可检索。
- **链接素材**只索引 URL、页面标题、站点名与用户备注，不抓取网页正文（产品范围）。
- **备注/标签/摘要变化**：重建该条 FTS，并用新的 search document 重嵌入；原文不变时也要重建，因为摘要/标签是向量的一部分。
- **长文本**：V0.1 只有一条文档级向量，语义召回对尾部内容较弱；全文检索不受影响。V0.5 若文本文件成为高频入口，增加分块向量（按 300–500 token 切块，多向量按 item 聚合），FTS 不变。
- **图片素材同理**：OCR 全文进 FTS；摘要/标签/视觉属性进入向量与结构化过滤。

### 7.4 视觉理解（VLM）

- **输入**：入库图片（导入图先去 EXIF，截图无 EXIF）；长边压到 Provider 限制（通常 2048），JPEG q85，<5MB；不发送本地路径/文件名。
- **输出 JSON（严格 schema）**：

```json
{
  "summary": "一句话摘要，≤120 字（目标 80 字内）",
  "content_type": "screenshot|photo|diagram|document|whiteboard|ui|other",
  "primary_color": "orange|red|blue|green|yellow|purple|pink|brown|black|white|gray|cyan",
  "chart_type": "line|bar|pie|scatter|funnel|table|none",
  "ui_type": "pricing|dashboard|login|form|article|other|none",
  "entities": ["竞品 A", "定价页"],
  "layout": "三栏布局，顶部导航，底部 logo 墙",
  "has_formula": false,
  "tags": ["定价页", "橙色", "漏斗图"],
  "confidence": 0.86
}
```

- **校验**：`serde` 严格反序列化；去掉 Markdown 代码围栏；字段越界/枚举非法 → 修复一次 → 仍失败则进入重试队列。
- **标签**：3–6 个；去重、不与合集名冲突；规范化（NFKC、trim、长度 ≤20）；用户编辑后 `tags_user_edited=1`，后续重试不覆盖；用户删除的自动标签写入 `tag_suppressions`。
- **摘要**：可编辑；用户编辑后 `summary_user_edited=1`，重试不覆盖。
- **文本素材**：走 `TextProvider` 摘要（云端）或本地抽取式摘要（首段 + 标题）；无云端时 `summary_source=extractive`，标「离线摘要」。
- **图片素材（无 VLM）**：用 OCR 摘录生成占位摘要（`summary_source=ocr`），卡片标「OCR 摘要」；不生成标签与视觉属性，不冒充 AI 描述。
- **公式（P1）**：VLM 输出 LaTeX 存 `insights.latex`；KaTeX 渲染；可复制源码。
- **表格结构化（P2）**：输出 Markdown/CSV 存 `insight` 扩展字段。

### 7.5 两阶段索引

为满足「确认提交到可检索 ≤3s」：

```
提交
  → 缩略图（本地）
  → 本地 OCR
  → 用 OCR 文本做临时嵌入 → 写 FTS/向量（此时已可检索，status=parsing）
  → 云端 VLM（或文本摘要）
  → 更新 insight/标签/视觉属性
  → 用「摘要+标签+实体+OCR」重建嵌入与 FTS（status=ready）
```

- VLM 失败/超时/未配置：保留临时索引，用 OCR 摘录生成占位摘要（`summary_source=ocr`），`status=pending`，卡片标「OCR 摘要」；恢复后自动重试并替换。
- OCR 无文字：无摘要，卡片标题回退为来源窗口标题/捕获时间，标「待解析」。
- 用户删除素材：取消该素材所有排队/运行任务；已完成部分随事务清理。
- 用户编辑摘要/标签/备注：只触发 `reindex`（FTS + 嵌入），不调用 VLM。

### 7.6 任务队列

**状态机：** `queued → running → succeeded | failed | cancelled`；失败按 2/5/15s 退避，最多 3 次，然后转 `pending`（UI 可手动重试）。

- SQLite 持久化；内存队列 + `tokio::sync::Notify` 唤醒；重试按 `next_run_at` 定时。
- 2 路工作线程（可配置）；新捕获优先，重试次之，重建索引最后。
- 任务幂等；唯一键 `(item_id, stage, revision)`；旧 revision 自动取消。
- 启动恢复：`running → queued`；清理临时文件；托盘/主窗口提示「已恢复 N 项待解析」。
- 删除/清空库：事务取消任务；在途 HTTP 通过取消令牌中断。

### 7.7 离线与降级

| 场景 | 行为 |
|---|---|
| 未配置 Key / Key 失效 | 捕获 + OCR + 临时嵌入可用；OCR 摘录占位（标「OCR 摘要」）；VLM 任务排队；顶部提示配置 |
| 无网络 | 本地 PP-OCR/嵌入/检索可用；Windows OCR 兜底；云端任务暂停；OCR 摘录占位；状态栏「离线模式」 |
| VLM 超时/5xx | 退避重试；仍失败 → OCR 摘录 + pending；详情页可手动重试 |
| 本地 VLM 未实现 | 不提供本地描述；按 OCR 摘录降级；UI 不宣称「完全离线描述」 |
| 嵌入模型未加载 | 检索退化为 FTS + 过滤；后台加载完成后自动恢复 |
| 主 Provider 失败 | 切换备用 Provider；都失败则排队 |
| 网络恢复 | 探测成功 → 恢复队列；不轮询云端（仅在有排队任务时探测） |

### 7.8 成本控制

- 每张图一次 VLM 调用；输入图降采样；目标单用户月成本 ≤¥15（按约 300 次/月估算，需按实际厂商价格复核）。
- 可选优化：相同 `content_hash` 的图片复用已生成的 insight（素材仍各自入库，不改变「不去重」的产品行为），减少重复调用。
- 请求并发上限 2；失败重试不计费的部分优先；记录每次调用的 token/耗时（本地）。
- 无遥测上传；成本数据仅本地可见。

### 7.9 提示词与结构化输出

- 提示词按「产品字段定义 + 枚举 + 反例 + 输出 schema」组织，版本化存放（`crates/ai/prompts/*.md`），修改需跑固定样本回归。
- 摘要要求：客观描述画面，不推断画面外信息；标签覆盖主题/场景/形态；颜色必须从枚举中选。
- 禁止输出用户未可见的结论；命中解释不依赖 VLM 自由文本。

---

### 7.10 本地模型模式与 OCR 降级（当前决定）

**决定：** 暂不实现本地 VLM，也不承诺 F6.5 的「本地描述」。当前描述来源只有两种：云端 VLM（已配置且在线）与 OCR 摘录（降级）。

| 场景 | 描述来源 | 状态与标注 | 检索影响 |
|---|---|---|---|
| 云端正常 | 云端 VLM | 正常 | 摘要/标签/视觉属性全可用 |
| 离线 / 无 Key / VLM 失败 | OCR 摘录（`summary_source=ocr`） | 卡片标「OCR 摘要」，状态 pending | OCR/FTS/向量可用；颜色/图表/视觉属性不可用 |
| OCR 也无文字 | 无摘要 | 标题回退为来源窗口标题/时间，标「待解析」 | 仅来源/时间/备注等元数据可搜 |

- **OCR 摘录规则**：取 OCR 文本中第一个非空、长度 ≥6 的行（跳过纯数字/单字 UI 标签），压缩空白后截断 60–100 字；没有合适行则取前 100 字。
- **不做假标签**：OCR 降级不生成自动标签、颜色/图表/界面类型；用户可手动打标。
- **升级路径**：VLM 配置恢复后重跑 `vision` 任务，成功则替换为 VLM 摘要并补齐标签/视觉属性；`summary_source=user`（用户编辑过）不覆盖。
- **验收口径**：这**不是** F6.5 的「本地模型模式」。PRD 的「描述与嵌入均在本地完成」应回写为「本地 OCR/嵌入 + OCR 摘录降级；本地 VLM 后续单独立项」，否则无法按原文验收。
- **本地 VLM 立项条件**：具备 8GB+ 显存测试机、确定模型/量化/延迟目标后再启动；接口保留，不进入当前排期。

## 8. IPC 与前端

### 8.1 命令与事件

**核心命令（示意）：**

```text
capture_start() / capture_cancel()
search_items(req) -> SearchResponse
list_items(filter, cursor, limit) -> Page<ItemCard>
get_item(id) -> ItemDetail
update_note(id, note) / update_tags(id, tags) / star_item(id, on)
list_tags() / create_tag() / rename_tag() / delete_tag()
list_collections() / create_collection() / add_items_to_collection(ids, collection_id) / remove_items_from_collection(ids, collection_id)
export_item(id, dir) / export_collection(id, dir)
copy_item_content(id) / copy_summary(id)
retry_item(id) / delete_item(id) / delete_all()
get_settings() / update_settings(patch) / set_provider_key(id, key) / test_provider(id)
get_storage_usage() / open_library_folder() / change_library_location(path)
get_data_flow_info() / get_metrics_summary()
```

**事件：**

```text
item_created { id }
item_updated { id, fields[] }
job_progress { item_id, stage, state }
capture_completed { item_id, thumb_url }
network_status { online, mode }
provider_error { provider_id, code, message }
storage_warning { free_bytes }
```

- 事件只带最小信息；UI 用 TanStack Query 失效/重取。
- 搜索结果用 top-K（默认 100）+ 游标；列表用 `(captured_at, id)` keyset 分页。
- 所有命令返回结构化错误码（`CONFIG_MISSING`、`NETWORK`、`PROVIDER_AUTH`、`VISION_UNSUPPORTED`、`STORAGE_FULL`、`NOT_FOUND`、`INVALID_INPUT`），前端映射文案。
- `test_provider` 对视觉 Provider 发送小图做能力探测，不能只测文本；文本 Provider 单独测文本连通。

### 8.2 类型生成

- `tauri-specta` 从 Rust 命令与 DTO 生成 TypeScript 客户端与类型；前端只使用生成物，不手写接口。
- 生成物纳入版本控制；CI 检查生成物与 Rust 定义一致（重新生成后 diff 为空）。

### 8.3 资源协议

- `visrecall://thumb/<item_id>`：返回缩略图（WebP），`Cache-Control: immutable`。
- `visrecall://original/<item_id>?max=2048`：详情预览；`max` 只允许白名单值。
- **不暴露任意文件路径**；协议处理器按 item id 查 DB，拒绝越界/不存在；防止路径穿越。
- 导出/打开文件夹使用系统 shell，路径先 canonicalize，且必须在用户选择的库/导出目录内。

### 8.4 React 目录

```text
src/
  app/            # 路由、窗口壳、错误边界
  features/
    search/       # SearchBar, QueryChips, ResultGrid, ResultCard, ExplainBar
    detail/       # DetailPanel, SummaryEditor, TagEditor, NoteEditor, Actions
    inbox/        # InboxList, WeeklyReview
    collections/  # Sidebar, CollectionTree, DragAssign
    settings/     # Capture, Models, Storage, Privacy, About
    onboarding/   # 三种捕获方式教学
  components/ui/  # shadcn/ui 组件
  lib/            # ipc 生成物、queryClient、events、format
  stores/         # UI 状态（选中项、布局、搜索草稿）
  styles/         # Tailwind、主题变量
```

- **服务端状态**：TanStack Query（items/search/settings）；**UI 状态**：Zustand（选中、面板宽度、快捷键提示）。
- 搜索防抖 250ms；请求带序号，丢弃过期响应；chip 删除立即重算。
- 编辑（摘要/备注/标签）乐观更新 + 300ms 防抖保存 + 失焦 flush；失败回滚并提示。
- 结果网格虚拟化；图片懒加载 + 骨架屏；详情面板按需加载原图。
- 键盘：`Ctrl+K` 聚焦搜索、↑↓ 选择、Enter 打开、Esc 返回、E 导出（V0.5）；全流程键盘可达。
- 无障碍：对比度 WCAG AA、焦点可见、语义化标签、屏幕阅读器可用。
- 国际化：zh-CN 默认，文案外置；en P1。

### 8.5 窗口与标题栏

- V0.1 可使用原生标题栏，优先保证 Windows 11 Snap Layouts 与稳定性；V0.5 再评估自定义标题栏（需自行实现拖拽区、缩放边框与 Snap）。
- 窗口大小/位置持久化；多显示器时校正到可见区域。
- 关闭主窗口 → 托盘常驻；设置窗口独立同策略。

### 8.6 原始内容复制（F5.3，P0）

- 图片：写入 `CF_DIB`（BGRA）并注册 `PNG` 格式，兼容 Word/微信/浏览器；主按钮文案「复制原图」。
- 文本：`CF_UNICODETEXT`，保留换行（Windows 剪贴板使用 CRLF），不附加摘要/标签/来源。
- 链接：`CF_UNICODETEXT` 原始 URL。
- 摘要独立复制：仅文本；摘要缺失或待解析时禁用。
- 复制成功由前端轻提示；失败返回结构化错误码，不静默。

---

## 9. 非功能指标与验证

| 指标 | 目标 | 验证方法 |
|---|---|---|
| 托盘常驻内存 | ≤150MB（目标 40–70MB） | 主窗口关闭 60s 后，Process Explorer 私有工作集；确认无 WebView2 进程 |
| 主窗口内存 | 160–280MB（含嵌入模型，估算） | 打开搜索/详情后测量；不作为硬指标 |
| 4K 双屏抓帧峰值 | 进程工作集增量 ≤100MB | ETW/WPR + 抓帧前后快照；VRAM 单独记录 |
| 捕获响应 | ≤150ms | 快捷键时间戳 → 覆盖层首帧 Present；ETW/PresentMon |
| 选区交互 | 高亮跟随 ≤50ms，60fps | PresentMon 帧时间；4K 多窗口场景 |
| 入库可检索 | ≤3s（P95） | 提交时间 → 首次 OCR 索引完成时间 |
| OCR 准确率 | 清晰截图字符准确率 ≥97% | 100–200 张真实截图；PP-OCR 主路径统计，Windows OCR 兜底单独统计 |
| 检索延迟 | ≤800ms（P95，素材 ≤100 条） | 50 条真实查询 + 分阶段埋点；不使用万级合成库 |
| 空闲 CPU | <1% | 托盘空闲 10 分钟，PerfMon |
| 冷启动到托盘 | ≤2s（机械盘 4s） | 进程启动 → 托盘创建成功；不计主窗口打开耗时 |
| 容量 | 暂不作为 V0.1 验收项 | 上线后观察真实数据量；不合成 5 万/10 万条库 |
| 可靠性 | 进程强杀不丢已入库素材；任务可恢复 | 解析中 `taskkill /F`，重启核对 |
| 兼容性 | Win10 22H2 / Win11；x64/ARM64；100/125/150/200% | 兼容矩阵 + 实机/虚拟机 |
| 可访问性 | 全流程键盘可达；WCAG AA | 手动键盘走查 + 对比度检查 |

**基准设施：**

- `tests/bench` 生成 ≤100 条合成素材（元数据 + OCR 文本 + 向量），不依赖真实图片；`criterion` 测解析、FTS、向量、融合的单阶段耗时，不做万级容量基准。
- 内存测量脚本：启动 → 空闲 → 捕获 → 解析 → 检索，分阶段记录工作集与句柄数。
- GPU/覆盖层性能用 PresentMon/ETW；不把「肉眼流畅」当证据。

---

## 10. 测试策略

### 10.1 分层

| 层 | 内容 |
|---|---|
| 单元测试 | 几何（缩放/吸附/控制点/DPI 换算，含 `proptest`）、查询解析、RRF、命中解释、任务状态机、保留策略、文件名清洗、路径安全 |
| 集成测试 | 临时库 + 固定图片/文本；PP-OCR/Windows OCR 双引擎样本集；VLM 用 `wiremock` 录制夹具；OCR 摘录降级与恢复替换；导出/迁移往返；删除清理；崩溃恢复 |
| 契约测试 | DeepSeek 视觉/文本与备用 Provider 的录制夹具；适配器响应解析；JSON schema 校验；错误码映射 |
| 前端测试 | Vitest + React Testing Library；搜索/详情/设置关键交互；IPC 用 mock |
| E2E | `tauri-driver` + WebdriverIO 驱动主窗口；测试库由 CLI 预置；云端 Provider 指向本地 mock |
| 手动矩阵 | 多屏/混合 DPI、任务栏自动隐藏、全屏、DRM、混合显卡、OLE 拖放（Chrome/Word/资源管理器/微信）、输入法 |

### 10.2 检索评测

- 50 条真实查询 + 标注，素材库控制在 100 条以内；Top-3 命中率 ≥80% 为 V0.1 退出条件。
- 变更 FTS 分词、嵌入模型、RRF 权重、解析规则时必跑；结果存档对比。
- 记录「不是这个」反馈，逐步扩充评测集。
- 评测只关注排序质量与可用性；素材库固定在 100 条以内，不做万级延迟/容量压测。

### 10.3 兼容性矩阵

- 系统：Win10 22H2 x64、Win11 x64、Win11 ARM64。
- DPI：100/125/150/200%；单屏/双屏/混合缩放；主副屏互换。
- 场景：任务栏自动隐藏、全屏视频、独占全屏游戏、远程桌面、DRM 窗口、无边框应用。
- 输入：中文输入法、键盘全流程、拖放四种来源。

### 10.4 CI 与打包

- GitHub Actions（Windows x64）：`cargo fmt/clippy/test`、`pnpm lint/typecheck/test`、构建、NSIS/MSI 产物。
- ARM64：优先原生 ARM64 runner；否则交叉编译 + 本地 ARM64 实机 smoke test。
- 生成物一致性检查：`tauri-specta` 重新生成后 diff 必须为空。
- 性能基准 nightly 运行，PR 只跑单元/集成；检索评测在排序相关改动时手动/定时跑。
- 安装包内置 WebView2 Evergreen Bootstrapper，离线安装不依赖用户预装。
- 签名与自动更新不在 V0.1；安装包需在 README 说明 SmartScreen 提示。

### 10.5 分发形态与前置条件

**支持的分发形态：** 单机桌面应用。Tauri NSIS per-user + WebView2 Evergreen Bootstrapper + 随包或校验下载模型；无服务端、无账号；数据全部在用户本机。

**分发前必须补齐：**

| 项 | 说明 |
|---|---|
| x64 / ARM64 安装包 | 分别构建与 smoke test；ARM64 依赖（ONNX Runtime/WebView2/sqlite-vec）先验证 |
| 代码签名 | 未签名会被 SmartScreen/杀软拦截；EV/OV 证书按预算选择 |
| WebView2 | 安装包内置 bootstrapper；离线环境可用 |
| 模型分发 | 随包或 CDN 下载 + 校验和；安装包体积与首次启动体验权衡 |
| API Key | BYOK 引导与连通性测试；绝不把共享 Key 打进客户端 |
| 诊断 | 默认关闭的可选崩溃/错误上报；隐私说明与数据流向页 |
| 更新 | V1.0 后再做签名更新通道；需静态更新端点与回滚策略 |
| 兼容性 | Win10 22H2 / Win11、x64/ARM64、多屏 DPI、杀软误报处理 |
| 许可证 | 模型、ONNX Runtime、字体等第三方声明 |

**不支持的形态：** 多用户/SaaS/团队协作。当前架构没有账号、鉴权、同步、计费与服务端；若要做，建议「客户端 SQLite + 云端 PostgreSQL + 同步服务」的独立架构，而不是把 PostgreSQL 放到本地。

---

## 11. 里程碑

> PRD 的「V0.1 六周」按团队估算；单人 + Rust + 原生 Windows 集成，实际需要约 3 倍。以下是务实排期。

### 11.1 技术验证（第 1–2 周，必须通过）

| 验证项 | 通过标准 | 不通过时 |
|---|---|---|
| 多屏/混合 DPI 冻结帧与选区 | 150% 缩放下选区与裁剪无偏移；4K 双屏抓帧响应 ≤150ms | 调整覆盖层窗口策略；必要时降低覆盖层渲染分辨率或启用 DXGI 快路径 |
| OLE 拖放 | Chrome 选中文字、Word 文本、资源管理器图片、微信图片均可拖入并正确判定 | 改为原生拖放辅助窗口/缩短支持范围 |
| DeepSeek 视觉 Provider | 小图请求成功、JSON 可解析、失败可切备用 | 改用第三方 DeepSeek-VL2 托管或其他视觉厂商 |
| PP-OCR 主路径 | 100–200 张样本字符准确率 ≥97%；P95 解析延迟与解析峰值内存达标；Windows OCR 兜底可用 | 调模型/预处理；仍不达标则重新评审主备 |
| WebView2 内存 | 主窗口关闭 60s 后进程完全退出；托盘常驻 ≤150MB | 立即销毁（去掉 60s 缓冲）；或退回 .NET 方案 |
| SQLite FTS + 向量 | ≤100 条合成素材检索可用、P95 ≤800ms；不要求万级压测 | 调整分词/索引结构；容量优化上线后评估 |
| 覆盖层 60fps | 4K 下单屏拖动控制点稳定 60fps | 降渲染分辨率；优化重绘区域 |

任何一项不通过，先调整方案并重新验证，不进入业务开发。

### 11.2 V0.1（第 3–16 周）

| 阶段 | 内容 |
|---|---|
| W3–6 | 捕获闭环：快捷键、抓帧、窗口识别、覆盖层、选区、裁剪落盘、入库、素材列表 |
| W7–9 | 存储/详情：SQLite、缩略图、详情面板、备注、标签编辑、原始内容复制、删除、崩溃恢复 |
| W10–13 | AI 与检索：OCR、嵌入、VLM Provider、两阶段索引、查询解析、FTS+向量+RRF、chip、命中解释 |
| W14–16 | 打磨与交付：多屏/DPI 矩阵、内存优化、离线降级、数据流向页、50 条查询评测、打包安装、V0.1 验收 |

**V0.1 退出条件：** 50 条真实查询 Top-3 命中率 ≥80%；自用 14 天日均捕获 ≥8 次。

**V0.1 范围：** F1.1–F1.5、F1.8、F2.1–F2.4、F3.1–F3.4、F4.3–F4.4、F5.3、F6.1–F6.4、F7.1–F7.3。
**明确不做：** 备注浮层（F1.6）、剪贴板（F1.7）、精确搜索（F3.5）、搜索历史（F3.7）、收件箱回顾与合集（F4.1–F4.2）、时间线/批量/星标（F4.5–F4.7）、导出（F5.1–F5.2）、公式/本地模型/迁移（F2.5、F6.5、F7.4–F7.7；F6.5 推迟）。

### 11.3 V0.5（第 17–24 周）

合集、收件箱回顾、标签管理、筛选器、时间线、批量操作、捕获后备注浮层、剪贴板、搜索历史、单条/合集导出。

### 11.4 V1.0（第 25–36 周）

公式转 LaTeX、存储管理、快捷键自定义、完整数据导出、迁移包、性能与可靠性收尾。本地模型模式（F6.5）不在 V1.0 承诺范围；待具备本地 VLM 测试条件后单独立项。

### 11.5 范围裁剪顺序（进度告急时）

1. 自定义标题栏 → 用原生标题栏。
2. 时间线视图、星标、相似推荐（本就 P1/P2）。
3. 跨屏选区 → 先支持同 DPI 多屏。
4. 覆盖层动画与视觉打磨 → 保留功能，降低动效。
5. **不裁剪：** 冻结帧语义、OLE 文本拖放、命中解释、离线 OCR/嵌入、数据可导出。

---

## 12. 风险登记

| # | 风险 | 概率/影响 | 缓解 | 触发信号 |
|---|---|---|---|---|
| R1 | 原生覆盖层 + D3D11 复杂度超预期 | 高/高 | 2 周 spike；`OverlayRenderer` 抽象；GDI+CPU 代理兜底 | spike 未在 2 周内跑通 4K 60fps |
| R2 | OLE 拖放兼容性（浏览器/Office/微信） | 高/中 | 早期实测四类来源；保留原生辅助落点窗口方案 | 任一高频来源无法取到文本/图片 |
| R3 | WebView2 内存回收不彻底 | 中/高 | 关闭即销毁；实测进程退出；必要时立即销毁 | 60s 后仍有 WebView2 进程 |
| R4 | 中文 FTS 召回质量 | 中/中 | 50 条评测集；jieba vs trigram A/B；可加同义词 | Top-3 命中率 <80% |
| R5 | 嵌入模型切换/兼容 | 中/中 | 模型 ID 固定于库；新表重建后原子切换 | 检索结果明显退化 |
| R6 | VLM JSON 不稳定 | 中/中 | 严格 schema + 修复 + 重试 + 备用 Provider | 解析失败率 >3% |
| R7 | 云端成本/限流 | 中/中 | 降采样、并发 2、哈希缓存、主备切换 | 单用户月成本 >¥15 |
| R8 | Win32/COM 在 Rust 中的未定义行为 | 中/高 | 隔离 unsafe；几何纯函数化；回调 `catch_unwind`；代码审查 | 偶发崩溃/句柄泄漏 |
| R9 | 多屏混合 DPI 坐标错误 | 中/高 | 统一物理像素坐标；`proptest`；实机矩阵 | 150%/200% 下偏移 |
| R10 | 图片导入超大像素内存峰值 | 中/中 | WIC 缩放解码；像素上限；逐张处理 | 导入 20MB 图时内存飙升 |
| R11 | 库迁移中断/数据丢失 | 低/高 | 复制 + 校验 + 原子切换；源保留；可续传 | 校验失败/中断 |
| R12 | ARM64 依赖不可用 | 中/中 | 早期验证 ONNX/WebView2/SQLite ARM64；单独产物 | 依赖无 ARM64 二进制 |
| R13 | 范围蔓延 | 高/中 | V0.1 功能冻结；P1/P2 明确延后 | 新增非验收项功能 |
| R14 | Windows 10 22H2 已停止支持 | 中/低 | 按 PRD 支持，但文档标注安全风险；优先 Win11 测试 | 用户反馈 Win10 特定问题 |
| R15 | 本地模型模式无测试硬件、无法按期实现 | 高/中 | 明确定义为 OCR 摘录降级；PRD 回写；不进当前排期 | 验收要求「完全离线描述」 |
| R16 | DeepSeek 官方 API 不支持图像输入/模型名不兼容 | 中/高 | 视觉能力探测；第三方托管 DeepSeek-VL2 或 Qwen-VL/GLM-4V 备用；配置层可切换 | 视觉请求返回 400/不支持 image_url |
| R17 | PP-OCR 在低端 CPU 上延迟/内存超标 | 中/中 | mobile 模型、限制线程数、懒加载/空闲释放；Windows OCR 兜底；必要时降低检测分辨率 | P95 解析 >3s 或解析峰值 >200MB |
| R18 | 真实数据增长后检索变慢（V0.1 仅验证 ≤100 条） | 低/中 | 保留 top-K/分页与 `VectorIndex` 抽象；上线后监控真实库延迟，超阈值再引入量化/HNSW | 真实库明显超出 100 条测试规模（如数千条）且检索变慢 |

---

## 13. 待决问题

| # | 问题 | 建议 / 影响 | 决策人 |
|---|---|---|---|
| Q1 | 默认 VLM 厂商 | 测试期定为 DeepSeek 视觉 endpoint（官方或第三方托管 DeepSeek-VL2）；发布前按质量/价格复核，保留 Qwen-VL/GLM-4V 备用 | 产品 + 技术 |
| Q2 | 嵌入模型随包分发还是首次下载 | 建议随包（30–60MB），保证离线首次可用；影响安装包体积 | 技术 |
| Q3 | 「4K 双屏抓帧 ≤100MB」是总工作集还是增量 | 架构按增量 ≤100MB 设计，同时尽量压低总量；需产品确认口径 | 产品 + 技术 |
| Q4 | 是否保留 60s 延迟销毁主窗口 | 先用 60s；若内存 NFR 实测超标则改立即销毁 | 技术 |
| Q5 | 自定义标题栏 vs 原生标题栏 | V0.1 原生（保留 Snap）；V0.5 按视觉需要再评估 | 设计 + 技术 |
| Q6 | 本地度量数据是否允许可选上传 | V0.1 只本地；若需要北极星指标，先做本地汇总 + 用户主动导出 | 产品 + 商业 |
| Q7 | 账号/订阅（PRD Q3） | 不在本架构范围；当前设计无服务端依赖 | 商业 |
| Q8 | 开源（PRD Q6） | 影响模型/字体/依赖许可证与分发；V1.0 前定 | 创始团队 |
| Q9 | ARM64 实机测试资源 | 需要 Win11 ARM64 设备/VM；否则只能交叉编译 + 有限 smoke test | 技术 |
| Q10 | F6.5 是否回写为「OCR 摘录降级」 | 当前决定不实现本地 VLM；需产品同步 PRD，否则 V1.0 验收口径冲突 | 产品 + 技术 |
| Q11 | PP-OCR 具体版本与量化（v5 mobile vs v4 mobile） | 优先 v5 mobile；不可用/不达标再回退 v4；以样本集准确率与延迟为准 | 技术 |

---

## 附录 A：库目录示例

```
VisRecall/
├── library.json
├── visrecall.db
├── visrecall.db-wal
├── originals/
│   └── images/
│       └── 2026/11/<item-id>.png
├── texts/
│   └── <item-id>.txt
├── thumbs/
│   └── <item-id>.webp
├── backups/
│   └── visrecall.db.bak-3
└── tmp/
```

## 附录 B：Insight JSON Schema（摘要）

```json
{
  "type": "object",
  "required": ["summary", "content_type", "primary_color", "chart_type", "entities", "tags", "has_formula", "confidence"],
  "properties": {
    "summary": { "type": "string", "maxLength": 120 },
    "content_type": { "enum": ["screenshot", "photo", "diagram", "document", "whiteboard", "ui", "other"] },
    "primary_color": { "enum": ["orange", "red", "blue", "green", "yellow", "purple", "pink", "brown", "black", "white", "gray", "cyan"] },
    "chart_type": { "enum": ["line", "bar", "pie", "scatter", "funnel", "table", "none"] },
    "ui_type": { "enum": ["pricing", "dashboard", "login", "form", "article", "other", "none"] },
    "entities": { "type": "array", "items": { "type": "string" }, "maxItems": 8 },
    "layout": { "type": "string" },
    "has_formula": { "type": "boolean" },
    "tags": { "type": "array", "items": { "type": "string" }, "minItems": 3, "maxItems": 6 },
    "confidence": { "type": "number", "minimum": 0, "maximum": 1 }
  }
}
```

## 附录 C：配置示例

```json
{
  "library_path": "C:\\Users\\Tomwu\\Documents\\VisRecall",
  "startup_mode": "silent",
  "capture": {
    "enabled": false,
    "realtime_fallback": true
  },
  "hotkeys": {
    "capture": "Ctrl+Shift+S",
    "open_main": "Ctrl+Shift+Space"
  },
  "ball": {
    "enabled": true,
    "size": 48,
    "auto_collapse": true,
    "hide_fullscreen": true
  },
  "retention": {
    "mode": "forever",
    "days": null,
    "exempt_starred": true,
    "exempt_exported": true
  },
  "embedding": {
    "provider": "local",
    "model_id": "bge-small-zh-v1.5-int8",
    "dim": 512
  },
  "ocr": {
    "engine": "auto",
    "primary": "ppocr",
    "fallback": "windows",
    "model": "pp-ocrv5-mobile",
    "confidence_threshold": 0.6
  },
  "providers": {
    "vision": [
      { "id": "deepseek-vl", "base_url": "https://api.deepseek.com/v1", "model": "deepseek-vl2", "credential_key": "VisRecall/deepseek-vl", "capabilities": ["vision"] },
      { "id": "siliconflow-qwen-vl", "base_url": "https://api.siliconflow.cn/v1", "model": "Qwen/Qwen2.5-VL-32B-Instruct", "credential_key": "VisRecall/siliconflow-qwen-vl", "capabilities": ["vision"] }
    ],
    "text": [
      { "id": "deepseek-chat", "base_url": "https://api.deepseek.com/v1", "model": "deepseek-chat", "credential_key": "VisRecall/deepseek-chat", "capabilities": ["text"] },
      { "id": "siliconflow-qwen", "base_url": "https://api.siliconflow.cn/v1", "model": "Qwen/Qwen2.5-7B-Instruct", "credential_key": "VisRecall/siliconflow-qwen", "capabilities": ["text"] }
    ]
  },
  "privacy": {
    "clipboard_capture": false,
    "telemetry": false
  }
}
```

> DeepSeek 官方 API 的视觉模型可用性与模型名以当前账号文档为准；若官方 endpoint 不接受图像，把 `base_url` 指向第三方 DeepSeek-VL2 托管服务，或使用备用视觉 Provider。配置保存前必须通过「小图视觉能力探测」。

## 附录 D：术语

| 术语 | 含义 |
|---|---|
| 冻结帧 | 触发截屏瞬间抓取、在选区过程中保持不变的屏幕画面 |
| 覆盖层 | 覆盖在每个显示器上的置顶窗口，负责显示冻结帧与选区交互 |
| 两阶段索引 | OCR 后先建立可检索索引，VLM 返回后再增强并重建 |
| RRF | Reciprocal Rank Fusion，多路召回结果的排名融合 |
| 工作集 | 进程当前驻留在物理内存中的页面集合；本文件中的内存指标口径 |
| VRAM | 显存；冻结帧纹理与覆盖层交换链所在位置 |

---

*本文件为技术架构设计，不包含实现代码；实现顺序与任务拆分见 `docs/specs/v0.1/README.md` 及各阶段三件套。*
