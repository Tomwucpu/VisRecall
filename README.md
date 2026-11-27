<p align="center">
  <img src="src-tauri/icons/128x128@2x.png" width="112" height="112" alt="VisRecall" />
</p>

<h1 align="center">VisRecall</h1>

<p align="center">
  <b>更轻、更顺手的本地素材库。截图、图片与文本，一句话找回。</b>
</p>
<p align="center">
  <img alt="License" src="https://img.shields.io/badge/license-Apache--2.0-3b82f6" />
  <img alt="Rust" src="https://img.shields.io/badge/rust-stable%20%C2%B7%202021-b7410e" />
  <img alt="Tauri" src="https://img.shields.io/badge/tauri-2-24c8db" />
  <img alt="Platforms" src="https://img.shields.io/badge/platforms-Windows%2010%20%7C%2011%20x64-6b7280" />
</p>

<p align="center">
  <a href="https://github.com/Tomwucpu/VisRecall/releases/latest">
    <img src=".github/assets/download-button.svg" width="288" alt="下载 VisRecall" />
  </a>
</p>

OCR 识字、语义向量化、全文检索与素材存储默认均在本机离线完成；配置并开启云端模型后，可进一步为图片生成视觉描述、摘要与自动标签。

## 功能特性

- **素材收集**：全局热键或悬浮球触发多屏冻结框选截图；向悬浮球拖放图片、文本/富文本片段或链接直接入库；主界面「＋ 添加」支持批量导入文件、新建文本及只读目录引用导入。
- **本地与云端两阶段理解**：图片入库先经本地 PP-OCR（Windows OCR 兜底）取字并建立本地语义向量，秒级可搜；开启云端解析后，再由大模型补充画面描述、结构化视觉属性（颜色/图表/界面类型）与自动标签。
- **中文混合检索**：SQLite FTS5 全文检索、本地 BGE 语义向量检索与视觉/类型属性软命中三路 RRF 融合排序，附带命中解释与 OCR 文本框定位；支持时间、`app:应用`、`#标签`、引号精确匹配及多维筛选栏。
- **素材整理**：详情面板支持查看原图、复制原图/富文本/纯文本、重命名标题、编辑备注/AI 描述/OCR 文本/库内正文、增删改名标签与单条重新解析；列表支持多选批量删除。
- **AI 与隐私可控**：本地 PP-OCR 与 BGE 模型随包提供；大模型在「设置 → 模型」按需添加（支持 OpenAI 兼容、Anthropic、Ollama、自定义 REST 四种协议），API 密钥仅存 Windows 凭据管理器，界面不回显；图片与文本「上传云端解析」为两个独立开关且默认关闭；素材库明文存储可整体迁移，应用默认常驻系统托盘（可选启动时打开主窗口）。

## 安装

1. 在 [GitHub Releases](https://github.com/Tomwucpu/VisRecall/releases) 按目标机器网络环境下载安装包（两者均内置本地 OCR / 嵌入模型与 ONNX Runtime）：

   | 安装包 | 体积 | 选择场景 |
   | --- | --- | --- |
   | `VisRecall_<版本>_x64-setup.exe` | 约 42 MB | 机器可联网。缺少 WebView2 时安装程序会自动联网安装；Windows 11 与 Windows 10（1803+）通常已自带。 |
   | `VisRecall_<版本>_x64-setup-offline.exe` | 约 330 MB | 机器离线或受限联网。内置 WebView2 Evergreen 独立安装器，无需联网。 |

2. 下载同版本的 `SHA256SUMS.txt`，在 PowerShell 校验安装包哈希，例如 `Get-FileHash -Algorithm SHA256 -LiteralPath .\VisRecall_0.1.0_x64-setup.exe`。
3. 准备本地推理所需的 [Microsoft Visual C++ v14 Redistributable（x64）](https://learn.microsoft.com/cpp/windows/latest-supported-vc-redist)。
4. 运行安装包，先选「安装位置」（存放程序与随包模型），首次安装再选「素材库位置」（默认 `%USERPROFILE%\Documents\VisRecall`，两处目录不可重叠）。
5. 启动 VisRecall：默认静默驻留系统托盘，左键点击托盘图标打开主窗口（可在「设置 → 通用」改为启动时显示主窗口）。

首发候选包面向 Windows 11 x64，具体兼容范围与代码签名/SmartScreen 说明以对应 Release 页面为准。

## 使用

- **启用截图**：截图功能默认关闭。前往「设置 → 捕获」开启「启用截图」后，即可使用全局快捷键（默认 `Ctrl+Shift+S`）或单击悬浮球截屏；未开启截图时悬浮球仍支持拖放入库。
- **托管与引用导入**：截图、拖放与文件导入的素材复制到素材库目录内托管；通过「导入文件夹」关联的外部目录以只读引用方式索引，原始文件保留在原路径（可在「设置 → 导入目录」同步或重新定位）。
- **云端视觉与文本增强**：若需按画面内容（无文字图片、颜色、图表形态）检索，先在「设置 → 模型」添加并激活视觉模型，再开启同页的「图片上传云端解析」（文本摘要对应「文本上传云端解析」）。关闭或未配置时不会发起任何网络请求，素材仅使用本地 OCR / 原文建立全文与向量索引。
- **驻留与卸载**：关闭主窗口后应用继续驻留托盘（空闲 60 秒自动释放 WebView2 内存），右键托盘选择「退出」彻底关闭进程。覆盖升级与卸载均保留素材库和应用配置。

## 开发构建

### 技术栈

- **桌面壳**：Tauri 2（Rust + WebView2）+ 原生 Win32 / D3D11 / Direct2D 覆盖层与悬浮球，仅支持 Windows
- **前端**：React 19 + TypeScript 5.8 + Vite 7 + TanStack Query 5，无路由、无第三方 UI 组件库，手写水墨令牌 CSS
- **存储**：SQLite（WAL、bundled FTS5、int8 向量表）+ 库目录明文文件
- **AI**：本地 PP-OCRv4 + Windows OCR 兜底、本地 `bge-small-zh-v1.5-int8`（ONNX Runtime 动态加载）；可选云端模型（`openai` / `anthropic` / `ollama` / `rest` 协议）

### 环境要求

| 依赖 | 说明 |
| --- | --- |
| Visual Studio C++ Build Tools + Windows SDK | 提供 MSVC 工具链 |
| Rust | 按 [`rust-toolchain.toml`](rust-toolchain.toml) 自动匹配 `stable` + `x86_64-pc-windows-msvc` |
| Node.js / pnpm | 发布验证版本 Node `24.19.0`、pnpm `10.30.3` |
| WebView2 运行时 | Windows 11 与 Windows 10（1803+）通常已自带 |

`models/` 中的 PP-OCRv4、BGE int8 模型与 `onnxruntime.dll` 已随仓库提供，无需额外下载。

### 常用命令

| 命令 | 作用 |
| --- | --- |
| `pnpm install` | 安装前端依赖 |
| `pnpm tauri dev` | 启动完整桌面应用（推荐；debug 启动会自动刷新 `src/lib/bindings.ts`） |
| `pnpm dev` | 仅启动 Vite（无 Tauri IPC，仅用于纯样式调试） |
| `pnpm typecheck` / `pnpm lint` / `pnpm test` | 前端类型检查 / ESLint / Vitest 单元测试 |
| `pnpm build` | 前端类型检查 + 生产构建（产出 `dist/`） |
| `cargo test --workspace` | Rust 全工作区单元与集成测试 |
| `cargo test -p visrecall-ai -- --ignored` | 依赖 `models/` 真实模型文件的 AI 集成测试（发版前必跑） |
| `cargo clippy --workspace --all-targets` | Rust 静态检查 |
| `cargo fmt --all` | Rust 代码格式化 |
| `powershell -File scripts/models-check.ps1 -Dir models` | 校验 `models/` 文件与 `models/manifest.json` 哈希一致 |
| `powershell -File scripts/ocr-accuracy.ps1 -Ocr <目录> -Annotations <目录>` | 对 `ocr_spike` 产出的 `*.ocr.txt` 与人工标注计算字符准确率 |
| `cargo run -p visrecall-eval -- --db <库目录> --queries <jsonl> --report <输出.md>` | 运行混合检索评测 CLI 并输出分桶指标报告 |
| `pnpm release:build` | 构建轻量版、离线版 NSIS 安装包与 `SHA256SUMS.txt` 至 `target/release-assets` |

提交前最低验证：`pnpm typecheck && pnpm lint && pnpm test` 与 `cargo test --workspace`。

### 注意事项

- 新克隆仓库在首次直接执行 `cargo build` / `cargo test --workspace` 前，必须先跑一次 `pnpm build` 生成 `dist/`（`src-tauri` 默认开启 `custom-protocol` 内嵌前端产物）。
- 只跑 `pnpm dev` 没有 Tauri IPC，日常联调一律使用 `pnpm tauri dev`。
- 新增或修改 Rust IPC 命令后需运行一次 `pnpm tauri dev` 自动更新 `src/lib/bindings.ts`，严禁手改该文件。
- `vite.config.ts` 忽略 `crates/`、`docs/`、`target/`，修改 Rust 代码后需重启 `pnpm tauri dev`。

## 目录结构

```text
src/          前端源码：app/ 状态中枢，features/ 业务域组件，lib/ IPC 与缓存层，styles/ 样式令牌
src-tauri/    桌面壳：启动与安装交接、托盘、主窗口延迟销毁、IPC 命令、visrecall:// 资源协议
crates/       Rust workspace 成员（见下表）
models/       随包 PP-OCR / BGE 模型、词表、onnxruntime.dll 与 manifest.json 校验清单
scripts/      模型哈希校验、OCR 精度统计、NSIS 发布打包脚本
docs/         技术架构文档、Windows 发布手册、检索评测模板与视觉原型（本地独立仓库管理）
```

| Crate | 职责 |
| --- | --- |
| `domain` | 纯领域类型、`AppError`、Provider / VectorIndex trait、选区几何与量化计算 |
| `library` | 配置与路径、SQLite 迁移与读写、FTS5 / int8 向量 / 标签 / 引用导入源 / 孤儿清理 |
| `capture` | 显示器与窗口枚举、Per-Monitor V2 DPI、GDI 抓帧与裁剪 |
| `shell` | 全局热键、原生悬浮球、D3D11/D2D 冻结帧覆盖层、剪贴板、OLE 拖放 |
| `ai` | ONNX Runtime 动态加载、PP-OCRv4 + Windows OCR 兜底、BGE 嵌入、四协议云端模型、凭据管理 |
| `search` | 非对称分词、查询解析、FTS + 向量 + 软命中三路召回、RRF 融合、命中解释 |
| `pipeline` | `jobs` 表驱动的两阶段索引（`ocr` / `index` / `vlm` / `rebuild`）、崩溃恢复与向量蓝绿重建 |
| `app-core` | 核心编排：捕获、文件/目录/文本导入、检索、详情、WebP 缩略图、存储迁移、事件总线 |
| `eval` | 独立检索评测库与 CLI（`visrecall-eval`） |

依赖方向保持单向无环：`src-tauri → app-core → { shell → capture, pipeline → search → library, ai } → domain`（`src-tauri` 另直连 `capture/library/ai/domain` 做壳层胶水；`search` 不依赖 `ai`，`pipeline` 不依赖 `app-core` 与 `ai`）。

## 反馈与许可证

在 [GitHub Issues](https://github.com/Tomwucpu/VisRecall/issues) 反馈问题时，请附上 VisRecall 版本、Windows 版本、安装包 SHA-256、复现步骤与脱敏后的错误信息（请先移除截图、库文件、凭据和日志中的个人信息）。

随包模型与 ONNX Runtime 的来源与许可详见 [THIRD_PARTY_NOTICES.txt](THIRD_PARTY_NOTICES.txt)。VisRecall 以 [Apache License 2.0](LICENSE) 发布，版权归 Tomwu 所有。
