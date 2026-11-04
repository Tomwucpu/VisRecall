# 阶段 01 验收记录（01-capture-loop）

本文件记录手工验收与性能门禁的**实测**结果。逐任务的自动化测试见各 crate 的
`cargo test`；本文件只放需要真实机器测量的项目。

> Task 10 建立本文件并写入「D3D11 门禁」；其余验收矩阵（P1-R01–P1-R12 全量、
> 30 分钟连续使用、双屏/混合 DPI 手工矩阵）由 Task 19 补齐。未填写的项一律视为
> **未验证**，不得当作通过。

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
