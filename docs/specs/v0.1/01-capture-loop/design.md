# 阶段 01：捕获闭环 · 设计

> 对应需求：`requirements.md` P1-R01–P1-R12  
> 技术依据：《VisRecall-技术架构设计 v1.2》§3、§4、§5.1–5.4、§8  
> 目标：用最少的子系统完成可日常使用的捕获闭环；不引入 OCR、向量、检索。

## 1. 设计结论

| 决策 | 结论 | 理由 |
|---|---|---|
| 进程模型 | 单进程多线程 | 与架构一致；避免 IPC 与状态同步 |
| 桌面骨架 | Tauri 2 + React 19 + TS | 主窗口/设置用 WebView；托盘与原生窗口独立 |
| 悬浮球 | 原生 Win32 分层窗口 | OLE 拖放、置顶不抢焦点、低内存 |
| 覆盖层 | 原生 Win32 + D3D11 + Direct2D | 4K 多屏冻结帧与选区渲染 |
| 抓帧 | GDI BitBlt → D3D11 纹理 | 静态画面可靠；CPU 位图上传后立即释放 |
| 选区几何 | 纯函数状态机，物理像素坐标 | 可单测、可 proptest、避免 DPI 偏移 |
| 存储 | SQLite bundled + WAL | 单文件、事务、可搬移 |
| 缩略图 | 阶段 01 先用 PNG（480px 长边） | `image` 内置编码，避免引入 libwebp 构建链；阶段 04 再评估 WebP |
| 前端 IPC | `tauri-specta` 生成 TypeScript | 不手写 DTO；阶段 01 命令少，先建立生成链路 |
| 任务队列 | 阶段 01 只建 `jobs` 表与恢复骨架 | 真实 OCR/VLM 任务在阶段 03 接入 |

### 1.1 已知偏差

- 架构文档写缩略图为 `.webp`；阶段 01 使用 `.png`，避免 `libwebp` 原生依赖。阶段 04 若安装包体积或缩略图加载不达标，再切换 WebP 并重建缩略图。
- 架构文档规定 4 读连接 + 1 写连接；阶段 01 使用「写连接加锁 + 按需只读连接」的简单模型，阶段 03 检索并发上来后再引入连接池。
- 阶段 01 悬浮球只做「球形态 + 单击 + 拖动 + 拖放接收视觉反馈」；贴边收起、悬停展开、全屏隐藏、首次引导在阶段 02 补齐。

## 2. 工作区与文件结构

```text
VisRecall/
├─ Cargo.toml                         # workspace
├─ rust-toolchain.toml
├─ package.json
├─ pnpm-lock.yaml
├─ vite.config.ts
├─ tsconfig.json
├─ index.html
├─ crates/
│  ├─ domain/
│  │  └─ src/{lib.rs, ids.rs, geometry.rs, item.rs, capture.rs, error.rs}
│  ├─ library/
│  │  └─ src/{lib.rs, config.rs, paths.rs, db.rs, migrations.rs, items.rs, files.rs}
│  ├─ capture/
│  │  └─ src/{lib.rs, display.rs, grab_gdi.rs, window.rs, crop.rs, trait_defs.rs}
│  ├─ shell/
│  │  └─ src/
│  │     ├─ lib.rs
│  │     ├─ hotkey.rs
│  │     ├─ ball.rs
│  │     ├─ selection.rs
│  │     ├─ drop_target.rs
│  │     └─ overlay/{mod.rs, d3d.rs, render.rs, input.rs}
│  └─ app-core/
│     └─ src/{lib.rs, app.rs, events.rs, capture_flow.rs, import_flow.rs, thumbnail.rs}
├─ src-tauri/
│  ├─ Cargo.toml
│  ├─ build.rs
│  ├─ tauri.conf.json
│  ├─ capabilities/default.json
│  ├─ icons/icon.ico              # 占位图标；正式图标阶段 04 替换
│  └─ src/{main.rs, tray.rs, ipc.rs, protocol.rs, window_lifecycle.rs}
├─ src/
│  ├─ main.tsx
│  ├─ app/{App.tsx, ErrorBoundary.tsx}
│  ├─ features/library/{ItemGrid.tsx, ItemCard.tsx, EmptyState.tsx, useItems.ts}
│  ├─ lib/{bindings.ts, queryClient.ts, events.ts, format.ts}
│  └─ styles/globals.css
└─ tests/
   ├─ capture_flow.rs
   └─ fixtures/
```

阶段 01 不创建 `search`、`ai`、`pipeline` crate；阶段 03 再按架构新增，避免空壳。

## 3. 线程与所有权

```text
main thread (Tauri/tao)
  ├─ tray / main window / settings window
  ├─ receives ShellEvent + OverlayEvent
  └─ delegates capture to CaptureCoordinator

hotkey thread
  └─ RegisterHotKey + message loop -> ShellEvent::CaptureRequest

shell-ball thread (STA)
  ├─ layered ball window
  ├─ OLE IDropTarget
  └─ -> ShellEvent::{CaptureRequest, Drop}

capture worker (dedicated std::thread)
  ├─ record foreground
  ├─ hide ball + DwmFlush
  ├─ GDI grab all displays -> Vec<RawFrame>
  └─ -> OverlayCommand::Show

overlay thread
  ├─ owns D3D11 device, swap chains, frozen textures, overlay HWNDs
  ├─ D2D draws frozen frame + dim + selection + handles + label
  ├─ receives mouse/keyboard, updates SelectionState
  └─ -> OverlayEvent::{Committed(CapturedImage), Cancelled}

tokio runtime (Tauri managed)
  ├─ async file IO / thumbnail generation (spawn_blocking)
  └─ DB writes serialized by Library write mutex
```

规则：

- D3D11 设备、swap chain、D3D 纹理只在覆盖层线程创建和使用；不跨线程移动 COM 接口。
- 抓到的是 `RawFrame`（CPU `Vec<u8>`，`Send`）；覆盖层线程负责上传纹理。
- 覆盖层提交时只回读选区并返回 `CapturedImage`（BGRA），PNG 编码与落盘在 app-core 的工作线程完成。
- UI 线程不做同步 IO；所有 IPC 命令只做参数校验、投递任务、返回 ID/结果。
- 原生 UI 线程分别 `CoInitializeEx`：悬浮球 STA（OLE 拖放），覆盖层按 D2D 要求初始化。

### 3.1 线程间消息类型

```rust
#[derive(Clone, Copy, Debug)]
pub enum CaptureTrigger { Hotkey, BallClick, UiButton, Drop }

pub enum ShellEvent {
    CaptureRequest(CaptureTrigger),
    Drop(visrecall_domain::drop::DropContent),
    BallVisibility(bool),
    BallMoved { display_id: String, nx: f32, ny: f32 },
}

pub enum OverlayCommand {
    Show {
        frames: Vec<RawFrame>,
        displays: Vec<DisplayInfo>,
        foreground: ForegroundInfo,
    },
    Cancel,
}

pub enum OverlayEvent {
    Committed {
        image: CapturedImage,
        global_rect: PhysicalRect,
        foreground: ForegroundInfo,
    },
    Cancelled,
}

pub struct OverlayHandle { tx: crossbeam_channel::Sender<OverlayCommand> }

impl OverlayHandle {
    pub fn show(&self, frames: Vec<RawFrame>, displays: Vec<DisplayInfo>, foreground: ForegroundInfo) -> Result<(), AppError>;
    pub fn cancel(&self) -> Result<(), AppError>;
}

pub struct ShellHandle { /* crossbeam senders to hotkey/ball threads */ }

impl ShellHandle {
    pub fn hide_own_windows(&self) -> Result<(), AppError>;
    pub fn show_own_windows(&self);
    pub fn set_ball_visible(&self, visible: bool);
}
```

- `ShellEvent` 由热键线程、悬浮球线程发送给 app-core。
- `OverlayCommand`/`OverlayEvent` 在 app-core 与覆盖层线程之间传递；不跨线程传 D3D COM 接口。
- 所有消息使用 `crossbeam-channel`，不走 JSON。

## 4. 领域类型

`crates/domain/src/geometry.rs`：

```rust
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PhysicalPoint { pub x: i32, pub y: i32 }

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PhysicalRect { pub x: i32, pub y: i32, pub width: u32, pub height: u32 }

impl PhysicalRect {
    pub fn new(x: i32, y: i32, width: u32, height: u32) -> Self;
    pub fn from_points(a: PhysicalPoint, b: PhysicalPoint) -> Self; // 自动归一化
    pub fn right(&self) -> i32;
    pub fn bottom(&self) -> i32;
    pub fn is_empty(&self) -> bool;
    pub fn contains(&self, p: PhysicalPoint) -> bool;
    pub fn intersect(&self, other: &Self) -> Option<Self>;
    pub fn union(&self, other: &Self) -> Self;
    pub fn inflate(&self, dx: i32, dy: i32) -> Self;
}
```

`crates/domain/src/capture.rs`：

```rust
pub struct DisplayInfo {
    pub id: String,              // 设备名，如 \\.\DISPLAY1
    pub bounds: PhysicalRect,    // 物理像素，可为负坐标
    pub work_area: PhysicalRect,
    pub dpi: u32,                // 96/120/144/192
    pub primary: bool,
}

pub struct RawFrame {
    pub display_id: String,
    pub width: u32,
    pub height: u32,
    pub stride: usize,           // bytes per row
    pub bgra: Vec<u8>,           // top-down, BGRA8
}

pub struct ForegroundInfo {
    pub hwnd: isize,
    pub pid: u32,
    pub process_name: String,
    pub display_name: String,
    pub title: String,
    pub bounds: PhysicalRect,
}

pub struct CapturedImage {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,   // top-down, BGRA8，宽高为选区尺寸
}

pub struct WindowInfo {
    pub hwnd: isize,
    pub title: String,
    pub class_name: String,
    pub process_name: String,
    pub bounds: PhysicalRect,
    pub tool_window: bool,
    pub cloaked: bool,
}

pub enum DropContent {
    Files(Vec<std::path::PathBuf>),
    Text { plain: String, html: Option<String> },
    Url { url: String },
    ImageDib { width: u32, height: u32, bgra: Vec<u8> },
}

pub trait FrameGrabber: Send + Sync {
    fn grab(&self, displays: &[DisplayInfo]) -> Result<Vec<RawFrame>, AppError>;
}

pub trait WindowProbe: Send + Sync {
    fn foreground(&self) -> Result<ForegroundInfo, AppError>;
    fn enumerate(&self) -> Result<Vec<WindowInfo>, AppError>;
    fn hit_chain(&self, point: PhysicalPoint) -> Result<Vec<WindowInfo>, AppError>;
    fn hit_chain_excluding(&self, point: PhysicalPoint, excluded_pid: u32) -> Result<Vec<WindowInfo>, AppError>;
}

pub fn now_millis() -> i64;  // 全部 captured_at/created_at/updated_at 使用 Unix 毫秒
```

> `FrameGrabber`/`WindowProbe` 的 trait 声明与实现位于 `crates/capture`（`trait_defs.rs` / `window.rs`），domain 只定义数据与错误类型；`DropContent` 位于 `crates/domain/src/drop.rs`，由 shell 重新导出。阶段 01 的 `ForegroundInfo.display_name` 解释为进程显示名；所在显示器由 `bounds` 与 `DisplayInfo` 交集推断（schema 不存显示器列）。

> 时间戳统一使用 Unix **毫秒**。阶段 01 的 keyset 游标用 `captured_at`（毫秒）+ `rowid` 兜底；同一毫秒内极少见，阶段 02 若需要可升级为复合游标。

`crates/domain/src/item.rs`：

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ItemKind { Image, Text, Link }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ItemStatus { Parsing, Ready, Pending, Failed }

pub struct NewItem {
    pub kind: ItemKind,
    pub captured_at: i64,
    pub source_app: Option<String>,
    pub source_title: Option<String>,
    pub source_url: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub char_count: Option<u32>,
    pub original_path: String,   // 相对库根目录
    pub content_hash: String,
}

pub struct Item {
    pub id: String,
    pub kind: ItemKind,
    pub status: ItemStatus,
    pub captured_at: i64,
    pub source_app: Option<String>,
    pub source_title: Option<String>,
    pub source_url: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub char_count: Option<u32>,
    pub original_path: String,
    pub thumb_path: Option<String>,
    pub note: String,
    pub content_hash: String,
}
```

`crates/domain/src/error.rs`：

```rust
#[derive(Debug, thiserror::Error, Serialize)]
pub enum AppError {
    #[error("config missing")] ConfigMissing,
    #[error("invalid input: {0}")] InvalidInput(String),
    #[error("not found")] NotFound,
    #[error("storage full")] StorageFull,
    #[error("capture failed: {0}")] CaptureFailed(String),
    #[error("capture cancelled")] CaptureCancelled,
    #[error("drm protected")] DrmProtected,
    #[error("io: {0}")] Io(String),
    #[error("database: {0}")] Database(String),
    #[error("internal: {0}")] Internal(String),
}
```

错误码序列化为前端可映射的字符串；阶段 01 只使用上述变体。

## 5. 捕获流程

```text
Hotkey / BallClick / UI
  -> CaptureRequest { trigger, requested_at }
  -> CaptureCoordinator::start
       1. WindowProbe::foreground()          记录来源窗口
       2. ShellHandle::hide_own_windows()     隐藏悬浮球/气泡
       3. DwmFlush()                          等待合成
       4. DisplayProbe::enumerate()           显示器列表（物理像素/DPI）
       5. FrameGrabber::grab(displays)        逐显示器 GDI 抓帧 -> Vec<RawFrame>
       6. 恢复悬浮球（覆盖层显示后）
       7. OverlayHandle::show(frames, displays, foreground)
  -> Overlay thread
       8. 为每个显示器创建/复用 swap chain 与窗口
       9. 上传 RawFrame 到 D3D11 纹理
      10. 显示冻结帧，进入选区态
      11. 用户提交 -> 回读选区 -> 拼合 BGRA -> 关闭窗口
  -> CaptureCoordinator::on_overlay_event(Committed)
      12. PNG encode -> tmp/<uuid>.tmp -> fsync -> rename
      13. Library::insert_item(status=parsing)
      14. emit item_created
      15. spawn thumbnail -> update thumb_path/status=ready -> emit item_updated
```

失败分支：

- `foreground()` 失败：仍允许捕获，来源字段置空。
- `grab()` 失败：重试一次；仍失败 → 覆盖层实时模式（透明窗口，提交时现场抓选区）。
- 全黑帧：返回 `DrmProtected`，不显示覆盖层，托盘提示。
- 提交后落盘失败：覆盖层已关闭，托盘提示 `Io`/`StorageFull`，临时文件清理。
- 缩略图失败：素材保持 `parsing` 或转 `failed`，列表显示占位；不阻塞主流程。

## 6. 坐标系统与 DPI

- 进程声明 **Per-Monitor V2** DPI 感知（Tauri 自定义 manifest 或在 `main.rs` 启动前设置）。
- 所有几何使用**虚拟屏幕物理像素**：显示器 `bounds` 来自 `GetMonitorInfoW`，DPI 来自 `GetDpiForMonitor`。
- 覆盖层窗口位置/尺寸使用物理像素；D2D 上下文 `SetDpi(96, 96)`，因此 1 DIP = 1 物理像素。
- 鼠标坐标用 `GetCursorPos`（PMv2 下为物理像素）；跨屏时保持全局坐标。
- 选中窗口边界用 `DWMWA_EXTENDED_FRAME_BOUNDS`，避免 `GetWindowRect` 的不可见边框。
- 最小选区 10×10 物理像素；控制点命中区域按 96 DPI 下的 10px 设计，在 150%/200% 下按 DPI 缩放。
- 提交时按全局选区与每个显示器 `bounds` 求交，逐屏回读后拼合；屏间空隙填 0（透明黑），阶段 01 接受该限制。

## 7. 覆盖层渲染

### 7.1 窗口

每个显示器一个 `WS_POPUP` 窗口：

- 位置 = `DisplayInfo.bounds`，客户区尺寸 = `bounds.width × bounds.height`。
- 扩展样式：`WS_EX_TOPMOST | WS_EX_TOOLWINDOW`。
- 不使用 `WS_EX_NOACTIVATE`，因为覆盖层需要键盘焦点；显示后 `SetForegroundWindow` + `SetFocus`，失败时 `AttachThreadInput` 兜底。
- 类名 `VisRecallOverlay`，注册一次；窗口过程只处理输入与绘制消息。
- 所有窗口共享一个 `SelectionState`；鼠标在起始窗口 `SetCapture`，`WM_MOUSEMOVE` 用全局坐标更新并重绘所有覆盖层。
- 覆盖层可见时，`WindowFromPoint` 会命中覆盖层自身；窗口高亮必须使用 `hit_chain_excluding(point, GetCurrentProcessId())` 从 Z 序顶层向下跳过本进程窗口，再对选中窗口构建子窗口链。
- 关闭后把焦点还给捕获前的前台窗口。

### 7.2 D3D11/D2D 管线

```text
D3D11CreateDevice(BGRA_SUPPORT, 默认适配器, 硬件 -> WARP 回退)
  -> IDXGIDevice -> IDXGIAdapter -> IDXGIFactory2
  -> CreateSwapChainForHwnd(
         DXGI_FORMAT_B8G8R8A8_UNORM,
         DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
         2 buffers
     )
  -> D2D1CreateFactory -> ID2D1Factory1::CreateDevice(d3d_device)
  -> ID2D1Device::CreateDeviceContext
  -> 每帧：CreateBitmapFromDxgiSurface(back buffer) -> BeginDraw
          DrawBitmap(frozen texture bitmap)
          FillRectangle(压暗区域)
          DrawRectangle(选区)
          FillRectangle(8 个控制点)
          DrawText(尺寸/窗口标签/已冻结)
          EndDraw -> Present(1, 0)
```

- 冻结帧：`RawFrame.bgra` 通过 `UpdateSubresource` 上传到 `D3D11_USAGE_DEFAULT` 纹理；随后 CPU `Vec<u8>` 释放。
- 冻结纹理转 D2D bitmap：`ID3D11Texture2D -> IDXGISurface -> CreateBitmapFromDxgiSurface(D2D1_BITMAP_OPTIONS_NONE)`。
- 回读：提交时为每个相交显示器创建 staging texture，`CopyResource` 后 `Map`，按选区裁剪并拼合。
- 颜色空间：阶段 01 按 SDR 处理；HDR 暂不做色调映射。

### 7.3 门禁与降级

- **门禁**：2 周内单屏 4K 拖动控制点稳定 60fps，双 4K 抓帧 + 首帧显示 ≤150ms。
- 门禁不通过时按顺序降级：
  1. 覆盖层渲染分辨率降为 0.5×（提交仍从全分辨率纹理裁剪）。
  2. 改用 GDI+ 分层窗口渲染（不做 60fps 保证，记录为已知限制）。
  3. 仍不达标则重新评审覆盖层方案，不进入后续阶段。
- 抓帧失败/DRM：覆盖层不显示，托盘提示；无法识别窗口时仍允许拖拽框选。

## 8. 选区状态机

`crates/shell/src/selection.rs`：

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionPhase { Idle, HoverWindow, Dragging, Adjusting, Committed, Cancelled }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handle { NorthWest, North, NorthEast, East, SouthEast, South, SouthWest, West, Move }

#[derive(Clone, Copy, Debug, Default)]
pub struct Modifiers { pub shift: bool, pub ctrl: bool }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key { Left, Right, Up, Down, Enter, Escape }

pub struct SelectionState {
    pub phase: SelectionPhase,
    pub anchor: PhysicalPoint,
    pub start_rect: PhysicalRect,
    pub rect: PhysicalRect,          // 用户拖拽/调整得到的选区，不含悬停高亮
    pub hover_rect: PhysicalRect,    // 当前悬停命中的窗口候选，仅用于高亮/单击采用
    pub active: Option<Handle>,
}

impl SelectionState {
    pub fn new() -> Self;
    pub fn on_mouse_down(&mut self, p: PhysicalPoint, handle: Option<Handle>);
    pub fn on_mouse_move(&mut self, p: PhysicalPoint, m: Modifiers, candidates: &[PhysicalRect]);
    pub fn on_mouse_up(&mut self);
    pub fn on_key(&mut self, key: Key, shift: bool, candidates: &[PhysicalRect]);
    pub fn on_wheel(&mut self, delta: i32);
    pub fn hovered_rect(&self) -> Option<PhysicalRect>;
    pub fn commit(&mut self) -> Option<PhysicalRect>;
    pub fn cancel(&mut self);
}
```

纯函数（可单测）：

```rust
pub fn hit_test_handle(rect: PhysicalRect, p: PhysicalPoint, dpi: u32) -> Option<Handle>;
pub fn drag_rect(handle: Handle, start: PhysicalRect, delta: PhysicalPoint, min: u32) -> PhysicalRect;
pub fn snap_rect(rect: PhysicalRect, candidates: &[PhysicalRect], threshold: i32) -> PhysicalRect;
pub fn clamp_to_virtual_screen(rect: PhysicalRect, displays: &[DisplayInfo]) -> PhysicalRect;
```

规则：

- `Dragging`：从 anchor 到当前点 `from_points`；小于 10×10 仍显示但提交时取消。
- `Adjusting`：拖控制点时固定对角/对边；不允许宽高为负；最小 10×10。
- `Move`：整体平移；保持尺寸。
- 吸附：距候选窗口边 8 物理 px 内对齐；`Ctrl` 关闭；方向键微调后不吸附。`Move` 只做整框平移吸附、不得改变宽高；边/角手柄只吸附被拖动的边，固定边保持不动。状态机内部使用 `snap_move_rect`/`snap_resize_rect`/`snap_drag_rect`（`pub(crate)`）实现上述三种语义；公开 `snap_rect` 作为通用四边吸附保留。
- 滚轮：在指针下的窗口链中切换 `hover_rect` 并重算高亮；单击（未拖动）采用 `hover_rect` 的窗口边界。
- `HoverWindow` 的鼠标移动只更新 `hover_rect`，不得覆盖已有 `rect`；取消时清空 `rect` 与 `hover_rect`。
- `Esc`/右键：任意阶段取消；`Enter`：提交（若有效）。

## 9. 悬浮球与 OLE 拖放

### 9.1 悬浮球

- 窗口：`WS_POPUP`，扩展样式 `WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE`。
- 绘制：32 位 ARGB DIB + `UpdateLayeredWindow`；GDI+ 绘制圆、阴影、图标。
- 交互：单击触发捕获；按下后移动 >4px 视为拖动；位置按显示器记忆，写入配置的归一化坐标。
- 阶段 01 不实现贴边收起/悬停展开/全屏隐藏/引导气泡；保留 `BallConfig` 字段，阶段 02 使用。
- 拖拽接收：球体放大 1.15×，根据 `IDropTarget` 可用格式显示「存为图片 / 存为片段 / 存为链接 / 导入 N 个文件」。

```rust
pub struct BallHandle { /* sender to ball thread */ }
impl BallHandle {
    pub fn set_visible(&self, visible: bool);
}
```

### 9.2 OLE 拖放

```rust
pub trait DropTargetSink: Send {
    fn on_drop(&self, content: DropContent) -> Result<()>;
}
```

`DropContent` 定义在 `crates/domain/src/drop.rs`，悬浮球线程与 app-core 共用；`shell::drop_target` 重新导出它，便于测试。

- 悬浮球线程 `OleInitialize` + `RegisterDragDrop`；`DragEnter/DragOver` 只读可用格式并更新视觉状态，`Drop` 读取数据后投递 `ShellEvent::Drop`。
- 阶段 01 必须支持 `CF_HDROP`、`CF_UNICODETEXT`、`CF_HTML`、`CF_DIB`；`CF_DIB` 用 `GetDIBits` 转 BGRA 后走图片入库。若拖拽源还注册了 PNG 格式，优先按 PNG 解码，失败回退 `CF_DIB`。
- 文本/HTML 剥离格式用简单标签剥离与 `html_escape` 反向处理；URL 用 `url` crate 判定。
- 文件导入在 app-core 的导入流程中逐文件校验、复制/转码，不阻塞悬浮球线程。

## 10. 存储

### 10.1 阶段 01 Schema

`crates/library/migrations/0001_core.sql`（PRAGMA 在打开连接时设置，不放在迁移事务里）：

```sql
CREATE TABLE IF NOT EXISTS schema_migrations (
  version INTEGER PRIMARY KEY,
  applied_at INTEGER NOT NULL
);

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
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE INDEX idx_items_captured ON items(captured_at DESC);
CREATE INDEX idx_items_status ON items(status);
CREATE INDEX idx_items_hash ON items(content_hash);

CREATE TABLE settings (
  key TEXT PRIMARY KEY,
  value_json TEXT NOT NULL
);

CREATE TABLE jobs (
  id TEXT PRIMARY KEY,
  item_id TEXT NOT NULL,
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
```

- 连接打开后先执行 `PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;`，再按 `schema_migrations` 应用 DDL；不要在迁移事务内切换 journal mode。
- `Library::recover_jobs() -> Result<u64>`：启动时把 `jobs.state='running'` 改为 `queued`，返回恢复条数；阶段 01 只做状态恢复，阶段 03 接入真实任务。
- 阶段 02 追加 `insights/tags/item_tags/collections/queries`；阶段 03 追加 FTS/向量表。阶段 01 不预建未使用表。

### 10.2 文件写入

- 图片：`tmp/<uuid>.tmp` 写入 → `File::sync_all` → `rename` 到 `originals/images/YYYY/MM/<uuid>.png` → 插入 DB。
- 文本：`texts/<uuid>.txt`，UTF-8；同样走临时文件 + rename。
- 路径存相对库根目录；读取时拼接并 `canonicalize` 校验仍在库内。
- 启动时删除 `tmp/` 下超过 24h 的文件；扫描 `originals/` 与 `texts/` 中数据库不存在的孤儿文件，记录日志但不自动删除（阶段 02 提供清理入口）。
- 缩略图：`thumbs/<uuid>.png`，长边 480px；生成失败不阻塞列表。

### 10.3 配置

**配置：**

```rust
pub struct AppPaths {
    pub config_file: PathBuf,
    pub default_library_root: PathBuf,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LibraryMeta {
    pub library_id: String,
    pub format_version: u32,
    pub created_at: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub library_path: PathBuf,
    pub hotkeys: HotkeyConfig,
    pub ball: BallConfig,
    pub capture: CaptureConfig,
    pub privacy: PrivacyConfig,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct HotkeyConfig { pub capture: String }

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct BallConfig {
    pub enabled: bool,
    pub size: u32,
    pub positions: std::collections::HashMap<String, (f32, f32)>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct CaptureConfig { pub realtime_fallback: bool }

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct PrivacyConfig { pub clipboard_capture: bool, pub telemetry: bool }

impl Default for AppConfig { /* 默认库路径 + Ctrl+Shift+S + ball enabled/size 48 */ }
```

`config.json` 阶段 01 字段：

```json
{
  "library_path": "C:\\Users\\Tomwu\\Documents\\VisRecall",
  "hotkeys": { "capture": "Ctrl+Shift+S" },
  "ball": { "enabled": true, "size": 48, "positions": {} },
  "capture": { "realtime_fallback": true },
  "privacy": { "clipboard_capture": false, "telemetry": false }
}
```

- 原子写入：临时文件 + `rename`。
- 读取失败回退默认值，保留损坏文件为 `config.json.bad-<timestamp>`，并通过事件提示。

## 11. IPC 与前端

### 11.1 命令（阶段 01）

```text
list_items(cursor: Option<i64>, limit: u32) -> Result<Vec<ItemDto>, ApiError>
get_item(id: String) -> Result<ItemDto, ApiError>
get_app_status() -> AppStatus
get_settings() -> SettingsDto
update_settings(patch: SettingsPatch) -> Result<SettingsDto, ApiError>
open_library_folder() -> Result<(), ApiError>
```

IPC 错误 DTO（不把 domain 的 `AppError` 暴露给 specta）：

```rust
#[derive(Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct ApiError {
    pub code: String,      // CONFIG_MISSING / NETWORK / STORAGE_FULL / NOT_FOUND / INVALID_INPUT / INTERNAL ...
    pub message: String,
}

impl From<AppError> for ApiError { /* 映射变体到稳定错误码 */ }
```

DTO 定义在 `src-tauri/src/ipc.rs`，避免 domain 依赖 specta：

```rust
#[derive(Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct ItemDto {
    pub id: String,
    pub kind: String,          // image/text/link
    pub status: String,        // parsing/ready/pending/failed
    pub captured_at: i64,
    pub source_app: Option<String>,
    pub source_title: Option<String>,
    pub source_url: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub char_count: Option<u32>,
    pub note: String,
    pub thumb_url: Option<String>,
}

#[derive(Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct AppStatus {
    pub version: String,
    pub library_path: String,
    pub library_ok: bool,
}

#[derive(Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct SettingsDto {
    pub library_path: String,
    pub capture_hotkey: String,
    pub ball_enabled: bool,
    pub ball_size: u32,
}

#[derive(Deserialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct SettingsPatch {
    pub ball_enabled: Option<bool>,
    pub ball_size: Option<u32>,
    pub capture_hotkey: Option<String>,
}
```

`ItemDto.thumb_url` 由后端拼接为 `visrecall://thumb/<id>`；前端不自行拼路径。

### 11.2 事件

```text
item_created { id }
item_updated { id, fields[] }
capture_completed { item_id, thumb_url }
capture_failed { code, message }
hotkey_conflict { message }
storage_warning { free_bytes }
app_recovered { count }
```

```rust
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemEvent { pub id: String }

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemUpdatedEvent { pub id: String, pub fields: Vec<String> }

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureFailedEvent { pub code: String, pub message: String }
```

前端收到事件后使 TanStack Query 缓存失效并重取；不把大图/大文本放进事件。

`app-core` 不直接依赖 Tauri：定义 `EventSink: Send + Sync { fn emit(&self, event: AppEvent); }`，`src-tauri` 用 `AppHandle` 实现（`app.emit("item_created", ItemEvent { id })`）。

```rust
pub enum AppEvent {
    ItemCreated(ItemEvent),
    ItemUpdated(ItemUpdatedEvent),
    CaptureFailed(CaptureFailedEvent),
    HotkeyConflict { message: String },
    AppRecovered { count: u64 },
}

pub trait EventSink: Send + Sync {
    fn emit(&self, event: AppEvent);
}
```

### 11.3 资源协议

- `visrecall://thumb/<item_id>`：按 ID 查 DB 取 `thumb_path`，校验相对路径后返回 PNG；缺图返回 404。
- `visrecall://original/<item_id>?max=2048`：阶段 02 使用；阶段 01 只做 thumb。
- 协议处理器拒绝绝对路径、`..`、不存在 ID；不暴露任意文件。

### 11.4 React

```text
src/
  app/App.tsx                 标题栏 + 工具栏 + 列表
  features/library/
    useItems.ts               useInfiniteQuery + 事件失效
    ItemGrid.tsx              CSS grid + 虚拟滚动（阶段 01 可先用简单分页）
    ItemCard.tsx              缩略图/状态/来源/时间
    EmptyState.tsx            快捷键引导
  lib/
    bindings.ts               tauri-specta 生成物（不手改）
    events.ts                 订阅 item_created/item_updated
    queryClient.ts            QueryClient 配置
```

## 12. 失败与降级矩阵

| 场景 | 行为 | 用户可见 |
|---|---|---|
| GDI 抓帧失败 | 重试一次；仍失败进入实时模式 | 覆盖层角标「实时模式」 |
| 全黑帧/DRM | 丢弃，不显示覆盖层 | 托盘通知「该窗口无法截取」 |
| 窗口识别失败 | 不高亮，仍可拖拽 | 不报错 |
| 选区 <10×10 | 取消 | 提示「选区过小」 |
| 落盘失败 | 清理临时文件 | 托盘通知 + 错误码 |
| 存储 <2GB | 拒绝新捕获，保留浏览 | 托盘通知 + 设置入口 |
| 缩略图失败 | 素材保留 `parsing`/`failed` | 卡片占位 + 重试入口（阶段 02） |
| 覆盖层聚焦失败 | `AttachThreadInput` 兜底 | 仍失败提示点击一次 |
| 拖放格式不支持 | 不产生记录 | 气泡列出支持格式 |
| 配置损坏 | 回退默认值 | 顶部提示 |

## 13. 测试策略

| 层 | 内容 | 工具 |
|---|---|---|
| 单元 | 矩形归一化/相交/吸附、选区状态机、热键解析、路径安全、拖放分类、文件名清洗 | `cargo test` + `proptest` |
| 集成 | 临时库 + 假抓帧器 + 假覆盖层：提交 → 文件 → DB → 事件；配置/迁移往返 | `cargo test --test capture_flow` |
| 原生手工 | 抓帧/窗口识别/覆盖层 60fps/多屏 DPI/OLE 四类来源/托盘内存 | PresentMon、任务管理器、实机矩阵 |
| 前端 | ItemCard 状态、空状态、事件失效 | Vitest + React Testing Library |
| 基准 | 100 条以内合成素材的列表/缩略图/启动耗时 | 简单计时脚本，不做万级压测 |

测试隔离：

- 所有文件系统测试用 `tempfile::TempDir`，不写用户目录。
- Win32 相关逻辑通过 trait 注入假实现；纯几何/解析逻辑不依赖 HWND。
- 手工测试使用真实截图、真实窗口，样张保存到 `tests/fixtures/`（不含隐私内容）。

## 14. 风险与门禁

| 风险 | 门禁 | 不通过时 |
|---|---|---|
| D3D11 覆盖层复杂度 | 2 周内 4K 60fps + 首帧 ≤150ms | 降渲染分辨率 → GDI+ 覆盖层 → 重新评审 |
| OLE 拖放兼容性 | Chrome/Word/资源管理器/微信四类来源 | 缩短支持范围或加原生辅助落点窗口 |
| WebView2 内存回收 | 关闭 60s 后进程退出，托盘 ≤150MB | 立即销毁窗口；仍超标则重新评审 |
| 多屏混合 DPI 偏移 | 100/125/150/200% 选区与保存图一致 | 统一物理像素修正；必要时先支持同 DPI 多屏 |
| GDI 抓帧 DRM/黑屏 | 打开 DRM 视频验证丢弃路径 | 增加 DXGI 快路径；仍失败提示不支持 |
| 拖放编码 | UTF-8/GB18030 文本实测 | 增加编码探测库或明确限制 |

## 15. 阶段 01 完成后的系统边界

- 可捕获、可浏览、可拖放；没有 OCR/VLM/检索。
- `status=ready` 只表示缩略图已生成；阶段 03 引入「可检索」状态语义时，再扩展 `jobs` 与事件，不改变 `items` 主键/文件布局。
- 阶段 02 在不改表结构的前提下可追加详情、标签、备注、复制、删除；`items` 已包含 `note` 字段，标签走新表。
