# 阶段 03：AI 与检索 · 验收记录

> 状态：**待执行（模板，2026-11-08 建）**。本文件在 Task 1（T0 门禁）与 Task 16（阶段验收）中回填；执行规则沿用阶段 02 口径。
> 口径：每一项必须有**结论 + 证据**。结论只能是「通过 / 未通过 / 未验证（环境限制）」；「未验证」必须写原因与替代证据，不允许留空或静默勾选。决策项（无代码产物）结论为「通过（决策项）」。
> 证据形式：命令输出片段、日志片段、测试用例名（`cargo test` / `pnpm test`）、评测报告路径、数据库查询结果、截图路径。涉及 API Key 的证据**必须脱敏**。

## 0. 环境与产物

| 项 | 值 |
|---|---|
| 日期 | 待填 |
| 机器 / CPU / 内存 | 待填 |
| Windows 版本 / 显示配置 | 待填 |
| 库路径与素材条数 | 待填（验收库 = §0.1 的 20 条） |
| `cargo --version` / `node --version` / `pnpm --version` | 待填 |
| 模型文件（manifest sha256 核对） | 待填：PP-OCR det/rec、bge int8、tokenizer.json 的版本与校验结果 |
| release 二进制路径与构建时间 | 待填 |
| commit | 待填 |

## 0.1 测试数据构成（固定 20 条）

沿用阶段 02 口径（构成与阶段 02 相同的 14 捕获 + 6 拖入基底），本阶段追加索引相关处置：

| 类别 | 条数 | 覆盖点 |
|---|---|---|
| 图片 | 14 | 1080p 全屏 4（含 1 条 4K）、窗口截图 4、小区域 2、拖入 JPEG/长图/GIF/WebP 各 1；其中 ≥2 张**无文字纯色图**（OCR 空结果路径）、≥2 张**含公式截图**（has_formula） |
| 文本 | 4 | ≥20000 字符 1（FTS 全量与嵌入截断）、含 emoji 1、短文本 1、带 BOM 1 |
| 链接 | 2 | 标题抓取成功/失败各 1 |
| **合计** | **20** | 14 捕获 + 6 拖入 |

附加处置：

| 处置 | 数量 | 验证点 |
|---|---|---|
| 标签 | ≥3 个（含 1 个自动标签被删除后重解析，验证不回贴） | suppressions |
| 备注 | 2 条 | note 触发 re-index |
| 缺失 FTS 行 | 1 条（手工删） | 启动自检修复 |
| 断网捕获 | 2 条 | OCR 摘录占位 + pending |
| 同画面二次捕获 | 1 条 | content_hash insight 复用（VLM 只调一次） |

评测集：50 条真实查询 + 人工标注（分桶 15/15/15 起，私有文件本地保存，不入库）；标注口径见 `docs/eval/queries.template.jsonl`。

## 1. T0 启动门禁（Task 1 回填；不通过不进入 Task 2+）

> **2026-11-08 执行结论（Task 1）**：门禁 2/3/5 通过；门禁 1 的**延迟与内存达标、字符准确率未验证**
> ——本机只有 5 张真实截图且无人工标注，`100–200 张`口径与 `P3-R01` 的 20 张抽样口径都缺素材。
> 按 2026-11-08 决策（用户确认）先以「未验证（环境限制）」登记并继续推进实现，**素材到位后必须
> 补测后在 §2 P3-R01 与本节回填**，不得默认通过。门禁 4（vec0）按 design §1.2 搁置。

| # | 验证项 | 通过标准 | 结论 | 证据 |
|---|---|---|---|---|
| 1 | PP-OCR ONNX 主路径 | 100–200 张真实截图字符准确率 ≥97%；典型截图 P95 ≤1.5s；4K ≤4s | 延迟/内存**通过**；字符准确率**未验证（环境限制）** | 见 §1.1 |
| 2 | bge-small-zh int8（ort） | 单条 <30ms（P95）；内存增量 ≤60MB；量化排序不劣化 | **通过**（内存 +56.4MB，贴线） | 见 §1.2 |
| 3 | jieba + FTS5 中文检索 | bundled FTS5 可用；20 条素材 10 条真实中文查询字面召回可用、snippet 正常 | **通过** | 见 §1.3 |
| 4 | （可选）vec0 静态链接 | 编译链接成功且 KNN 与暴力一致 | 未验证（按 design §1.2 搁置） | 不进依赖图、不建 vec0 表；主路径为 int8 暴力余弦 |
| 5 | ort ARM64 交叉 | `cargo check --target aarch64-pc-windows-msvc` 通过 | **通过** | 见 §1.4 |

### 1.1 PP-OCR（门禁 1）

命令：
`cargo run --release -p visrecall-ai --example ocr_spike -- --images <dir> [--out <dir>]`

| 项 | 实测 | 门禁 | 结论 |
|---|---|---|---|
| 引擎加载 | 319–522 ms（det + rec，int8 无关、FP32 权重 4.7MB + 10.9MB） | — | — |
| 典型截图 2560×1440（3 张） | p50 301 ms、**P95 798 ms**、max 798 ms | 典型 P95 ≤1.5 s | 通过 |
| 超长截图 2868×8000（降采样到 4096） | **1856 ms** | 4K（长边 ≤4096）≤4 s | 通过 |
| 模型常驻内存 | 进程工作集 3.8 MB → 50.7 MB（**+46.9 MB**：onnxruntime.dll + 两个模型） | — | — |
| 首次识别内存峰值 | 50.7 MB → 138.6 MB（**+87.9 MB**，首张即到顶，之后每张 ≈ +0.2 MB） | 托盘硬上限 150 MB（全局） | **风险项**，见下 |
| 端到端冒烟 | 5 张真实截图（1080p 浏览器页、游戏手柄工具页 ×2、1024 图标、2868×8000 简历）全部返回文本，0 报错，无空结果 | — | 通过 |
| 字符准确率 | **未验证（环境限制）**：本机仅 5 张真实截图、无人工标注 | ≥97%（100–200 张） | 未验证 |

内存风险说明（登记为待收口）：首张截图的 +87.9 MB 来自 DBNet 首层在 960×544 尺度上的 FP32
中间张量（32×544×960×4 B ≈ 66 MB）＋ ORT arena，与线程数无关（`INTRA_THREADS` 1 vs 2 实测差 <1 MB）。
叠加嵌入模型常驻 +56 MB 后进程工作集约 190 MB，**超过 150 MB 硬上限**。收口方案（按序）：
① DET 长边 960 → 736/640 后重测字符准确率（准确率不达标则不动）；② OCR session 空闲卸载
（阶段 04 内存优化一并做）。**在字符准确率门禁可测之前不调 DET 分辨率**，避免用未验证的质量换内存。

OCR 后处理与 PaddleOCR 官方实现的差异（已在 design §1.2 登记）：轴对齐框近似 minAreaRect、
unclip 用面积/周长外扩、无 cls 方向分类、长边 >4096 降采样。

### 1.2 bge-small-zh-v1.5 int8（门禁 2）

命令：`cargo run --release -p visrecall-ai --example embed_spike`

| 项 | 实测 | 门禁 | 结论 |
|---|---|---|---|
| 模型加载 | 145 ms | — | — |
| 常驻内存增量 | 3.2 MB → 59.6 MB（**+56.4 MB**） | ≤60 MB | 通过（余量 3.6 MB，贴线） |
| 维度 | 512 | 512 | 通过 |
| 确定性 | 同文本两次向量 max\|a−b\| = 0.0 | 一致 | 通过 |
| 单条延迟（50 条中文，含首次） | mean 1.0 ms、p50 1.0 ms、**P95 1.4 ms**、max 1.5 ms | P95 <30 ms | 通过 |
| 语义 sanity | 10/10 组「相关对 cos > 不相关对 cos」；最小 margin +0.162（报错截图组） | 全部通过 | 通过 |
| 查询缓存 | cold 1.0 ms → warm 0.001 ms | 命中不重算 | 通过 |

> 说明：门禁原文写的是「量化排序不劣化（Spearman >0.99）」，T0 用**语义 sanity（相关/不相关对）**
> 作为等价判定；Spearman 一致性属于 int8 量化纯函数测试（`domain::vector_math`，Task 4 落地）。
> 样本仅 50 条文本、结论不外推。

### 1.3 jieba + FTS5（门禁 3）

命令：`cargo test -p visrecall-library --test fts_spike`（6 passed）

| 用例 | 覆盖 | 结果 |
|---|---|---|
| `fts5_is_available_in_bundled_sqlite` | rusqlite bundled 的 FTS5 建表/写入/MATCH | 通过（`sqlite_version()` 已记录） |
| `ten_chinese_queries_recall_expected_documents` | 20 条中文素材 × 10 条真实查询（2 字词、4 字词、专有名词、报错码 `502`、中英混排 `git 提交`） | 10/10 召回目标 |
| `snippet_marks_the_matched_terms` | `snippet(-1,'«','»','…',16)` 命中标记 | 通过 |
| `bm25_orders_the_more_relevant_document_first` | `ORDER BY bm25()` 排序 | 通过 |
| `delete_removes_the_row_from_the_index` | 显式 DELETE 后计数 -1 | 通过 |
| `emoji_and_fullwidth_queries_do_not_error` | emoji / 全角标点 / 引号查询不崩 | 通过 |

分词口径：写入侧与查询侧都走同一个 `jieba.cut(text, false)`，token 以空格连接后再交给 FTS5
`unicode61 remove_diacritics 2`；查询表达式把每个 token 加双引号再 OR（避免 FTS5 语法字符问题）。

**Task 3 落地时的两处细化**（生产分词器 `crates/search/src/tokenizer.rs`，spike 用的是裸 `cut`）：
1. **内置领域词表**：jieba 默认词典把 `营收/高亮/看板/漏斗图/饼图/大盘/置信度` 切成单字
   （实测 `营收 → 营 + 收`），单字 token 几乎命中所有素材；改为启动时 `add_word` 固化
   30 个截图高频复合词。刻意**不加** `发票号` 这类「更长词」——否则查「发票」会漏。
2. **索引侧细化、查询侧粗化**：索引用 `cut_for_search`（`漏斗图 → 漏斗 漏斗图 图`），查询用
   `cut`，因此任一切分粒度都能命中，避免两侧粒度不一致导致的漏召回。
3. 转义统一走「每个词包成 FTS5 字符串字面量」（内部 `"` → `""`），不维护危险字符黑名单；
   引号内内容按 `AND`（必命中）语义，见 §1.3 上方与 design §1.2。

### 1.4 ort ARM64 交叉（门禁 5）

`rustup target add aarch64-pc-windows-msvc` 后 `cargo check -p visrecall-ai --target aarch64-pc-windows-msvc`
→ **Finished**（通过）。因为 `ort` 走 `load-dynamic`（运行期加载 `onnxruntime.dll`），不在链接期引入
架构相关的原生库，交叉 check 不需要 ARM64 链接器。

### 1.5 模型文件与依赖偏差（Task 1 登记）

| 项 | 落地内容 | 与 spec 的偏差 |
|---|---|---|
| PP-OCR | **PP-OCRv4 mobile** det + rec（`ch_PP-OCRv4_det_infer.onnx` 4.7 MB、`ch_PP-OCRv4_rec_infer.onnx` 10.9 MB）取自 `SWHL/RapidOCR`（PaddleOCR 官方模型，RapidOCR 转 ONNX）；字典 `ppocr_keys_v1.txt` 取自 PaddlePaddle/PaddleOCR | v5 只有 Paddle 格式（`PaddlePaddle/PP-OCRv5_mobile_*`），ONNX 转换需 paddle2onnx，且社区 ONNX 版本来源不可信；design 允许 v4，此处固定为 v4 |
| 嵌入 | `bge-small-zh-v1.5` 动态 int8 ONNX（`model_quantized.onnx` 24.0 MB）+ `tokenizer.json`，取自 `Xenova/bge-small-zh-v1.5` | 无 |
| 运行时 | `ort 2.0.0-rc.15` + **`load-dynamic`**，`onnxruntime.dll`（1.31.1，取自 PyPI wheel）与模型一起放 `models/`，**sha256 一并进 manifest** | 静态链接不可用：`ort-sys` 预编译静态库用较新 MSVC STL（依赖 `__std_*_vectorized`），本机 MSVC 14.37 链接报 20 个 `LNK2019`。`load-dynamic` 还带来安装包只需附带一个 dll 的好处（阶段 04 打包项） |
| `ndarray` | 0.17 | design 写 0.16；`ort 2.0.0-rc.15` 硬依赖 0.17，二者不能共存 |
| `jieba-rs` | 0.11（`cut` 返回 `Vec<Token>`，取 `token.word`） | tasks 写 0.7；`TOKENIZER_VERSION` 将写 `jieba-rs-0.11`（与依赖同步维护） |
| 依赖清单 | 新增 `async-trait`/`base64`/`image`/`jieba-rs`/`keyring`/`lru`/`ndarray`/`ort`/`reqwest`/`sha2`/`tokio`/`tokenizers`/`wiremock` | `sha2`（manifest 校验）、`image`（PNG 解码/缩放）、`lru`（查询缓存）是 tasks.md 未列但实现必需的依赖 |


## 1.1 阶段 02 遗留项（阶段 03 收口）

| # | 项目 | 结论 | 证据 |
|---|---|---|---|
| 1 | 20 条数据视觉验收 + 截图证据（阶段 02 L1/L2） | 待验证 | 待填 |
| 2 | 标签云点击筛选、hue-dot（本阶段 P3-R10 落地后验收，见 §2） | 待验证 | 待填 |
| 3 | 阶段 01 门禁中仍为「未验证（环境限制）」的 9 项（物理 4K/PresentMon、双屏、100%/200% 缩放、物理 OLE 四来源、DRM、提权、30 分钟长跑、重复捕获、帧率量化） | 待验证（沿用阶段 02 §1 结论，有新证据则更新） | 待填 |
| 4 | 跨卷迁移（阶段 02 L4） | 待验证 | 待填 |

## 2. 阶段 03 验收矩阵

| 需求 | 场景 | 结论 | 证据 |
|---|---|---|---|
| P3-R01 | OCR 准确率 ≥97%（20 张抽样） | 待验证 | 待填 |
| P3-R01 | 模型文件缺失 → Windows OCR 降级 + warn；纯色图空结果 | 待验证 | 待填 |
| P3-R02 | 嵌入确定性 / 语义 sanity / LRU 命中 | 待验证 | 待填（`--ignored` 用例名） |
| P3-R02 | 模型加载耗时与内存增量 ≤60MB | 待验证 | 待填 |
| P3-R03 | VLM 全字段落库；畸形 JSON 修复一次；401 终止重试 | 待验证 | 待填 |
| P3-R03 | 断网：OCR 摘录占位 + pending；恢复后增强替换 | 待验证 | 待填 |
| P3-R03 | 自动标签删除后不回贴（suppressions） | 待验证 | 待填 |
| P3-R04 | 提交→可检索 ≤3s（P95，≥10 次采样） | 待验证 | 待填 |
| P3-R04 | 同事务回滚（无半更新）；阶段二中断恢复 | 待验证 | 待填 |
| P3-R05 | 10 条真实中文查询字面召回 + snippet；删除后 FTS 行消失 | 待验证 | 待填 |
| P3-R06 | 解析规则正/反例；chip 删除重算；混输 | 待验证 | 待填 |
| P3-R07 | 三通道各自必命中；权重变更排序可复现；≤800ms P95 | 待验证 | 待填 |
| P3-R07 | filters-only 尾部附加；降级（语义跳过）仍出结果 | 待验证 | 待填 |
| P3-R08 | 三类命中解释与索引事实一致；OCR 高亮框定位；退化路径 | 待验证 | 待填 |
| P3-R09 | 防抖/过期丢弃；空查询回列表；徽标与 ExplainBar；状态栏降级标注 | 待验证 | 待填 |
| P3-R10 | 标签点击筛选→取消；hue-dot 有/无；颜色口径一致 | 待验证 | 待填 |
| P3-R11 | 凭据管理器条目；IPC/界面无明文；主备切换与恢复；探测三态 | 待验证 | 待填 |
| P3-R12 | revision 取消旧任务；蓝绿重建零中断；自检修复；hash 复用 | 待验证 | 待填 |
| P3-R13 | 降级矩阵逐行（断网/错 Key/删模型/无文字图）；崩溃恢复；无重复执行 | 待验证 | 待填 |
| P3-R14 | **Top-3 命中率 ≥80%**；分桶报告；≥1 轮调参对比存档 | 待验证 | 待填（报告路径） |
| P3-R15 | 全部自动化门禁 + 绑定同步 + release 运行 | 待验证 | 待填 |

## 3. 自动化门禁输出

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test --workspace -- --ignored        # 真实模型用例（本地）
pnpm lint
pnpm typecheck
pnpm test
pnpm build
```

| 命令 | 结果 | 备注 |
|---|---|---|
| （全部） | 待执行 | 待填 |

## 4. 已知限制与偏差（阶段结束时确认）

预登记项（来自 design §1.2，执行中按实际情况增删）：

| 项 | 现状 | 收口阶段 |
|---|---|---|
| trigram 引号短语 | 引号按必命中词组（AND），无语序保证 | 阶段 04 或评测驱动 |
| vec0 快路径 | 不进迁移 0003，仅 spike | 规模需要时（迁移 0004） |
| 嵌入空闲卸载 | 懒加载 + 预热，不按空闲卸载 | 阶段 04 内存优化 |
| PP-OCR cls | 无方向分类（截图水平假设） | 长尾复评 |
| `fts.tokenizer_version` 变更自动重建 | 记录 + warn | 真需要时复用 rebuild |
| pending 素材自动轮询 | 仅启动重试一轮 | V0.1 边界 |
| VLM 用户编辑保护 | 字段预留，逻辑不做 | V0.5 |
| ARM64 | 仅 `cargo check` | 阶段 04 |
| entities 详情展示 | 参与索引与解释，不展示 | P1 |

### 4.1 实施阶段新增的实现偏差（Task 8/9）

| 项 | design/tasks 原文 | 实际做法 | 原因与影响 |
|---|---|---|---|
| `vlm` 入队时机 | Task 10 Step 3「提交成功后图片 `enqueue(ocr)`、文本/链接 `enqueue(index)`；**全部 `enqueue(vlm)`**」 | capture 路径只入队**第一阶段**（图片 `ocr`、文本/链接 `index`）；`vlm` 由 `ocr` handler 在入队 `index` **之后**接力入队 | 若 capture 同时入队 `vlm`，同优先级下 `vlm` 可能先于 `index` 被 claim，导致 `item_enriched` 早于 `item_indexed`，违背 P3-R04「阶段一先完成、立即可检索」与 Task 9 Step 6 的事件顺序断言。同一优先级按 rowid 先进先出，接力入队后顺序天然成立 |
| 蓝绿重建的驱动方式 | Task 9 Step 4「`start_rebuild` → 入队批量 `rebuild` 任务（每批 100 rowid）→ 全部完成后计数比对 → 切指针」 | `lifecycle::rebuild_embeddings` **同步**完成（每批 100 条，20 条规模一次跑完）；`rebuild` **任务**只用于单条素材的派生数据修复（与 `index` 同一份代码） | `jobs` 表没有 payload 列，「目标表名/新模型 id」只能塞 `settings` 或额外建表；20 条规模下 job 调度是纯开销。验收口径（新建表 → 行数校验 → 原子切指针 → 旧表保留 → 检索零中断）全部满足，见 `lifecycle.rs` 测试 |
| `fts.tokenizer_version` 记录点 | design §4.2「写入时取当前」 | `Library::open` 的自检里首次写入、之后不一致只 `warn` | 与原意一致，明确记录时机 |
| 单条 FTS 修复 | 无 | 新增 `lifecycle::rebuild_fts_row`（无 UI 的管理入口） | 排障/评测用，未改变任何需求行为 |

## 5. 结论

**待执行。** 阶段 03 完成判定见 `requirements.md` §5（评测门禁 Top-3 ≥80% 为硬条件）。
