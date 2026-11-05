# 阶段 01 验收记录（01-capture-loop）

本文件记录手工验收与性能门禁的**实测**结果。逐任务的自动化测试见各 crate 的
`cargo test`；本文件只放需要真实机器测量的项目。

> Task 10 建立本文件并写入「D3D11 门禁」；Task 19（2026-11-05）补齐阶段 01 验收矩阵、
> 性能记录、已知限制与结论（见文末「阶段 01 退出验收」一节）。仍有一批项目**没有**
> 在本机条件下测到（物理 4K、PresentMon、双屏/混合 DPI、物理拖放、DRM、100% 缩放等），
> 文末逐项标 `NOT RUN` 并写明原因。未填写的项一律视为 **未验证**，不得当作通过。

## 测量环境

| 项目 | 值 |
|---|---|
| 日期 | 2026-11-04（Fix round 1 复测） |
| 操作系统 | Windows 11 家庭中文版 10.0.22621（Build 22621） |
| 物理显示器 | 1 台，2560×1440 @ 165Hz，缩放 150%（DPI 144），设备名 `\\.\DISPLAY1` |
| 刷新率来源 | `EnumDisplaySettingsW(ENUM_CURRENT_SETTINGS).dmDisplayFrequency` → `refresh_hz=165` |
| GPU（显示输出） | AMD Radeon(TM) 780M（驱动 32.0.11026.1） |
| GPU（独显，未用于覆盖层） | NVIDIA GeForce RTX 4060 Laptop GPU（驱动 32.0.15.6636） |
| GPU（虚拟适配器） | GameViewer Virtual Display Adapter（驱动 15.6.5.199） |
| D3D11 设备 | 默认适配器 `AMD Radeon(TM) 780M`，驱动类型 `hardware`（无 WARP 回退） |
| 进程 DPI 感知 | Per-Monitor V2（`set_per_monitor_v2()`，design §6） |
| 构建 | `cargo build --release -p visrecall-shell --example overlay_spike` |
| 帧时间工具 | 进程内 `Instant`（QPC），测量相邻 `Present(1,0)` 返回时刻的间隔；**本机未安装 PresentMon**（`where PresentMon` 无结果），未做 PresentMon 交叉验证 |
| 丢帧阈值 | 相邻 Present 间隔 > 20 ms 计一次 |

**本机没有物理 4K 显示器。** 4K 数据来自「合成 4K 窗口」：窗口客户区与 swap chain 都是
3840×2160（`GetClientRect` 实测 3840×2160），与客户区尺寸一致，`DXGI_SCALING_STRETCH`
不发生实际缩放；窗口大于 2560×1440 屏幕的部分被 **DWM 只裁剪、不缩放**（窗口未拉伸、
画面不被重采样），D3D11/D2D 的渲染与呈现负载仍是完整 4K。该组数据**不能**声称
「物理 4K 已验证」。

## D3D11 门禁（Task 10 / design §7.3、P1-R05、P1-R12）

### 测量口径（先读，否则数字会被误读）

1. **帧时间** = 同一窗口相邻两次 `Present(1,0)` 返回时刻的间隔，**含 vsync 等待**，
   与 PresentMon 的「Present 间隔」口径接近但来源不同（进程内 QPC）。
2. **预热 60 帧**：每窗口前 60 帧只计数、不进统计。`presents` 含预热帧，
   `samples = presents - 60`（第 61 帧与第 60 帧之间的间隔已计入）。
3. **`work_avg_ms` 不是 GPU 余量证据。** 它是 `draw_test_frame` 的 CPU 提交耗时
   （D2D 录制 + `EndDraw`，不含 vsync 等待），只说明 CPU 侧提交很轻（0.16–0.35 ms）。
   GPU/合成余量的证据是：30 s 内平均 Present 间隔 ≈6.06 ms 且持续到 165 fps，
   即每帧都跟上了 165 Hz 的 vblank（周期 6.06 ms），而不是「刚够 60 fps」。
4. **drop 阈值 20 ms 在 165 Hz 下偏松**：一个 vblank 是 6.06 ms，漏掉 1 个 vblank 是
   12.12 ms，仍低于 20 ms 而**不会**计入 `drops`。因此 `drops(>20ms)=0` 只说明没有
   ≥20 ms 的长间隔，不能单独证明「无单帧卡顿」；约束单帧卡顿的是 `max_ms`
   （本次三组 max 均 ≤16.67 ms，即至多 2 个 vblank）。
5. **场景 C 的 `SPIKE_RESULT` 只取窗口 0**（输出含 `summary_window=0
   summary_is_aggregate=false`）。两个窗口的帧时间是**各自独立**的数值，不是合计；
   若要合计吞吐，应把两行的 fps 相加（≈330 Present/s），不能把 avg/p95 相加。
6. **无 PresentMon**：本机不可用，全部帧时间来自进程内 QPC。**Task 19 必须**在装有
   PresentMon 的机器上补一次交叉验证（同一场景、同一 30 s 窗口）。
7. **刷新率说明**：`refresh_hz=165` 解释 6.06 ms 就是 vblank 周期；本机没有 60 Hz
   屏，因此「60 Hz 下的 16.7 ms」只能以预算比较，不能作为实测值。

### 判据与结论

Task 10 判据（合成 4K）：

1. 30 s 持续 ≈165 fps，平均 Present 间隔 ≈6.06 ms；
2. P95 ≤ 20 ms，max ≤ 16.67 ms；
3. `drops(>20ms)` = 0；
4. 首帧（进程内 `main` → 第一次成功 `Present`）≤ 150 ms；
5. 双合成 4K 同时 Present 无 D3D 设备/交换链错误。

| 日期 | 环境 | 场景 | 平均 Present 间隔 | P95 | max | drops(>20ms) | 首帧 | 结论 |
|---|---|---|---|---|---|---|---|---|
| 2026-11-04 | 2560×1440@165Hz，AMD 780M，硬件 D3D11 | A. 原生 2560×1440 单窗口 30 s | 6.060 ms（165.01 fps） | 6.251 ms | 11.074 ms | 0 | 116.55 ms | 子项通过 |
| 2026-11-04 | 同上 | B. 合成 4K（3840×2160）单窗口 30 s | 6.061 ms（164.99 fps） | 6.231 ms | 13.184 ms | 0 | 125.91 ms | 子项通过 |
| 2026-11-04 | 同上 | C. 双合成 4K（2×3840×2160）同时 Present 10 s | 6.064 ms / 6.061 ms（165.0 / 165.0 fps，窗口 0 / 1 各自独立） | 6.202 / 6.275 ms | 10.490 / 10.157 ms | 0 / 0 | 131.90 ms | 子项通过（无 D3D 错误） |

**结论措辞（固定）：Task 10 合成 4K 子项通过；design §7.3 完整门禁（物理 4K 显示器 +
抓帧 + 拖动控制点 + PresentMon）仍为 CONDITIONAL，待 Task 11/19。**

即：本表只覆盖「无抓帧负载的 4K 合成 Present」这一段；不得据此写下「门禁通过」，
因为 design §7.3 的完整判据还包含物理 4K 面板、抓帧后的帧上传与拖动控制点交互，
这些在 Task 10 范围内**未验证**。

### 首帧阶段打点（Task 11 预创建策略的依据）

`SPIKE_PHASE` 行；`display_enum_ms` / `first_present_ms` 相对进程起点累计，
其余三项是阶段耗时：

| 场景 | display_enum_ms | window_create_ms | d3d_init_ms | renderer_init_ms | first_present_ms |
|---|---|---|---|---|---|
| A（1×1440p） | 2.43 | 22.87 | 77.31 | 7.30 | 116.55 |
| B（1×合成 4K） | 2.82 | 23.24 | 77.15 | 6.60 | 125.91 |
| C（2×合成 4K） | 2.60 | 27.26 | 74.18 | 9.13 | 131.90 |

读数：`D3dContext::new()`（`D3D11CreateDevice` + DXGI 工厂 + D2D/DWrite 工厂）占首帧的
约 2/3（74–77 ms）；窗口创建约 23–27 ms；交换链 + D2D 渲染器只需 7–9 ms。
若 Task 11 要让「快捷键 → 覆盖层首帧」在 150 ms 内且给抓帧留时间，**预创建
`D3dContext`（进程启动时建好并挂在覆盖层线程）是最有效的单点优化**；窗口创建
（23 ms，主要来自 `CreateWindowExW` + 首次窗口类注册）次之。

### 原始输出

场景 A —— 原生 2560×1440 单窗口：

```bash
cargo run --release -p visrecall-shell --example overlay_spike -- --duration 30
```

```text
SPIKE_DISPLAY index=0 id=\\.\DISPLAY1 bounds=2560x1440+0+0 dpi=144 primary=true refresh_hz=165
SPIKE_ENV displays=1 primary=\\.\DISPLAY1 bounds=2560x1440+0+0 dpi=144 width=2560 height=1440 size_source=native windows=1 duration_s=30 refresh_hz=165 warmup_frames=60 drop_threshold_ms=20
SPIKE_WINDOW_GEOMETRY index=0 display=\\.\DISPLAY1 x=0 y=0 client=2560x1440 planned=2560x1440 duplicate=false
SPIKE_DEVICE driver=hardware adapter=AMD Radeon(TM) 780M
SPIKE_PHASE display_enum_ms=2.43 window_create_ms=22.87 d3d_init_ms=77.31 renderer_init_ms=7.30 first_present_ms=116.55
SPIKE_WINDOW index=0 display=\\.\DISPLAY1 width=2560 height=1440 presents=4921 samples=4861 avg_ms=6.060 p50_ms=6.057 p95_ms=6.251 p99_ms=6.363 max_ms=11.074 drops=0 fps=165.01 work_avg_ms=0.164 present_avg_ms=5.892
SPIKE_RESULT width=2560 height=1440 windows=1 summary_window=0 summary_is_aggregate=false avg_ms=6.060 p50_ms=6.057 p95_ms=6.251 p99_ms=6.363 max_ms=11.074 drops=0 fps=165.01 first_frame_ms=116.55 work_avg_ms=0.164 presents=4921 samples=4861 warmup_frames=60 duration_s=30 driver=hardware
```

场景 B —— 合成 4K 单窗口：

```bash
cargo run --release -p visrecall-shell --example overlay_spike -- --duration 30 --width 3840 --height 2160
```

```text
SPIKE_DISPLAY index=0 id=\\.\DISPLAY1 bounds=2560x1440+0+0 dpi=144 primary=true refresh_hz=165
SPIKE_ENV displays=1 primary=\\.\DISPLAY1 bounds=2560x1440+0+0 dpi=144 width=3840 height=2160 size_source=override windows=1 duration_s=30 refresh_hz=165 warmup_frames=60 drop_threshold_ms=20
SPIKE_WINDOW_GEOMETRY index=0 display=\\.\DISPLAY1 x=0 y=0 client=3840x2160 planned=3840x2160 duplicate=false
SPIKE_DEVICE driver=hardware adapter=AMD Radeon(TM) 780M
SPIKE_PHASE display_enum_ms=2.82 window_create_ms=23.24 d3d_init_ms=77.15 renderer_init_ms=6.60 first_present_ms=125.91
SPIKE_WINDOW index=0 display=\\.\DISPLAY1 width=3840 height=2160 presents=4909 samples=4849 avg_ms=6.061 p50_ms=6.057 p95_ms=6.231 p99_ms=6.364 max_ms=13.184 drops=0 fps=164.99 work_avg_ms=0.354 present_avg_ms=5.702
SPIKE_RESULT width=3840 height=2160 windows=1 summary_window=0 summary_is_aggregate=false avg_ms=6.061 p50_ms=6.057 p95_ms=6.231 p99_ms=6.364 max_ms=13.184 drops=0 fps=164.99 first_frame_ms=125.91 work_avg_ms=0.354 presents=4909 samples=4849 warmup_frames=60 duration_s=30 driver=hardware
```

场景 C —— 双合成 4K 同时 Present（本机只有 1 台显示器，`--windows 2` 用主屏复制第二个
窗口并偏移 48 px，`duplicate=true`）：

```bash
cargo run --release -p visrecall-shell --example overlay_spike -- --duration 10 --width 3840 --height 2160 --windows 2
```

```text
SPIKE_DISPLAY index=0 id=\\.\DISPLAY1 bounds=2560x1440+0+0 dpi=144 primary=true refresh_hz=165
SPIKE_ENV displays=1 primary=\\.\DISPLAY1 bounds=2560x1440+0+0 dpi=144 width=3840 height=2160 size_source=override windows=2 duration_s=10 refresh_hz=165 warmup_frames=60 drop_threshold_ms=20
SPIKE_WINDOW_GEOMETRY index=0 display=\\.\DISPLAY1 x=0 y=0 client=3840x2160 planned=3840x2160 duplicate=false
SPIKE_WINDOW_GEOMETRY index=1 display=\\.\DISPLAY1 x=48 y=48 client=3840x2160 planned=3840x2160 duplicate=true
SPIKE_DEVICE driver=hardware adapter=AMD Radeon(TM) 780M
SPIKE_PHASE display_enum_ms=2.60 window_create_ms=27.26 d3d_init_ms=74.18 renderer_init_ms=9.13 first_present_ms=131.90
SPIKE_WINDOW index=0 display=\\.\DISPLAY1 width=3840 height=2160 presents=1618 samples=1558 avg_ms=6.064 p50_ms=6.061 p95_ms=6.202 p99_ms=6.301 max_ms=10.490 drops=0 fps=164.91 work_avg_ms=0.218 present_avg_ms=5.011
SPIKE_WINDOW index=1 display=\\.\DISPLAY1 width=3840 height=2160 presents=1618 samples=1558 avg_ms=6.061 p50_ms=6.061 p95_ms=6.275 p99_ms=6.443 max_ms=10.157 drops=0 fps=164.99 work_avg_ms=0.295 present_avg_ms=0.532
SPIKE_RESULT width=3840 height=2160 windows=2 summary_window=0 summary_is_aggregate=false avg_ms=6.064 p50_ms=6.061 p95_ms=6.202 p99_ms=6.301 max_ms=10.490 drops=0 fps=164.91 first_frame_ms=131.90 work_avg_ms=0.218 presents=1618 samples=1558 warmup_frames=60 duration_s=10 driver=hardware
```

（`SPIKE_RESULT` 的 avg/p95/max/drops/fps 都是**窗口 0** 的数值，窗口 1 见上一行
`SPIKE_WINDOW index=1`；两者不可相加成「合计帧时间」。）

### 修复前数据（`da35ebf`，仅作追溯与对照，不作为门禁证据）

修复前的报告把 `work_avg_ms` 当成 GPU 余量证据、并写了「门禁通过」，属表述错误；
同一台机器上修复前的三组数值为：

| 场景 | 平均 Present 间隔 | P95 | max | 首帧 | work_avg_ms |
|---|---|---|---|---|---|
| A | 6.067 ms（164.84 fps） | 6.238 ms | 16.031 ms | 106.94 ms | 0.086 |
| B | 6.063 ms（164.94 fps） | 6.222 ms | 16.146 ms | 112.17 ms | 0.206 |
| C | 6.064 / 6.061 ms | 6.195 ms | 11.601 ms | 135.39 ms | 0.212 |

修复前后平均间隔差 <0.01 ms（同一 vblank 周期），属正常抖动；修复后的首帧略长
（116–132 ms vs 107–135 ms）与新增的 `SPIKE_PHASE` 打点、窗口类注册/注销同量级，
不影响 ≤150 ms 的判据。**以修复后的表为准。**

### 交互与清理验证

- `Esc` 退出：向窗口投递 `WM_KEYDOWN`/`VK_ESCAPE` 后进程打印 summary 并退出
  （修复前实测 707 次 Present 后退出，退出码 0；该代码路径本轮未改动）。
- 时长到期退出：本轮 A/B/C 与一次 3 s 短跑均正常打印 summary，退出码 0。
- 关闭窗口（`WM_CLOSE` → `WM_DESTROY`）走同一条 `WM_QUIT` 路径；退出时先释放
  渲染器与设备（`drop`），再 `DestroyWindow`，最后 `UnregisterClassW` 注销窗口类。
- 文字渲染：Direct2D `DrawText`（Microsoft YaHei UI，52 px，灰度抗锯齿）中文
  「VisRecall Overlay Spike · 已冻结」字形正确、无方框缺字（用 `CopyFromScreen` 抓屏
  确认了字形部分）；`BeginDraw`/`DrawText`/`EndDraw` 全程无 HRESULT 错误。
  注意：GDI 抓屏读不到 FLIP 模型的后续帧（图中大面积是桌面旧内容），因此
  「整屏无闪烁」属主观项，留待 Task 19 人眼复验。

## 未验证项（随本文件持续更新）

1. **物理 4K 面板**：本机无 4K 屏，B/C 是合成 4K。
2. **双物理显示器**：本机只有 1 台，`--windows 2` 是主屏复制窗口；默认「每台显示器
   一个窗口」的多显示器路径本轮**未实测**（代码已按 `enumerate_displays()` 遍历实现）。
3. **PresentMon 交叉验证**：本机无 PresentMon，全部为进程内 QPC；Task 19 必须补。
4. **完整首帧链路**：本表首帧不含抓帧/纹理上传/选区，P1-R12 在 Task 11/13/14 后复测。
5. **拖动控制点时的帧时间**：design §7.3 要求「拖动控制点」场景，Task 10 只有每帧
   移动的矩形，未覆盖交互负载。

---

# 阶段 01 退出验收（Task 19，2026-11-05）

本节是阶段 01 的退出验收记录，**不改写**上文 Task 10 的 D3D 门禁数据。所有数字都是
在本机实测的；没有测到的项一律标 `NOT RUN` 并写原因，**不把「代码路径 + 单元测试」
当作实测**。引用其它任务已实测证据时明确标注来源。

## A. 自动化门禁（全部实跑）

分支 `master`，基线提交 `10a477c`（Task 19 补丁：启动清理 tmp/孤儿扫描），Windows 11 22621。

| 命令 | 退出码 | 结果 |
|---|---|---|
| `cargo fmt --all -- --check` | 0 | 无输出 |
| `cargo clippy --workspace --all-targets -- -D warnings` | 0 | `Finished dev profile`，无告警 |
| `cargo test --workspace` | 0 | **322 passed / 0 failed**（20 个测试二进制 + 5 个 doc-test 套件，0 ignored） |
| `pnpm lint` | 0 | 无输出 |
| `pnpm typecheck` | 0 | `tsc --noEmit` 无输出 |
| `pnpm test` | 0 | **4 个文件 / 13 个用例全通过**（ItemCard 8、ItemGrid 2、EmptyState 1、events 2） |
| `pnpm build` | 0 | `86 modules transformed`；`dist/assets/index-BDc_oIcF.js` **266.29 kB**（gzip 82.94 kB）、`index-4cN57Dsb.css` 3.97 kB（gzip 1.32 kB）；1.34 s |

`cargo build --release -p visrecall` 产物 18,302,976 字节（17.5 MiB）。

## B. 测量环境（Task 19 手工矩阵）

| 项目 | 值 |
|---|---|
| 日期 | 2026-11-05 |
| 操作系统 | Windows 11 家庭中文版 10.0.22621，16 逻辑核 |
| 显示器 | **1 台**，2560×1440 @165Hz，缩放 **150%**（DPI 144），`\\.\DISPLAY1` |
| GPU | AMD Radeon(TM) 780M（覆盖层 `hardware` 驱动，无 WARP 回退） |
| 配置隔离 | `USERPROFILE`/`APPDATA`/`LOCALAPPDATA` 全部指向 `%TEMP%\vr19\*`，不触碰真实库 |
| 二进制 | `target/release/visrecall.exe`（`--features tauri/custom-protocol`，内嵌 `dist/`）；对照 `target/debug/visrecall.exe` |
| 输入注入 | PowerShell `mouse_event(MOVE|ABSOLUTE)` / `keybd_event`，进程 PMv2 感知（物理像素） |
| 日志 | `VISRECALL_LOG=debug`，退出时 `tracing` 写到重定向文件 |
| 干扰说明 | 验收期间本机同时存在**其它真实输入/自动化**（外部鼠标两次拖动过悬浮球、并两次「唤醒主窗口」使 60s 销毁计时器作废）。受影响项已重测，下文只记录**受控一次**的结果 |

## C. 阶段 01 验收矩阵

图例：`PASS` 实测通过；`PASS*` 实测通过但口径有限（见备注）；`NOT RUN` 本机条件不足未测；
`REF` 本轮未重测，引用先前任务的实测证据（不重复算通过）。

| # | 场景 | 方法 | 期望 | 结果 | 证据 / 备注 |
|---|---|---|---|---|---|
| 1 | 冷启动 → 托盘 | 临时 profile 冷启动 release | 进程驻留、仅托盘、无主窗口、库自动建好 | **PASS** | 日志 `tray icon created library_ok=true`、`cold start to tray: 136/141/194 ms`（release）、`210/231 ms`（debug）；窗口枚举只有 `VisRecallBall`/`VisRecallOverlay`/`tray_icon_app`，无 `Tauri Window` |
| 2 | 托盘空闲工作集 | 主窗口销毁 60s 后读 `WorkingSet64` | ≤150MB（目标 40–70MB） | **PASS** | release：WS **53.7MB**、私有 **36.9MB**、47 线程、~1000 句柄、`msedgewebview2` **0** 个（启动态 WS 43.0MB / debug 51.6MB） |
| 3 | 快捷键 → 拖拽 → Enter（150%，Ctrl 关吸附） | 热键 `Ctrl+Shift+S`，400,300 → 900,700，Enter | 图与选区一致、PNG/DB/列表正确 | **PASS** | `selection readback complete width=500 height=400 x=400 y=300`；PNG IHDR 500×400、bit depth 8 / color type 6（RGBA）；PNG 魔数正确；DB `status=ready` + `thumbs/<id>.png`；`tmp/` 无残留 |
| 4 | Esc 取消 | 拖拽后 Esc | 无记录 | **PASS** | `overlay session cancelled by user`；`items` 行数不变（2 → 2） |
| 5 | 右键取消 | 拖拽后右键 | 无记录 | **PASS** | 同上，`items` 行数不变 |
| 6 | 选区过小（5×5） | 400,300 → 405,305，Enter | 提示「选区过小」、取消、无记录 | **PASS** | `selection too small notice expired; cancelling session` → `overlay cancelled; own windows restored`；行数不变 |
| 7 | 方向键微调 | 拖 500×400 后按 `→` ×3，Enter | 位置 +1px/次、尺寸不变 | **PASS** | `selection readback complete width=500 height=400 x=403 y=300`（原 x=400） |
| 8 | Ctrl 关闭吸附 | 按住 Ctrl 拖拽 | 选区精确等于手绘矩形 | **PASS\*** | 本次 Ctrl 按住时 1px 不差；**未**构造「贴近窗口边缘、开/关 Ctrl 对比」的对照，吸附本身由 Task 12 单测与 Task 13 实测覆盖 |
| 9 | 悬浮球位置保存/重启恢复 | 拖动球 → 读 `config.json` → 重启 | 位置持久化并恢复 | **PASS** | 拖动后 `positions.\\.\DISPLAY1 = [0.8203125, 0.625]`；重启日志 `floating ball window created size=48 x=2076 y=876`（= 2100/2560、900/1440 归一化反算） |
| 10 | 悬浮球单击捕获 | 单击球心 (2100,900) | 触发捕获、球抓帧期间隐藏 | **PASS** | `capture requested trigger=BallClick` → `capture starting trigger=BallClick` → `frozen overlay shown show_total_ms=59.3` |
| 11 | 关闭悬浮球后热键可用 | `ball.enabled=false` 重启 → 热键捕获 | 球不可见、捕获正常、球不被误开 | **PASS** | `VisRecallBall:visible=False`；`capture starting trigger=Hotkey` → overlay 正常 → Esc 取消；全程无 `floating ball visibility changed visible=true` |
| 12 | 第二实例 | 再次启动 exe（4 次） | 第二进程退出并唤醒主窗口 | **PASS** | `SECOND_EXIT=0`、`visrecall` 进程数保持 1；首实例 `second instance detected; showing main window` + `main window created` |
| 13 | 强杀恢复 + 启动清理 | 库内放 `tmp/stale.tmp` + `tmp/another.tmp`，`Stop-Process -Force` 后重启 | tmp 被清空、已入库素材可读、库无损坏 | **PASS** | `startup cleanup finished tmp_removed=2 orphans=0`；`tmp/` 空；`PRAGMA integrity_check` = `ok`；原 2 条素材仍 `ready` 且缩略图在 |
| 14 | 空库 EmptyState | 首次启动空库 | 显示引导文案 | **REF** | Task 18 实测（「还没有素材 / 按 Ctrl+Shift+S 或点击悬浮球开始捕获」，PrintWindow 截图确认）+ `EmptyState.test.tsx`；本轮空库实例被前台 Chrome 抢占，未取到新截图 |
| 15 | 列表自动刷新 | 主窗口打开时捕获一条 | 新卡片置顶，无需手动刷新 | **PASS** | 截图见本轮「后捕获」快照：新卡片「目记 VisRecall / visrecall.exe / 刚刚」出现在第一格；工具栏计数由 102 回到 50（`resetQueries` 语义，Task 18 Fix 1） |
| 16 | 状态 parsing → ready | 捕获后看卡片徽标 | 先「解析中」后「正常」 | **PASS\*** | 本轮只观察到终态「正常」：小图缩略图 8ms 完成（`persisted` 37.413 → `thumbnail generated` 37.421），中间态未在界面上截到；事件顺序由 Task 17/18 单测固定 |
| 17 | 100 条列表 | 临时库插入 100 条 metadata（复制真实 PNG/缩略图）→ 共 104 条 | 网格渲染、排序 DESC、分页、滚动 | **PASS\*** | 「已加载 102 条」（截图时刻）→ 滚轮 30 格到第 34–43 条；卡片顺序按 `captured_at` 降序（「38 分钟前」→「47 分钟前」）；缩略图经 `visrecall://thumb` 全部加载；**滚动帧率未量化** |
| 18 | 提交到列表可见 | 捕获后在主窗口截图 | ≤3s | **PASS** | release：`persisted` 07:30:37.413 → 截图约 07:30:38 时卡片已可见（≤1s）；Task 17 记录缩略图链路 480ms（debug） |
| 19 | 关闭主窗口 60s → WebView2 退出 | `WM_CLOSE` 后 5s 轮询 75s | 60s 销毁窗口、WebView2 进程退出、进程常驻 | **PASS** | `hidden main window destroyed after 60s`（关闭后 60.0s）；`Tauri Window` 消失；`msedgewebview2`（`com.visrecall.desktop`）**6 → 0**；`visrecall` 仍驻留、球与托盘在（前两次尝试被外部「唤醒主窗口」作废计时器，见 B 表干扰说明） |
| 20 | 窗口点击采用窗口 | 在窗口上按下-抬起无位移 | 选中该窗口 bounds | **REF** | Task 13 实测（微信小窗 → `EVENT committed 15x90+876+736` 等于窗口 bounds）；本轮首次（受干扰）捕获也表现为「采用 Chrome 窗口 bounds」 |
| 21 | 滚轮层级下钻 | 浏览器内容区滚轮 | 高亮链下钻/上移 | **REF** | Task 13：滚轮事件正常处理、`hover window changed` 可见；**未**单独断言「切到第 N 个候选」。本轮未复测 |
| 22 | 拖放（真实 OLE 管线 12 场景） | 真实剪贴板 + `DoDragDrop` + 真实 `RegisterDragDrop` | 文本/PNG/txt/md/混合/CF_DIB/URL/超限/PDF/文件夹/取消 | **REF** | Task 16：12 个场景全部符合预期（超限 10001 字与 21 文件在 `DragEnter` 即拒绝 `effect=0`；21MB 图 Drop 后 `INVALID_INPUT`；PDF/文件夹 `DROP_REJECTED`；Esc 取消不落盘） |
| 23 | 物理四来源拖拽 | Chrome 文字 / Word 文本 / 资源管理器图片 / 微信图片 | 正确入库 | **NOT RUN** | 需真实物理拖拽；本机拖拽源应用与 OLE 会话无法在脚本中稳定复现（Task 16 已用真实 OLE 管线替代，未用物理鼠标） |
| 24 | 单屏 100% / 125% / 200% | 改缩放后重跑 | 无偏移 | **NOT RUN** | 修改显示缩放需注销/重启会话，会打断本轮验收；本轮只覆盖 150% |
| 25 | 双屏同 DPI / 混合 DPI / 负坐标 | 跨屏框选 | 图覆盖跨屏、边界不偏移 | **NOT RUN** | 本机只有 1 台显示器；相关逻辑（`compose_bgra` 拼合、`owned_center` 临空边、屏缝去重、负原点）只有 Task 13 单测 |
| 26 | 物理 4K 面板 | 4K 屏上重跑 | 帧时间/截图正确 | **NOT RUN** | 本机无 4K 屏；Task 10 的 4K 是**合成 4K 窗口**（DWM 只裁剪不缩放），不能替代 |
| 27 | DRM / 独占全屏 | 播放 DRM 视频后捕获 | 丢弃并提示，不产生记录 | **NOT RUN** | 未构造真实 DRM 窗口；`is_blank_frame` → `DRM_PROTECTED` 只有 Task 14 单测（假全黑帧） |
| 28 | 提权窗口（UIPI） | 以管理员身份运行的目标窗口 | 明确失败或可捕获 | **NOT RUN** | 需 UAC 提权会话；`RegisterHotKey`/抓帧/拖放在高完整性级别下的行为未验证 |
| 29 | ARM64 | ARM64 设备构建运行 | 可用 | **NOT RUN** | 阶段 04 范围；本机 x64 |
| 30 | 30 分钟连续使用 + ≥20 次真实捕获 | 长时间使用 | 无崩溃、≥20 次捕获 | **NOT RUN** | 本轮共 6 次成功捕获 + 2 次取消 + 1 次过小提交；进程累计运行约 21 分钟（含多次重启）**未崩溃**，但时长与次数都不足退出条件（requirements §5.2） |

## D. 性能记录

| 指标 | 目标 | 实测 | 构建/口径 | 来源 |
|---|---|---|---|---|
| 捕获入口响应（快捷键 → 覆盖层首帧） | ≤150 ms | **104 ms**（主窗口可见，含隐藏主窗口）；**51 / 117 / 118 / 135 ms**（托盘态、球单击、球关闭态） | release（首行）/debug，日志时间戳差 | 本轮（`hotkey pressed`/`capture starting` → `frozen overlay shown`） |
| 覆盖层内部 show → first Present | — | **6.9–49.1 ms**（`show_to_first_present_ms`） | debug/release | 本轮 `frozen overlay shown` |
| 覆盖层绘制 CPU 提交（一次重绘所有窗口 `draw_scene`） | — | avg **0.53–0.87 ms**，max **0.78–12.9 ms** | debug/release | 本轮 `OVERLAY_PERF`（Task 13 同口径 avg 0.46–0.56 / max 1.94–11.73 ms） |
| 鼠标输入 → Present | 高亮跟随 ≤50 ms | avg **1.79–4.42 ms**，max **3.4–17.8 ms**（1 个样本 17.84 ms，超出 16.7 ms 单帧预算但远低于 50 ms） | debug/release | 本轮 `OVERLAY_PERF`；Task 13 曾出现 15.64 ms 峰值 |
| 选区交互帧率 | 60 fps | 未用 PresentMon/帧率工具测量；`draw` + `Present` 耗时在 165 Hz 单帧预算内（见上两行） | — | 本轮 + Task 13 |
| 提交到列表可见（≤100 条库） | ≤3 s | **≤1 s**（缩略图 `persisted`+8 ms 完成，截图已见卡片）；Task 17 记录 **480 ms** | release（本轮）/debug（Task 17） | 本轮 + Task 17 |
| 托盘常驻内存（主窗口销毁后，无 WebView2） | ≤150 MB（目标 40–70 MB） | WS **53.7 MB** / 私有 **36.9 MB** | release | 本轮 |
| 托盘空闲 CPU | <1% | **60 s 采样内 CPU 时间增量 0.000 s**（16 逻辑核 → 0%）；**不是**要求的 10 分钟 PerfMon 门禁 | release | 本轮（短采样） |
| 4K 双屏抓帧工作集增量 | ≤100 MB | **NOT RUN**（无物理 4K / 无双屏） | — | — |
| 冷启动到托盘 | ≤2 s | **136–194 ms**（release，热文件系统）；210/231 ms（debug）；1093 ms 为 debug + 同时跑 release 编译时的抖动 | release/debug | 本轮（Task 5 曾测 44–56 ms，仅到托盘创建；本口径含覆盖层线程初始化） |
| 4K 合成 Present 帧时间 | design §7.3 | 见上文 Task 10 表：avg ≈6.06 ms、P95 ≤6.28 ms、max ≤13.18 ms、drops=0、首帧 116–132 ms | release，合成 4K，165 Hz | Task 10（**不是**物理 4K，**无** PresentMon） |

结论口径（沿用 Task 10 的措辞）：**Task 10 合成 4K 子项通过；design §7.3 完整门禁
（物理 4K 面板 + 抓帧 + 拖动控制点 + PresentMon 交叉验证）仍为 CONDITIONAL**。

## E. 已知限制（阶段 01）

1. **DRM / 独占全屏**：全黑帧判定与 `DRM_PROTECTED` 只有单测（假全黑帧）；未用真实 DRM
   视频、未在独占全屏游戏/播放器上实测。
2. **提权窗口（UIPI）**：以管理员运行的目标窗口在高完整性级别下，捕获、`RegisterHotKey`
   与 OLE 拖放的行为未验证；拖放目标对提权源进程同样受 UIPI 限制。
3. **UIPI 拖放**：跨完整性级别拖放未验证（与上一条同源）。
4. **跨屏间隙填 0**：`compose_bgra` 对未覆盖像素填 0（透明黑），只有单测；真实多屏
   间隙/负坐标下的合成结果未目视确认。
5. **缩略图是 PNG，不是 WebP**：`thumbs/<id>.png`，480 px 长边；库体积与解码开销未评估。
6. **实时模式在阶段 01 降级**：抓帧失败重试一次后直接 `CAPTURE_FAILED`，透明覆盖层 +
   提交时现场抓取的降级路径**未实现**（requirements P1-R03 已确认降级，阶段 02 再评估）。
7. **物理 4K / PresentMon 未验证**：4K 数据来自合成窗口；所有帧时间来自进程内 QPC。
8. **多屏 / 混合 DPI / 负坐标未验证**：本机单屏；控制点临空边、标签归属、跨屏拼合、
   `Present(1,0)/(0,0)` 分支只有单测。
9. **Word / 微信 / Chrome / 资源管理器的物理拖放未验证**：现有证据是真实 OLE 管线
   （真实剪贴板格式 + 真实 `DoDragDrop` + 真实 `RegisterDragDrop`），不是物理鼠标。
10. **ARM64 未验证**（阶段 04）。
11. **托盘内存 / 空闲 CPU 未按门禁口径完整测量**：内存是一次快照；CPU 是 60 s 短采样，
    不是 10 分钟 PerfMon。
12. **100 条列表滚动未完整实测**：只做了「渲染 + 滚轮滚动 + 分页」的视觉检查，没有帧率
    或掉帧量化；超过 50 条的「加载更多」交互由实现与截图确认，未做反复翻页压测。
13. **真实库中可能残留 Task 18 手工验证写入的 1 条素材**（`Documents\VisRecall`，来源
    `Code.exe`）；本轮验收全部在临时 profile 中进行，未清理该条，用户可自行删除。
14. **打包产物未验收**：阶段 01 没有执行 `pnpm tauri build`。实测发现用
    `cargo build --release -p visrecall`（未启用 `tauri/custom-protocol`）生成的二进制
    仍指向 `devUrl`（主窗口显示「localhost 拒绝连接」），只有
    `--features tauri/custom-protocol` 才会内嵌 `dist/`。发布路径需要在阶段 02 的打包任务
    里确认 `src-tauri/Cargo.toml` 的 feature 配置与产物验收（本任务不改代码）。
15. **60 s 销毁与外部唤醒**：窗口被外部唤醒（托盘点击/第二实例）时，销毁计时器按设计
    作废（`destroy timer superseded; main window kept`）。验收期间出现过两次，属正常行为，
    但意味着「60 s 销毁」只能在无并发唤醒时测得。
16. **状态 `parsing` 中间态**：缩略图 8 ms 内完成，界面上一闪而过，未截到「解析中」；
    由 Task 17/18 单测固定事件顺序与徽标文案。

## F. 结论

**阶段 01 有条件完成（2026-11-05）。**

- 自动化门禁全部通过：`cargo fmt` / `clippy -D warnings` / `cargo test --workspace`
  （322 passed）/ `pnpm lint` / `pnpm typecheck` / `pnpm test`（13 passed）/ `pnpm build`。
- 单屏 150% 主链路实测通过：冷启动 → 托盘 → 快捷键/悬浮球 → 冻结帧 → 选区（拖拽、
  方向键、最小选区、Esc/右键）→ 提交 → PNG + 入库 → 缩略图 → 列表置顶可见；托盘常驻
  内存 53.7 MB、60 s 后 WebView2 进程归零、强杀后临时文件清理与库完整性正常。
- **未验证、必须在阶段 02 启动前补齐**：物理 4K 面板与 PresentMon 交叉验证、双屏与
  混合 DPI（含负坐标）、物理 OLE 四来源拖放、DRM/独占全屏、提权窗口与 UIPI 拖放、
  100% 缩放下的一致性、30 分钟连续使用且 ≥20 次真实捕获、打包产物（`pnpm tauri build`）。
  详见上文 E 与 `docs/specs/v0.1/README.md` 的「阶段 02 启动前必须补齐」。
