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

图片内容由大模型生成描述后再写入索引，因此需要先配置大模型 API；OCR、向量化与存储都在本机完成。

## 功能特性

- **素材收集**：全局热键、悬浮球或覆盖层框选截图；导入图片、文件夹和文本；复制的内容可直接入库。
- **图片理解**：图片先经本地 OCR 取字，再由视觉大模型生成内容描述、摘要与自动标签，一起进入索引，因此可以用自然语言描述画面来找图。
- **中文检索**：OCR 文本全文检索（SQLite FTS5）与语义向量检索（BGE）共同召回，融合排序；支持时间、应用、标签等条件筛选。
- **素材整理**：详情面板查看原图、编辑 OCR 文本、重命名、查看自动标签；列表支持多选。
- **AI 配置**：本地 PP-OCR 与 BGE 嵌入随包提供；大模型在「设置 → 模型」自行添加（名称、模型 ID、接口地址、OpenAI 兼容或 Ollama 协议、API 密钥），密钥只存 Windows 凭据管理器，界面不回显。
- **隐私可控**：素材与索引保存在本机（SQLite + 库目录普通文件），素材库可迁移；图片与文本「上传云端解析」是独立开关，默认关闭，未配置模型时不会发起请求；应用常驻系统托盘，单实例运行。

## 安装

1. 在 [GitHub Releases](https://github.com/Tomwucpu/VisRecall/releases) 选择要安装的版本，按目标机器能否联网选择安装包。两者都随包携带本地 OCR/嵌入模型和 ONNX Runtime，安装后即可离线完成 OCR 与全文检索；图片内容的大模型描述需要联网，并在安装后配置模型 API：

  | 安装包 | 体积 | 选择场景 |
  | --- | --- | --- |
  | `VisRecall_<版本>_x64-setup.exe` | 约 42 MB | 机器可以联网。系统缺少 WebView2 时，安装程序会自动下载并安装运行时；Windows 11 和 Windows 10（1803 及以后）通常已自带。 |
  | `VisRecall_<版本>_x64-setup-offline.exe` | 约 330 MB | 机器不能联网，或安装程序被策略限制联网。安装包内置 WebView2 Evergreen 独立安装程序，无需下载。 |

2. 下载同版本页面上的 `SHA256SUMS.txt`，在 PowerShell 核对所选安装包，例如 `Get-FileHash -Algorithm SHA256 -LiteralPath .\VisRecall_0.1.0_x64-setup.exe`，结果应与校验文件中对应行一致。
3. 为本地 OCR 和语义检索准备 [Microsoft Visual C++ v14 Redistributable（x64）](https://learn.microsoft.com/cpp/windows/latest-supported-vc-redist)。离线机器可先下载该运行库安装程序。两种安装包都会安装 WebView2 运行时，差别只在于 WebView2 是否随包提供。
4. 运行安装包，先选「安装位置」，存放程序和随包模型。首次安装再选择「素材库位置」，默认是 `%USERPROFILE%\Documents\VisRecall`。也可选择可写的新目录或空目录，例如 `D:\VisRecall 数据\素材库`。两处目录需要分开。
5. 从开始菜单启动 VisRecall。应用默认在系统托盘启动，点击托盘图标打开主窗口。在「设置 → 存储」可以核对素材库路径，之后也可在该处迁移素材库。

首发候选包面向 Windows 11 x64。实际验收通过的 Windows 版本以对应 Release 的说明为准。未签名候选包可能触发 Windows SmartScreen 警告或被 Smart App Control 阻止，签名状态也以该 Release 的说明为准。

## 使用

- 截图默认关闭。在「设置 → 捕获」开启截图后，可使用设置中的全局热键或悬浮球截图。
- 可以导入图片、文件夹和文本。截图和复制入库的素材存放在素材库目录，引用导入的原始文件仍留在原处。
- 图片内容由大模型生成描述后再建立索引，因此需要先在「设置 → 模型」新增并选用一个视觉模型（填写模型 ID、接口地址、协议与 API 密钥），再开启同一页的「图片上传云端解析」。该开关默认关闭，未配置模型时不会发起请求；此时图片只能按本地 OCR 文本检索。
- 关闭主窗口后，应用继续驻留系统托盘。彻底退出时，右键点击托盘图标并选择「退出」。
- 同版重装、升级和卸载保留素材库和应用配置。卸载后重装可以继续使用原素材库。

## 开发构建

### 技术栈

- 桌面壳：Tauri 2（Rust + WebView2），仅支持 Windows
- 前端：React 19 + TypeScript + Vite + TanStack Query，无路由、无 UI 组件库
- 存储：SQLite（WAL、FTS5）+ 库目录普通文件
- AI：本地 PP-OCR 与 BGE 嵌入（ONNX Runtime）；图片内容描述使用视觉大模型（OpenAI 兼容或 Ollama 协议），需在设置中配置接口与密钥

### 环境要求

| 依赖 | 说明 |
| --- | --- |
| Visual Studio C++ Build Tools、Windows SDK | 提供 MSVC 工具链 |
| Rust | 按 [`rust-toolchain.toml`](rust-toolchain.toml) 自动安装，stable + `x86_64-pc-windows-msvc` |
| Node.js | 发布验证版本 `24.19.0` |
| pnpm | 发布验证版本 `10.30.3` |
| WebView2 运行时 | Windows 11 与 Windows 10（1803 及以后）通常已自带 |

`models/` 中的 PP-OCR、BGE 模型与 `onnxruntime.dll` 已随仓库提供，不需要额外下载。

### 常用命令

| 命令 | 作用 |
| --- | --- |
| `pnpm install` | 安装前端依赖 |
| `pnpm tauri dev` | 启动完整桌面应用（推荐；debug 构建会顺带刷新 `src/lib/bindings.ts`） |
| `pnpm dev` | 只起 Vite，只能看样式和组件 |
| `pnpm typecheck` | TypeScript 类型检查 |
| `pnpm lint` | ESLint 检查 |
| `pnpm test` | 前端单元测试（Vitest） |
| `pnpm build` | 前端类型检查 + 生产构建 |
| `cargo test --workspace` | Rust 测试 |
| `cargo clippy --workspace --all-targets` | Rust 静态检查 |
| `cargo fmt --all` | Rust 格式化 |
| `powershell -File scripts/models-check.ps1` | 校验 `models/` 与 `models/manifest.json` 一致 |
| `powershell -File scripts/ocr-accuracy.ps1` | OCR 精度自检 |
| `pnpm release:build` | 构建轻量版、离线版安装包与 `SHA256SUMS.txt`，输出到 `target/release-assets` |

提交前最低验证：`pnpm typecheck && pnpm lint && pnpm test` 与 `cargo test --workspace`。依赖真实模型文件的 AI 用例用 `cargo test -p visrecall-ai -- --ignored` 运行，发版前必跑。

### 注意事项

- 只跑 `pnpm dev` 时没有 Tauri IPC，界面会报错，需要 `pnpm tauri dev`。
- 新增或修改 Rust 命令后要跑一次 debug 构建（`pnpm tauri dev`）才会刷新 `src/lib/bindings.ts`；该文件是生成物，不要手改。
- `vite.config.ts` 刻意忽略 `crates/`、`docs/`、`target/`，Rust 改动没有 HMR，需要重启 `pnpm tauri dev`。
- 直接 `cargo build` / `cargo run` 前需先执行一次 `pnpm build`，否则缺少内嵌前端资源。
- 关闭主窗口不等同于退出进程：应用常驻托盘，单实例插件会唤醒已有实例。

## 目录结构

```text
src/          前端：app/ 为状态中枢，features/ 按业务域划分，lib/ 收口 IPC 与缓存
src-tauri/    桌面壳：启动顺序、托盘、窗口生命周期、IPC 命令、资源协议
crates/       Rust workspace（见下表）
models/       随包模型、词表与 ONNX Runtime（含 manifest.json 校验清单）
scripts/      模型校验、OCR 精度、发布打包脚本
docs/         设计文档、发布手册与评测材料（内部资料，不随公开仓库发布）
```

| crate | 职责 |
| --- | --- |
| `domain` | 纯类型、错误码、Provider trait、几何计算 |
| `library` | SQLite 迁移与读写、库路径、配置、FTS / 向量 / 标签 |
| `capture` | 屏幕抓帧、显示器与窗口枚举、DPI 处理、裁剪 |
| `shell` | 全局热键、悬浮球、覆盖层、剪贴板、OLE 拖放 |
| `ai` | OCR 链、BGE 嵌入、云端 VLM、密钥与模型清单 |
| `search` | 查询解析、三路召回、RRF 融合、命中解释 |
| `pipeline` | 两阶段索引任务：先本地 OCR + 向量索引，再由大模型补图片描述与标签 |
| `app-core` | 编排：捕获、导入、检索、详情、事件总线、管线宿主 |
| `eval` | 检索评测 CLI |

依赖方向保持单向：`src-tauri → app-core → { shell, pipeline } → { capture, library, search, ai } → domain`。`search` 不依赖 `ai`，`pipeline` 不依赖 `app-core`。

## 反馈与第三方声明

在 [GitHub Issues](https://github.com/Tomwucpu/VisRecall/issues) 反馈问题时，请提供 VisRecall 版本、Windows 版本、安装包 SHA-256、复现步骤和错误信息。请先移除截图、库文件、凭据和日志中的个人信息。

[第三方声明](THIRD_PARTY_NOTICES.txt)记录随包模型和 ONNX Runtime 的来源及许可。安装器模板的许可随安装包提供。

## 许可证

VisRecall 以 [Apache License 2.0](LICENSE) 发布，版权归 Tomwu 所有。可以自由使用、修改、分发和商用，需保留版权与许可声明、在修改过的文件中标注改动，并在分发时附带上文列出的第三方声明。该许可同时提供明确的专利授权。
