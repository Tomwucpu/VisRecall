# 阶段 01 验收记录（01-capture-loop）

本文件记录手工验收与性能门禁的**实测**结果。逐任务的自动化测试见各 crate 的
`cargo test`；本文件只放需要真实机器测量的项目。

> Task 10 建立本文件并写入「D3D11 门禁」；其余验收矩阵（P1-R01–P1-R12 全量、
> 30 分钟连续使用、双屏/混合 DPI 手工矩阵）由 Task 19 补齐。未填写的项一律视为
> **未验证**，不得当作通过。

## 测量环境

| 项目 | 值 |
|---|---|
| 日期 | 2026-11-04 |
| 操作系统 | Windows 11 家庭中文版 10.0.22621（Build 22621） |
| 物理显示器 | 1 台，2560×1440 @ 165Hz，缩放 150%（DPI 144），设备名 `\\.\DISPLAY1` |
| GPU（显示输出） | AMD Radeon(TM) 780M（驱动 32.0.11026.1） |
| GPU（独显，未用于覆盖层） | NVIDIA GeForce RTX 4060 Laptop GPU（驱动 32.0.15.6636） |
| GPU（虚拟适配器） | GameViewer Virtual Display Adapter（驱动 15.6.5.199） |
| D3D11 设备 | 默认适配器 `AMD Radeon(TM) 780M`，驱动类型 `hardware`（无 WARP 回退） |
| 进程 DPI 感知 | Per-Monitor V2（`set_per_monitor_v2()`，design §6） |
| 构建 | `cargo build --release -p visrecall-shell --example overlay_spike` |
| 帧时间工具 | 进程内 `Instant`（QPC）测量相邻 `Present(1,0)` 返回间隔；**本机未安装 PresentMon**（`where PresentMon` 无结果），因此未做 PresentMon 交叉验证 |
| 丢帧阈值 | 相邻 Present 间隔 > 20 ms 计一次 |

**本机没有物理 4K 显示器。** 4K 数据来自「合成 4K 窗口」：窗口与 swap chain 都是
3840×2160（`GetClientRect` 实测 3840×2160），超出 2560×1440 屏幕的部分被 DWM 裁剪，
D3D11/D2D 的渲染与呈现负载仍是完整 4K。该组数据**不能**声称「物理 4K 已验证」。

## D3D11 门禁（Task 10 / design §7.3、P1-R05、P1-R12）

门禁判据（合成 4K）：

1. 平均帧时间 ≤ 16.7 ms；
2. P95 帧时间 ≤ 20 ms；
3. 首帧（进程内 `main` 开始 → 第一次成功 `Present`） ≤ 150 ms；
4. 双合成 4K 同时 Present 无 D3D 设备/交换链错误。

| 日期 | 环境 | 场景 | 平均帧时间 | P95 | max | 丢帧 | 首帧 | 结论 |
|---|---|---|---|---|---|---|---|---|
| 2026-11-04 | 2560×1440@165Hz，AMD 780M，硬件 D3D11 | A. 原生 2560×1440 单窗口 30s | 6.067 ms（164.8 fps） | 6.238 ms | 16.031 ms | 0 | 106.94 ms | 通过 |
| 2026-11-04 | 同上 | B. 合成 4K（3840×2160）单窗口 30s | 6.063 ms（164.9 fps） | 6.222 ms | 16.146 ms | 0 | 112.17 ms | 通过 |
| 2026-11-04 | 同上 | C. 双合成 4K（2×3840×2160）同时 Present 10s | 6.064 ms / 6.061 ms（各 164.9 fps） | 6.195 ms | 11.601 ms | 0 | 135.39 ms | 通过（无 D3D 错误） |

**门禁结论：合成 4K 通过**（平均 6.06 ms ≤ 16.7 ms，P95 6.2 ms ≤ 20 ms，首帧 112 ms ≤ 150 ms，
双 4K 无 D3D 设备/交换链错误）。**真实 4K 显示器待 Task 19 在有物理 4K 屏的机器上复验。**

### 原始输出

场景 A —— 原生 2560×1440 单窗口：

```bash
cargo run --release -p visrecall-shell --example overlay_spike -- --duration 30
```

```text
SPIKE_ENV display=\\.\DISPLAY1 bounds=2560x1440+0+0 dpi=144 width=2560 height=1440 windows=1 duration_s=30
SPIKE_WINDOW_GEOMETRY index=0 x=0 y=0 client=2560x1440
SPIKE_DEVICE driver=hardware adapter=AMD Radeon(TM) 780M
SPIKE_WINDOW index=0 width=2560 height=1440 presents=4920 samples=4860 avg_ms=6.067 p50_ms=6.058 p95_ms=6.238 p99_ms=6.380 max_ms=16.031 drops=0 fps=164.84 work_avg_ms=0.086 present_avg_ms=5.950
SPIKE_RESULT width=2560 height=1440 windows=1 avg_ms=6.067 p50_ms=6.058 p95_ms=6.238 p99_ms=6.380 max_ms=16.031 drops=0 fps=164.84 first_frame_ms=106.94 work_avg_ms=0.086 presents=4920 samples=4860 duration_s=30 driver=hardware
```

场景 B —— 合成 4K 单窗口：

```bash
cargo run --release -p visrecall-shell --example overlay_spike -- --duration 30 --width 3840 --height 2160
```

```text
SPIKE_ENV display=\\.\DISPLAY1 bounds=2560x1440+0+0 dpi=144 width=3840 height=2160 windows=1 duration_s=30
SPIKE_WINDOW_GEOMETRY index=0 x=0 y=0 client=3840x2160
SPIKE_DEVICE driver=hardware adapter=AMD Radeon(TM) 780M
SPIKE_WINDOW index=0 width=3840 height=2160 presents=4160 samples=4100 avg_ms=6.063 p50_ms=6.059 p95_ms=6.222 p99_ms=6.361 max_ms=16.146 drops=0 fps=164.94 work_avg_ms=0.206 present_avg_ms=5.832
SPIKE_RESULT width=3840 height=2160 windows=1 avg_ms=6.063 p50_ms=6.059 p95_ms=6.222 p99_ms=6.361 max_ms=16.146 drops=0 fps=164.94 first_frame_ms=112.17 work_avg_ms=0.206 presents=4160 samples=4100 duration_s=30 driver=hardware
```

场景 C —— 双合成 4K 同时 Present（两个窗口都落在同一台物理屏上，第二个窗口偏移 48 px）：

```bash
cargo run --release -p visrecall-shell --example overlay_spike -- --duration 10 --width 3840 --height 2160 --windows 2
```

```text
SPIKE_ENV display=\\.\DISPLAY1 bounds=2560x1440+0+0 dpi=144 width=3840 height=2160 windows=2 duration_s=10
SPIKE_WINDOW_GEOMETRY index=0 x=0 y=0 client=3840x2160
SPIKE_WINDOW_GEOMETRY index=1 x=48 y=48 client=3840x2160
SPIKE_DEVICE driver=hardware adapter=AMD Radeon(TM) 780M
SPIKE_WINDOW index=0 width=3840 height=2160 presents=1621 samples=1561 avg_ms=6.064 p50_ms=6.059 p95_ms=6.195 p99_ms=6.412 max_ms=11.601 drops=0 fps=164.90 work_avg_ms=0.212 present_avg_ms=4.960
SPIKE_WINDOW index=1 width=3840 height=2160 presents=1621 samples=1561 avg_ms=6.061 p50_ms=6.059 p95_ms=6.243 p99_ms=6.411 max_ms=10.669 drops=0 fps=164.99 work_avg_ms=0.292 present_avg_ms=0.582
SPIKE_RESULT width=3840 height=2160 windows=2 avg_ms=6.064 p50_ms=6.059 p95_ms=6.195 p99_ms=6.412 max_ms=11.601 drops=0 fps=164.90 first_frame_ms=135.39 work_avg_ms=0.212 presents=1621 samples=1561 duration_s=10 driver=hardware
```

### 读数说明（重要）

- **刷新率是 165 Hz，不是 60 Hz。** 平均 6.06 ms 就是 165 Hz 的垂直同步周期，说明
  渲染循环每一帧都跟上了 vblank，而不是「刚好 60fps」。`work_avg_ms`（`draw_test_frame`
  的 CPU 提交耗时，不含 vsync 等待）为 0.09–0.31 ms，远低于 16.7 ms 的 60fps 预算，
  因此 4K 下仍有大量余量；但本表**不能**直接给出「60Hz 屏上的 16.7 ms 实测值」。
- **4K 是合成 4K**：窗口客户区与 swap chain 均为 3840×2160，DWM 把它缩/裁到 2560×1440
  的屏上；D3D11/D2D 的绘制与 Present 负载是真实 4K，但物理 4K 面板的合成与扫描输出
  未经验证。
- **场景 C 不是双物理屏**：两个 4K 窗口都落在同一台显示器上，每个窗口各 ~165 fps
  （合计 ~330 次 Present/s，约 2.7 Gpx/s）。它证明「两个 4K 交换链 + 一个 D3D11 设备」
  不会产生设备/交换链错误，**不**覆盖两台物理 4K 显示器的 DWM 合成路径。
- 表中「首帧」只包含 D3D/D2D 初始化与第一帧 Present，**不含**抓帧与纹理上传；P1-R12 的
  「快捷键 → 覆盖层首帧 ≤150 ms」还要在 Task 11/13/14 的完整链路上复测。
- 本机无 PresentMon，帧时间来自进程内 QPC；Task 19 在有 PresentMon 的机器上可交叉验证。

### 交互与清理验证

- `Esc` 退出：向窗口投递 `WM_KEYDOWN`/`VK_ESCAPE` 后进程打印 summary 并退出
  （实测 707 次 Present 后退出，退出码 0）。
- 时长到期退出：30 s 场景正常打印 summary 并退出。
- 关闭窗口（`WM_CLOSE` → `WM_DESTROY`）走同一条 `WM_QUIT` 路径后清理窗口与 COM。
- 文字渲染：Direct2D `DrawText`（Microsoft YaHei UI，52 px，灰度抗锯齿）中文
  「VisRecall Overlay Spike · 已冻结」字形正确、无方框缺字（用 `CopyFromScreen` 抓屏
  确认了字形部分）；`BeginDraw`/`DrawText`/`EndDraw` 全程无 HRESULT 错误。
  注意：GDI 抓屏读不到 FLIP 模型的后续帧（图中大面积是桌面旧内容），因此
  「整屏无闪烁」属主观项，留待 Task 19 人眼复验。
