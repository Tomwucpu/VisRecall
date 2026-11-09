# VisRecall V0.1 阶段 03：AI 与检索 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans` to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 落地两阶段索引（OCR→临时嵌入→≤3s 可检索；VLM 异步增强）与三路检索（SQL 过滤 + FTS BM25 + 语义向量，RRF 融合，命中解释 + OCR 高亮），交付模型配置（keyring/探测/主备）与 50 条评测集门禁（Top-3 ≥80%）。

**Architecture:** 三个新 crate：`ai`（纯推理与 HTTP，不碰 DB）、`pipeline`（jobs 驱动的 worker 与两阶段编排）、`search`（解析/召回/融合/解释，纯函数为主）；`library` 扩展迁移 0003 与索引原语（同事务写）；`domain` 持有全部 Provider trait；`app-core` 编排与注入；`eval` 独立 bin 做评测。依赖方向见 `design.md` §3。

**Tech Stack:** Rust 1.93（MSVC）、Tauri 2、React 19、TypeScript、SQLite（rusqlite bundled，FTS5）、`ort` 2.x、`jieba-rs` 0.7、`tokenizers`、`ndarray` 0.16、`tokio` 1.x、`reqwest` 0.13（async）、`keyring` 3.x、`base64` 0.22、`tauri-specta`、TanStack Query、Vitest。

## Global Constraints

- 仅 Windows 10 22H2（19045）+ / Windows 11；x64（ARM64 只做 `cargo check` 交叉验证）。
- 单进程 `visrecall.exe`；**查询路径（解析/FTS/嵌入/向量）零网络**；云端 VLM 仅入库侧。
- 素材测试规模 **≤20 条**（构成见 `verification.md` §0.1）；20 条结论不外推为大规模性能结论；设计上限约 10 万条。
- 新鲜度：提交→可检索 ≤3s（P95）；检索 ≤800ms（P95，测试规模内）；嵌入模型内存增量 ≤60MB；托盘常驻 ≤150MB 硬上限。
- insights + FTS + 向量三处写入**同一事务**；任何第二存储引擎、外部队列、HTTP 嵌入服务都禁止引入。
- API Key 只存 Windows 凭据管理器；不进库文件、config.json、日志与任何 IPC 响应。
- 模型文件带 sha256 清单；校验失败走兜底路径不崩溃。
- 事件只携带 id / 小载荷；OCR 全文、boxes、向量都不进事件。
- 依赖锁 `Cargo.lock` / `pnpm-lock.yaml`；`tauri-specta` 绑定再生成后必须提交；每个任务结束提交一次。
- 真实模型推理用例标 `#[ignore]`（CI 无模型跳过）；T0 与验收前本地 `cargo test -- --ignored` 必跑。

---

### Task 1: T0 启动门禁 spike（依赖引入 + 五项验证）

**Files:**
- Modify: 根 `Cargo.toml`（workspace `[workspace.dependencies]`：`jieba-rs`、`ort`、`tokenizers`、`ndarray`、`tokio`、`keyring`、`base64`；dev-dep `wiremock`）
- Create: `models/`（PP-OCR det/rec ONNX、bge-small-zh int8 ONNX、tokenizer.json——二进制入仓库）
- Create: `models/manifest.json`（文件名 → sha256）
- Create: `scripts/models-check.ps1`（计算/核对 sha256）
- Create: `crates/ai/Cargo.toml`、`crates/ai/src/lib.rs`（骨架）
- Create: `crates/ai/examples/ocr_spike.rs`、`crates/ai/examples/embed_spike.rs`
- Modify: `docs/specs/v0.1/03-ai-search/verification.md`（§1 T0 表回填实测数字）

**Interfaces:**
- Consumes: 无
- Produces: workspace 依赖锁定；`crates/ai` crate（空骨架 + models 定位工具 `models::resolve_model_dir()`）；T0 门禁结论

- [x] **Step 1: workspace 依赖与 ai crate 骨架**

按架构文档《VisRecall-阶段三-AI与检索架构设计》附录 A 与本文 Tech Stack 行把依赖加入 `[workspace.dependencies]`（版本以锁定的 `Cargo.lock` 为准）；`crates/ai` 只建 `lib.rs`（`pub mod models;`）与 `models.rs`：`resolve_model_dir() -> Result<PathBuf, AppError>` 按「`VISRECALL_MODEL_DIR` → exe 同级/上级 `models/` → cwd `models/`」顺序找到含 `manifest.json` 的目录；`verify_manifest()` 逐文件 sha256。

> 落地注记：目录解析改为「不写死绝对路径」的可执行文件向上回溯，并加 `runtime.rs`（`load-dynamic` 加载 `onnxruntime.dll`）。偏差与原因见 `verification.md` §1.5。

- [x] **Step 2: 放置模型文件 + 清单**

PP-OCRv4 mobile det + rec（v5 无可用 ONNX 来源，选定 v4 并固定）；bge-small-zh-v1.5 int8 动态量化 ONNX + tokenizer.json；`manifest.json` 记录 sha256 + 来源 URL + 许可。`scripts/models-check.ps1` 核对 6/6 OK。

- [ ] **Step 3: OCR spike**

`examples/ocr_spike.rs` 已实现并跑通：真实截图 5 张（1080p/1440p/2868×8000）全部返回文本、0 报错；典型 1440p P95 798ms、超长图 1856ms；`scripts/ocr-accuracy.ps1` 已实现（CER 口径）。
**未完成**：字符准确率门禁需 100–200 张（T0 口径）/ 20 张（P3-R01 口径）真实截图 + 人工标注，本机只有 5 张且无标注 → 按 2026-11-08 决策登记为「未验证（环境限制）」，素材到位后补测回填。

- [x] **Step 4: 嵌入 spike**

`examples/embed_spike.rs`：加载 145ms、单条 P95 1.4ms、内存 +56.4MB、维度 512、确定性 0 误差、语义 sanity 10/10、LRU 命中 0.001ms。

- [x] **Step 5: jieba + FTS5 spike**

`crates/library/tests/fts_spike.rs`（6 passed）：FTS5 可用性冒烟、20 条中文素材 × 10 条真实查询字面召回、`snippet()` 标记、BM25 排序、删除减行、emoji/全角查询不崩。

- [x] **Step 6: （可选）vec0 与 ARM64**

vec0 按 design §1.2 搁置（不进依赖图、不建表）；`cargo check -p visrecall-ai --target aarch64-pc-windows-msvc` 通过（`load-dynamic` 无架构相关原生库）。

- [x] **Step 7: 回填门禁表**

T0 五项实测数字与偏差已写入 `verification.md` §1（§1.1–§1.5）。门禁 1 的字符准确率未验证（环境限制）+ 内存叠加风险（OCR 88MB + 嵌入 56MB ≈ 190MB > 150MB 硬上限）已登记收口方案；**进 Task 2 的依据是用户 2026-11-08 决策「先按 20 张抽样口径，T0 的 100–200 张门禁记未验证」**。

**Verify:**

```bash
cargo run -p visrecall-ai --example ocr_spike -- --images <dir>    # 需模型文件
cargo run -p visrecall-ai --example embed_spike                     # 需模型文件
cargo test -p visrecall-library                                      # 含 FTS5/jieba spike
cargo clippy -p visrecall-ai --all-targets -- -D warnings
scripts/models-check.ps1
```

**Commit:** `feat(ai): add workspace deps, model files and t0 spike`

---

### Task 2: 迁移 0003 与索引存储原语（library）

**Files:**
- Create: `crates/library/migrations/0003_ai_search.sql`
- Modify: `crates/library/src/migrations.rs`（追加版本 3）
- Modify: `crates/library/src/items.rs`（revision 列读写、`bump_revision`、`primaryColor` JOIN）
- Create: `crates/library/src/insights.rs`
- Create: `crates/library/src/fts.rs`
- Create: `crates/library/src/vectors.rs`
- Create: `crates/library/src/queries_log.rs`
- Modify: `crates/library/src/purge.rs`（删除事务补 FTS 显式删除）
- Modify: `crates/library/src/db.rs`（启动自检计数）
- Modify: `crates/library/src/lib.rs`
- Create: `crates/library/tests/ai_storage_db.rs`

**Interfaces:**
- Consumes: 0001/0002 既有表；`settings_kv`
- Produces:
  - `Library::bump_revision(id) -> Result<i64, AppError>`（返回新 revision）
  - `Library::insight_upsert(&Insight)` / `insight_get(id)` / `insights_missing_fts()` 等自检查询
  - `Library::fts_upsert(rowid, cols: &FtsColumns)`（调用方已分词）/ `fts_delete(rowid)` / `fts_search(expr, candidates, limit) -> Vec<FtsHit>`
  - `Library::vector_upsert(rowid, vec8, dim, model_id)` / `vector_delete(rowid)` / `active_vector_table() -> String` / `vector_switch_active(table)` / `vector_counts()`
  - `Library::index_write(tx 闭包：insight + fts + vector 三写)` —— 以 `&mut Transaction` 参数暴露，供 pipeline 组装同事务
  - `Library::query_log_insert(QueryLog)` / `query_log_set_feedback(id, feedback)`

- [x] **Step 1: 写 0003 迁移**（SQL 见 design §4.1 原样；`ALTER TABLE items ADD COLUMN revision` + 四张表/索引）

- [x] **Step 2: revision 与 primaryColor**

`bump_revision`：`UPDATE items SET revision=revision+1, updated_at=?2 WHERE id=?1 RETURNING revision`；`list_items`/`get_item` 的 SELECT 增加 `LEFT JOIN insights` 取 `primary_color`（`Item.primary_color: Option<String>`，DTO 映射在 Task 12 一并接 bindings）。

- [x] **Step 3: fts.rs（contentful 语义）**

```rust
pub struct FtsColumns<'a> {
    pub title: &'a str, pub summary: &'a str, pub ocr_text: &'a str,
    pub note: &'a str, pub tags: &'a str, pub entities: &'a str,
    pub original_text: &'a str,
}
// fts_upsert：INSERT INTO items_fts(rowid, ...) VALUES(?1, ...)
//   ON CONFLICT(rowid) DO UPDATE SET ...（FTS5 支持 UPSERT？——不支持：先 DELETE 再 INSERT，同一事务内原子）
// fts_delete：DELETE FROM items_fts WHERE rowid=?1
// fts_search：
//   SELECT rowid, snippet(items_fts, -1, '«', '»', '…', 24), bm25(items_fts)
//   FROM items_fts WHERE items_fts MATCH ?1 [AND rowid IN (candidates)]
//   ORDER BY rank LIMIT ?2
```

候选集下推：候选 ≤500 时拼 `rowid IN (...)`；>500 或全集时不过滤（由调用方在融合后再过滤——20 条规模恒走 IN 分支，实现只留 IN 分支也可，登记简化）。

- [x] **Step 4: vectors.rs（暴力路径 + 蓝绿指针）**

`vector_search` 在 `vectors.rs` 不做——检索侧余弦在 `VectorIndex` 实现里（Task 4），本任务只做表读写：`vector_upsert`（DELETE+INSERT by rowid）、`vector_delete`、`active_vector_table`（读 settings，缺省 `item_vectors_int8`）、`vector_switch_active`（单事务改 settings）、`create_vector_table(name)`（同 0003 结构，供蓝绿）、`vector_counts()`（自检）。

- [x] **Step 5: 同事务写入口**

```rust
/// 三写同事务：调用方在闭包内完成 insight_upsert / fts_upsert / vector_upsert，
/// 任一 Err 整体回滚。这是 A1「一致性模型」的代码化身，禁止拆开调用。
pub fn write_index_tx(&self, f: impl FnOnce(&Transaction) -> Result<(), AppError>) -> Result<(), AppError>;
```

- [x] **Step 6: 删除与自检**

`purge.rs`：`delete_item_row` 的事务里追加 `DELETE FROM items_fts WHERE rowid=?`（insights/向量靠 FK 级联）；`library_purge` 追加 `DELETE FROM items_fts`、`DELETE FROM queries`。`db.rs` 启动：三个 COUNT 比对，不一致 `warn!`（修复入队在 Task 9 接）。

- [x] **Step 7: 测试**（`tests/ai_storage_db.rs`）

- 0003 幂等（两次 apply）；0002 库升级后 revision 默认 0、旧数据无损。
- `write_index_tx` 中段失败（闭包返回 Err）→ insights/FTS/向量三处均无写入。
- `fts_upsert` 覆盖写（同 rowid 二次写不重复行）；`fts_delete` 后 `COUNT` 减少。
- 蓝绿：`create_vector_table("..._v2")` → 写入 → `vector_switch_active` → `active_vector_table()` 返回新表；旧表数据仍在。
- `bump_revision` 返回递增值；删除素材后 FTS 行数、queries 无残留（purge 场景）。
- 自检：手工删一条 FTS 行 → 计数差 1。

**Verify:**

```bash
cargo test -p visrecall-library
cargo clippy -p visrecall-library --all-targets -- -D warnings
```

**Commit:** `feat(library): add migration 0003 with fts, vectors and insights storage`

---

### Task 3: search crate 骨架——分词器与 FTS 查询构造

**Files:**
- Create: `crates/search/Cargo.toml`、`crates/search/src/lib.rs`
- Create: `crates/search/src/tokenizer.rs`
- Create: `crates/search/src/fts_query.rs`
- Create: `crates/search/src/document.rs`
- Create: `crates/search/tests/tokenizer_fts.rs`

**Interfaces:**
- Consumes: `visrecall-library`（dev-dep，集成测试用）
- Produces: `TOKENIZER_VERSION`、`tokenize_for_fts`、`terms_for_query`、`build_match_expr(terms, quoted) -> String`、`build_search_document(&DocumentFields) -> String`

- [x] **Step 1: tokenizer.rs（唯一 jieba 入口）**

`jieba::Jieba` 全局 `OnceLock`；`tokenize_for_fts`：切分 → 过滤纯空白/标点 token → 空格连接；`terms_for_query`：切分 → 去停用词表（内置 ~100 常见虚词）→ 去重。`TOKENIZER_VERSION = "jieba-rs-0.7"`（与依赖版本同步维护，升级时改常量）。

- [x] **Step 2: fts_query.rs（MATCH 构造与转义）**

- 转义：剔除 `"` `*` `(` `)` `:` `^` `-` 等 FTS5 语法字符（中文查询天然不含，英文场景兜底）。
- `build_match_expr(terms, quoted)`：`terms` 以 ` OR ` 连接（空则不产生通道查询）；`quoted` 逐词转义后以 ` AND ` 连接，整体追加。返回空串表示 FTS 通道跳过。
- 拼接结果只含安全字符——proptest 断言任意随机输入不产生语法错误（对临时库跑 `MATCH` 冒烟）。

- [x] **Step 3: document.rs（索引侧文本）**

字段序 `title > summary > tags > entities > note > ocr_or_original`；按 512 token 预算截断（字符近似常量 `DOC_CHAR_BUDGET`，T0 校准值固化并注明）；字段间以 `；` 分隔，空字段跳过。

- [x] **Step 4: 测试**

- 同一文本 `tokenize_for_fts` 与 `terms_for_query` 词形一致（写入查得回）。
- `build_match_expr`：含语法字符输入安全；quoted AND 语义。
- `build_search_document`：超长 ocr 截断后 title 仍完整保留；空字段不影响。
- 集成：tempdir 建库 → `fts_upsert(tokenize_for_fts(...))` → `fts_search(build_match_expr(...))` 中文查询召回（10 条真实查询）。

**Verify:**

```bash
cargo test -p visrecall-search
cargo clippy -p visrecall-search --all-targets -- -D warnings
```

**Commit:** `feat(search): add tokenizer, match builder and search document`

---

### Task 4: VectorIndex trait 与暴力 int8 余弦

**Files:**
- Create: `crates/domain/src/vector_math.rs`
- Modify: `crates/domain/src/providers.rs`（`VectorIndex` trait，design §4.3）
- Modify: `crates/domain/src/lib.rs`
- Create: `crates/library/src/vector_index.rs`（`BruteForceInt8` 实现）
- Modify: `crates/library/src/lib.rs`
- Create: `crates/library/tests/vector_index.rs`

**Interfaces:**
- Consumes: `item_vectors_int8` 表、`vector.active_table` 指针
- Produces: `domain::{quantize_i8, cosine_i8, VectorIndex}`；`library::BruteForceInt8::new(library) -> Self`

- [x] **Step 1: vector_math 纯函数**

`quantize_i8`：`round(x * 127 / max|x|)`（全零向量返回全零）；`cosine_i8`：int8 点积 / (模长乘积)，字节循环按 u8 展开（编译器自动向量化；不引入 SIMD crate）。单测：量化误差下与 f32 余弦排序一致（随机 1000 对，Spearman >0.99）。

- [x] **Step 2: BruteForceInt8**

```rust
pub struct BruteForceInt8 { library: Arc<Library> }
impl VectorIndex for BruteForceInt8 {
    // add/remove：写活跃表（model_id 与 settings 的 embedding.model_id 校验，不一致 → Err）
    // search：SELECT item_rowid, embedding FROM <active_table>
    //   [WHERE item_rowid IN (candidates)] → Rust 侧逐条 cosine_i8 → Top-K（二叉堆）
}
```

`search` 的 `query: &[f32]` 先 `quantize_i8` 再比；`dim` 不匹配的行跳过并 `warn`（防脏数据毒化结果）。

- [x] **Step 3: 测试**

- 量化往返 + 余弦正确性（正交=0、同向=1）。
- 100 条随机向量 Top-10 与暴力 f32 参考 implementations 排序一致。
- candidates 过滤生效；dim 不匹配行被跳过。
- `add` 在 `model_id` 不一致时报错（防混库向量）。

**Verify:**

```bash
cargo test -p visrecall-domain -p visrecall-library --test vector_index
cargo clippy -p visrecall-domain -p visrecall-library --all-targets -- -D warnings
```

**Commit:** `feat(library): add brute-force int8 vector index`

---

### Task 5: ai crate——EmbeddingProvider（bge-small int8）

**Files:**
- Create: `crates/domain/src/providers.rs`（若 Task 4 未建则本任务建：`EmbeddingProvider` trait）
- Create: `crates/ai/src/embedder.rs`
- Modify: `crates/ai/src/lib.rs`

**Interfaces:**
- Consumes: `models/`（bge ONNX + tokenizer.json）、`models::resolve_model_dir`
- Produces: `ai::BgeSmallEmbedder`（实现 `EmbeddingProvider`；`id()="bge-small-zh-v1.5-int8"`、`dim()=512`、`ready()`、内建 LRU 256 查询缓存）

- [x] **Step 1: 实现**

懒加载：`OnceLock<Session>` + 状态 `AtomicU8 { NotReady, Loading, Ready, Failed }`；`embed` 在 NotReady 时同步加载（首次调用者承担，pipeline 阶段一天然如此）；`ready()` 供检索降级判定。tokenize → 512 截断 → ort run → CLS 池化 → L2 归一化。LRU：`Mutex<LruCache<String, Vec<f32>>>`，键为原文本，容量 256（嵌入批量为 1；批量接口保留给 rebuild 用）。

- [x] **Step 2: 测试**

- `#[ignore]`（需模型）：确定性、维度、延迟（P95 <30ms 断言）、语义 sanity、LRU 二次命中（时间显著下降）。
- 非 ignore：`ready()` 状态机（Failed 注入：模型目录指错 → ready=false、embed 报错不 panic）。

**Verify:**

```bash
cargo test -p visrecall-ai
cargo test -p visrecall-ai -- --ignored
cargo clippy -p visrecall-ai --all-targets -- -D warnings
```

**Commit:** `feat(ai): add bge-small int8 embedding provider`

---

### Task 6: ai crate——OcrProvider（PP-OCR 主 + Windows 兜底）

**Files:**
- Modify: `crates/domain/src/providers.rs`（`OcrProvider` trait、`OcrOutput`）
- Create: `crates/domain/src/insight.rs`（`OcrBox { text, x, y, w, h }` 归一化）
- Create: `crates/ai/src/ocr/mod.rs`、`ocr/ppocr.rs`、`ocr/windows_ocr.rs`
- Create: `crates/ai/tests/ocr_fixtures.rs`
- Create: `crates/ai/tests/fixtures/`（3–5 张带已知文本的截图 + 1 张无文字纯色图，入仓库）

**Interfaces:**
- Consumes: `models/`（det/rec ONNX）、`ndarray`
- Produces: `ai::PpocrOcr`、`ai::WindowsOcr`、`ai::ocr::OcrChain`（PP-OCR→Windows 责任链，`ocr_engine` 字段记录实际引擎）

- [x] **Step 1: ppocr.rs**

det：输入按比例 resize（长边 960、32 对齐）→ 二值图 → 阈值 + 盒子收缩（unclip 近似：按 box score 过滤即可，精度调优在 T0 已完成）；rec：逐 crop resize 高 48、宽按比例（上限 320）→ CTC 解码。输出：`text`（按 y-x 排序拼接，行间 `\n`）、`boxes`（归一化）、`confidence`（逐框均值）。长边 >4096 先等比降采样。

- [x] **Step 2: windows_ocr.rs**

`BitmapDecoder` → `SoftwareBitmap`（BGRA8）→ `OcrEngine::TryCreateFromLanguage(zh-CN)` 失败则 `TryCreateFromUserProfileLanguages` → `RecognizeAsync` block（专用 STA 线程）。boxes 归一化对齐 PP-OCR 结构（word 级合并为行级近似）。

- [x] **Step 3: OcrChain**

PP-OCR 失败（模型缺失/校验失败/推理错误）记 `warn!` → Windows OCR；Windows 也失败 → `Err`（上游按「OCR 无文字」矩阵处理）。链的构造只依赖 `resolve_model_dir` 结果，无运行时探测。

- [x] **Step 4: 测试**

- `#[ignore]`（需模型）：fixtures 图逐张断言关键短语出现在 `text`；boxes 归一化范围 [0,1]；置信度 ∈ [0,1]；**无文字纯色图断言 `text` 为空、boxes 为空、正常返回 `Ok`（空结果是正常结果而非 `Err`，走 §6.1 的空文本分支）**。
- 非 ignore：chain 降级（注入「模型目录无效」）→ `ocr_engine` 记录为 windows（本机有语言包时）或 `Err`（无语言包环境记 `#[ignore]`）。

**Verify:**

```bash
cargo test -p visrecall-ai --test ocr_fixtures -- --ignored
cargo clippy -p visrecall-ai --all-targets -- -D warnings
```

**Commit:** `feat(ai): add ppocr provider with windows fallback chain`

---

### Task 7: ai crate——VLM/Text Provider（OpenAI 兼容）+ keyring + 探测

**Files:**
- Modify: `crates/domain/src/providers.rs`（`VisionProvider` / `TextProvider` / `VisionInsight` / `TextInsight`）
- Create: `crates/ai/src/vlm/mod.rs`、`vlm/schema.rs`
- Create: `crates/ai/src/keyring.rs`
- Create: `crates/ai/tests/vlm_wiremock.rs`
- Modify: `crates/domain/src/error.rs`（`ProviderConfig` / `SchemaViolation` / `ModelNotReady` 变体与稳定码 `PROVIDER_CONFIG` / `SCHEMA_VIOLATION` / `MODEL_NOT_READY`）

**Interfaces:**
- Consumes: `reqwest`（async）、`keyring`、`base64`
- Produces:
  - `ai::vlm::OpenAiCompatible { base_url, model, api_key }`：`chat_completions(messages) -> Result<String>`
  - `impl VisionProvider`（图片 data URL + 结构化提示词 → `VisionInsight`）/ `impl TextProvider`
  - `ai::vlm::validate(raw_json) -> Result<VisionInsight, AppError>`（schema + 修复一次）
  - `ai::keyring::{save_key, load_key, delete_key}(slot)`（service `"VisRecall"`，account `vlm-primary`/`vlm-backup`）
  - `ai::vlm::probe(base_url, model, key) -> ProbeOutcome`（文本 ping + 1×1 PNG 视觉）

- [x] **Step 1: schema.rs**

serde 强类型（design §4.5）+ 枚举校验 + `summary` ≤200 字符（给修复留余量）+ `tags` ≤8、逐个 ≤32 字符（超限截断不报错）。`repair_once(raw, serde_err)`：把错误与原文拼进修复提示词再请求一次。剥离 ```` ```json ```` 包裹。

- [x] **Step 2: OpenAiCompatible 客户端**

`POST {base_url}/chat/completions`，Bearer key，`timeout 30s`；错误分类：连接/超时/5xx → `Provider`（可重试可切备）；401/403 → `ProviderConfig`（终止）；429 → `Provider` 且退避翻倍。视觉消息：`{ type: "image_url", image_url: { url: "data:image/png;base64,..." } }`。

- [x] **Step 3: keyring.rs**

`save_key` 覆盖写；`load_key` 不存在返回 `None`；`delete_key` 幂等。**日志与错误信息绝不包含 key 内容。**

- [x] **Step 4: wiremock 测试**

- 正常 JSON → `VisionInsight` 全字段。
- Markdown 包裹 → 剥离成功；畸形 JSON → 修复一次路径（mock 第二次返回合法）。
- 401 → `ProviderConfig` 且无重试；500 → `Provider`；超时（mock delay）→ `Provider`。
- 探测：mock 文本 ok + 视觉 ok → `{ vision: true }`；视觉端点 4xx → `{ vision: false }`。
- keyring 往返（真实凭据管理器，本机跑；CI 标 `#[ignore]`）。

**Verify:**

```bash
cargo test -p visrecall-ai --test vlm_wiremock
cargo test -p visrecall-ai -- --ignored
cargo clippy -p visrecall-ai --all-targets -- -D warnings
```

**Commit:** `feat(ai): add openai-compatible vlm providers with keyring`

---

### Task 8: pipeline crate——worker 框架

**Files:**
- Create: `crates/pipeline/Cargo.toml`、`src/lib.rs`、`src/worker.rs`
- Create: `crates/pipeline/tests/worker_framework.rs`

**Interfaces:**
- Consumes: `jobs` 表、`Library`、`EventSink`
- Produces:
  - `pipeline::Pipeline { deps: PipelineDeps }`：`spawn(library, deps, sink) -> PipelineHandle`（2 线程）
  - `PipelineHandle::shutdown()`（join，供测试与迁移闸门复用）
  - handler 注册：`stage -> Arc<dyn JobHandler>`（`JobHandler::run(&JobCtx) -> Result<(), AppError>`）
  - `enqueue(library, item_id, stage, priority) -> Result<(), AppError>`（revision 取当前 `items.revision`；`INSERT ... ON CONFLICT(item_id,stage,revision) DO NOTHING`）

- [x] **Step 1: claim 与状态机**

design §6.2 的 `UPDATE ... RETURNING` 原子 claim；成功 → 调 handler；`Ok` → `succeeded`；`Err(Provider)` → 退避（`next_run_at = now + [2,5,15]s`，attempts≥3 → `failed`）；`Err(ProviderConfig)` → 立即 `failed` 并 `last_error` 记配置错误；`Err` 其他 → 同退避。claim 后读 `items.revision` 比对，不匹配 → `cancelled`。

- [x] **Step 2: 循环与水位**

worker 空闲 500ms 轮询；`pipeline_status { queued, running }` 事件 100ms 合并窗口（`Mutex<Option<Payload>>` + 定时 flush 线程，或简单地在每次状态变化后发、上限每 100ms 一条）。

- [x] **Step 3: 测试（假 handler）**

- 两个任务两个 worker 并发执行无重复（假 handler 计数 + 断言）。
- 失败退避：attempts 1→2→3 → `failed`；`ProviderConfig` 一次即 `failed`。
- stale：入队后 `bump_revision` → claim 即 `cancelled`，handler 不执行。
- 崩溃恢复：预置 `running` 行 → 重新 spawn → 变 `queued` 且 attempts 保留。
- 优先级：同时入队 priority 0 与 2 → 0 先执行。

**Verify:**

```bash
cargo test -p visrecall-pipeline
cargo clippy -p visrecall-pipeline --all-targets -- -D warnings
```

**Commit:** `feat(pipeline): add job worker framework with backoff and stale cancel`

---

### Task 9: pipeline——两阶段编排与生命周期

**Files:**
- Create: `crates/pipeline/src/handlers/{mod.rs,ocr.rs,index.rs,vlm.rs,rebuild.rs}`
- Create: `crates/pipeline/src/lifecycle.rs`
- Create: `crates/pipeline/tests/two_stage.rs`、`tests/lifecycle.rs`
- Modify: `crates/library/src/items.rs`（`note_set`/`set_note` 与 tags 写路径挂 `bump_revision` 的内部钩子——**注意**：库层不依赖 pipeline，改为提供 `on_metadata_changed` 回调注入，由 app-core 注册入队）

**Interfaces:**
- Consumes: Task 3 `document.rs`/`tokenizer.rs`、Task 5/6/7 Provider、Task 8 worker、Task 2 `write_index_tx`
- Produces: `handlers::ocr/index/vlm/rebuild`（注册进 `Pipeline::spawn`）；`lifecycle::{run_startup_checks, retry_pending, start_rebuild}`；`Library` 的 `set_note`/`tag_add`/`tag_detach`/`tag_rename` 完成后通过注入的 `MetadataHook` 触发 `bump_revision + enqueue(index)`

- [x] **Step 1: handlers::ocr**

读 `items.original_path`（图片）→ `OcrChain.recognize` → `write_index_tx` 内 `insight_upsert`（只动 ocr_* 字段，UPSERT 保序合并：读旧值-合并-写回）→ `enqueue(index)`。OCR `Err`（两级都失败）→ insights 建行（无 ocr 字段）→ 照常 `enqueue(index)`（素材仍可被元数据搜到）。

- [x] **Step 2: handlers::index**

`build_search_document`（字段来自 items + insights + tags + entities）→ `embed` → `write_index_tx { insight_upsert(摘要占位合并), fts_upsert(tokenize_for_fts(...)), vector_upsert(quantize_i8(vec), 512, model_id) }` → `emit item_indexed`。占位摘要规则：阶段一 `summary_source` 现值为 `none` 时写 OCR/原文摘录前 280 字符（图片 `ocr`、文本 `extractive`）；已是 `vlm`/`user` 不覆盖（`summary_user_edited` 恒不覆盖）；**OCR 文本为空/缺失时不写占位**——summary 保持 NULL、source 保持 `none`（空串占位会让「无摘要」与「OCR 摘录为空」不可区分，详见 design §6.1 空文本分支）。

- [x] **Step 3: handlers::vlm**

`VisionProvider.analyze` / `TextProvider.summarize` → `validate`（修复一次在 Provider 内）→ `write_index_tx { insight_upsert 全字段 }` → 自动标签：`tag_add` 内部路径（`origin='auto'`、过滤 `tag_suppressions`、复用同名）→ `bump_revision` → `enqueue(index)` → `UPDATE items SET status='ready'` → `emit item_enriched` + `item_updated`。失败 3 次 → `status='pending'` + `emit item_updated`；`ProviderConfig` → 同上但 `last_error` 标注。

- [x] **Step 4: handlers::rebuild 与 lifecycle**

`start_rebuild(new_model_id)`：`create_vector_table("_v2")` → 入队批量 `rebuild` 任务（每批 100 rowid，priority 2）→ 每批写 v2 → 全部完成后 COUNT 比对 → `vector_switch_active` + `settings` 的 `embedding.model_id/dim` 更新（同一事务）。`run_startup_checks`：Task 2 的计数差异 → 缺 FTS/向量的素材入队 `index`。`retry_pending`：启动时 `status='pending'` 素材重入队 `vlm`（attempts 归零）。

- [x] **Step 5: MetadataHook 接线**

`app-core` 构造 `Library` 时注册 hook（`Arc<dyn Fn(item_ids)>`）：`set_note`、`tag_add/detach`（单素材）、`tag_rename/delete`（受影响素材集合）在写事务提交后调用 → hook 里 `bump_revision + enqueue(index)`。**hook 在事务外调用**（避免库层反向依赖 pipeline，也避免长事务）。

- [x] **Step 6: 测试（全部用假 Provider）**

- 两阶段时序：ocr → index → item_indexed → vlm → index → item_enriched；事件顺序断言。
- 新鲜度：假 Provider 零延迟下，提交到 `item_indexed` 的事务路径完整（真实延迟在验收实测）。
- VLM 失败注入 → 重试 3 次 → `pending`；OCR 摘录占位存在且 `summary_source='ocr'`。
- **无字图片**：假 OCR 返回空 `OcrOutput`（text 空、boxes 空）→ insights 建行且 `ocr_text`/`ocr_confidence` 为 NULL → 占位摘要不写（summary NULL、source `none`）→ index/vlm 照常完成 → 该素材可被标题/备注命中；断言不触发 <0.6 低置信路径。
- 401 注入 → 一次即停，`last_error` 含配置错误。
- suppressions：预置 `tag_suppressions("x","发票")` → VLM 返回含「发票」标签 → 不回贴。
- content_hash 复用：同 hash 两条素材 → VLM 只调一次（假 Provider 计数）。
- note 修改 → 旧 index 任务取消、新任务执行（FTS 含新备注）。
- rebuild：切表后 `vector.active_table` 变更、行数一致、检索不中断（切换前后各查一次）。
- 崩溃恢复：vlm 执行中 drop `PipelineHandle`（模拟崩溃）→ 重启 → 任务重跑、无重复副作用（insight 幂等）。

**Verify:**

```bash
cargo test -p visrecall-pipeline
cargo clippy -p visrecall-pipeline --all-targets -- -D warnings
```

**Commit:** `feat(pipeline): add two-stage indexing and lifecycle handlers`

---

### Task 10: app-core——编排接线、模型配置与事件

**Files:**
- Create: `crates/app-core/src/pipeline_host.rs`
- Create: `crates/app-core/src/search_flow.rs`（本任务先落嵌入查询与 Provider 注册表，检索编排在 Task 12）
- Modify: `crates/app-core/src/app.rs`（启动顺序、`ModelConfig`、配置热生效）
- Modify: `crates/app-core/src/events.rs`（4 个新事件）
- Modify: `crates/app-core/src/capture_flow.rs` / `import_flow.rs`（提交后入队）
- Modify: `crates/domain/src/config.rs`（`ModelConfig { primary, backup }`，`ProviderEntry { base_url, model }`）
- Modify: `crates/app-core/tests/`（新增 `pipeline_host.rs`）

**Interfaces:**
- Consumes: Task 5–9 全部产物
- Produces:
  - `AppCore::pipeline() -> &PipelineHandle`
  - `AppCore::embed_query(text) -> Option<Vec<f32>>`（LRU + ready 判定 + 语义状态事件）
  - `AppCore::model_config() / set_model_config(patch) / probe_model(slot)`
  - `AppCore::rebuild_embeddings(new_model_id)`（评测/管理）
  - 事件 `ItemIndexed` / `ItemEnriched` / `PipelineStatus` / `SemanticStatus`（名字 `item_indexed` / `item_enriched` / `pipeline_status` / `semantic_status`）
  - `AppStatus.semantic_ready`

- [x] **Step 1: Provider 注册表**

`pipeline_host::ProviderRegistry`：主备 `OpenAiCompatible` 构造（config.json + keyring 装配）、健康 `AtomicU8`、`active() -> &dyn VisionProvider`（主健康用主，否则备）；`model_config_set` 后重建注册表（热生效）；`model_probe` 走同一构造路径。

- [x] **Step 2: 启动顺序（design §7.5）**

`Library::open`（自检 warn）→ 迁移 → `Pipeline::spawn`（2 worker + handlers + MetadataHook 注册）→ `retry_pending` + `run_startup_checks` 修复入队 → 嵌入预热（后台线程 load，完成发 `semantic_status { ready }`）。

- [x] **Step 3: 提交路径入队**

`capture_flow` / `import_flow` 提交成功后：图片 `enqueue(ocr, 0)`；文本/链接 `enqueue(index, 0)`；全部 `enqueue(vlm, 0)`。**入队失败只 `warn`**（捕获主链路不为索引让路——素材在，可由自检/重启补索引）。

- [x] **Step 4: embed_query 与降级**

`embed_query`：`ready()` 为假 → 返回 `None` 并确保 `semantic_status { ready: false }` 已发；真 → LRU 查/算。预热线程完成 → `semantic_status { ready: true }`。

- [x] **Step 5: 测试**

- 启动顺序断言（事件序列：semantic_status 在 worker spawn 之后）。
- 入队失败注入（断开 jobs 表写权限模拟）→ 捕获仍成功、`warn` 有记录。
- `model_config_set` 热生效：改 base_url 后 `active()` 用新地址（假 HTTP 断言）。
- `embed_query` 未就绪 → None + 事件；就绪 → Some 且缓存命中。
- `AppStatus.semantic_ready` 派生正确。

**Verify:**

```bash
cargo test -p visrecall-app-core
cargo clippy -p visrecall-app-core --all-targets -- -D warnings
```

**Commit:** `feat(app-core): host pipeline, provider registry and semantic readiness`

---

### Task 11: search crate——查询解析器与过滤器

**Files:**
- Create: `crates/domain/src/search.rs`（`ParsedQuery` 等类型，design §4.3）
- Create: `crates/search/src/parser.rs`
- Create: `crates/search/src/filters.rs`
- Create: `crates/search/tests/parser.rs`（含 proptest）

**Interfaces:**
- Consumes: `terms_for_query`、颜色/图表/时间词表
- Produces: `parse_query(raw) -> ParsedQuery`；`filters::to_sql(&ParsedQuery) -> SqlFilter`（`{ clause: String, params: Vec<Value> }`，含 candidates 查询：`SELECT rowid FROM items LEFT JOIN insights ... WHERE ...`）

- [x] **Step 1: 规则表实现（design §6.6）**

时间词计算 `TimeRange`（本地时区、周一起始、`最近 N 天` N≤365）；颜色同义词表（含深色/浅色多值映射）；`app:X`/`来自X`/`在X里`；`tag:X`/`#X`；`"..."` 引号；「公式」→ hasFormula。匹配从原文中**剥离**已消费片段，剩余进 `semantic_text`；一个词只消费一次（「上个月」不得同时当时间与语义词）。

- [x] **Step 2: filters.rs**

生成候选 SQL：`items` 与 `insights`（`primary_color IN (...)`、`chart_type=?`、`has_formula=?`）、`items`（`captured_at BETWEEN`、`kind=?`、`source_app LIKE ? COLLATE NOCASE`）、`item_tags JOIN tags`（标签名精确 NOCASE）。无任何过滤器 → clause 为空（全集）。

- [x] **Step 3: 测试**

- 每规则正/反例（requirements P3-R06 验收清单）；混合查询「上个月来自 chrome 的橙色漏斗图 "季度营收"」全解析。
- proptest：随机输入不 panic、`semantic_text` 不含已剥离片段、往返（chips 重建的查询文本再解析等价）。
- `to_sql` 参数计数与占位符一致（`?` 数 == params.len() 断言）。

**Verify:**

```bash
cargo test -p visrecall-search --test parser
cargo clippy -p visrecall-search --all-targets -- -D warnings
```

**Commit:** `feat(search): add rule-based query parser and sql filters`

---

### Task 12: search crate——三路召回、RRF、解释与 search_items IPC

**Files:**
- Create: `crates/search/src/rank.rs`
- Create: `crates/search/src/execute.rs`
- Modify: `crates/search/src/lib.rs`
- Create: `crates/search/tests/execute_search.rs`
- Modify: `src-tauri/src/ipc.rs`（`search_items` / `search_feedback` / `embedding_rebuild` + DTO；`ItemDto.primaryColor` 接 bindings）
- Modify: `crates/app-core/src/search_flow.rs`（编排补全）
- Regenerate: `src/lib/bindings.ts`

**Interfaces:**
- Consumes: Task 2–4、11 全部；`EmbeddingProvider`（注入）；`search.rrf` settings
- Produces:
  - `rank::rrf(channels: &[&[(i64, f32)]], k, weights) -> Vec<(i64, f32)>`（平局 `captured_at DESC, rowid DESC`，由调用方传入排序键）
  - `execute::search(library, vector_index, embedder, raw, limit) -> Result<SearchOutcome, AppError>`（`SearchOutcome { took_ms, parsed, semantic_used, hits: Vec<ScoredItem>, query_id }`）
  - IPC：`search_items(raw, limit) -> SearchResponseDto`；`search_feedback(query_id, item_id, signal)`；`embedding_rebuild()`

- [ ] **Step 1: rank.rs**

`score = Σ w_ch/(k + rank_ch)`（rank 从 1 起）；稳定排序：score DESC → captured_at DESC → rowid DESC。纯函数单测：两通道交集加权、单通道、权重 0（关通道）、k 边界。

- [ ] **Step 2: execute.rs 编排**

design §6.3 流程：parse → embed（`Option`）→ `filters::to_sql` 候选 → `fts_search`（无词则跳）→ `vector_index.search`（无向量则跳）→ RRF → filters-only 尾部（score 0，排序 captured_at DESC）→ 装配 `Explanation`（filters 按素材实际属性命中过滤；terms = 查询词 ∩ FTS 命中词；`semantic` 仅无字面/过滤命中；`highlight_boxes` = terms 与 `ocr_boxes` 文本包含匹配）→ `query_log_insert` → `took_ms`。`failed` 素材不过滤。limit clamp 1..=200。

- [ ] **Step 3: IPC 与 DTO**

`search_items`：解析快路径（不阻塞）→ `embed_query` → `run_blocking(execute)`。DTO 见 design §5.2；`ItemDto` 增加 `primaryColor`（Task 2 的 JOIN 落到 DTO）。`search_feedback` 对不存在 queryId → `NOT_FOUND`。`embedding_rebuild` 调 Task 10 的 `rebuild_embeddings`。

- [ ] **Step 4: 测试**

- 假嵌入（hash→向量）下：纯字面、纯语义、混合、filters-only 四类查询各断言排序与解释。
- 降级：embedder 未就绪 → `semantic_used=false`，FTS 结果照常。
- 平局稳定：同分素材顺序确定（两次调用一致）。
- `queries` 行写入与 feedback 往返。
- `api_error_maps_stable_codes` 补 `MODEL_NOT_READY` 等新码；事件名稳定性测试补 4 条（Task 10 已加事件，此处一并断言）。
- bindings 再生成并提交。

**Verify:**

```bash
cargo test -p visrecall-search -p visrecall
cargo clippy --workspace --all-targets -- -D warnings
git diff --stat src/lib/bindings.ts
```

**Commit:** `feat(search): add three-channel retrieval, rrf fusion and explanation`

---

### Task 13: 前端搜索界面

**Files:**
- Create: `src/features/search/SearchBar.tsx`、`QueryChips.tsx`、`ResultGrid.tsx`、`ExplainBar.tsx`、`useSearch.ts`
- Create: `src/features/search/*.test.tsx`
- Modify: `src/app/App.tsx`（搜索模式切换、布局接入）
- Modify: `src/features/detail/DetailPanel.tsx`、`ImageOverlay.tsx`（真实摘要 + 来源徽标 + OCR 高亮 overlay）
- Modify: `src/features/library/ItemCard.tsx`（命中徽标、摘要首行；hue-dot 在 Task 14）
- Modify: `src/features/shell/StatusBar.tsx`（`pipeline_status` / `semantic_status`）
- Modify: `src/lib/api.ts`、`events.ts`、`queryClient.ts`

**Interfaces:**
- Consumes: `commands.searchItems` / `searchFeedback`（Task 12 生成）、新事件
- Produces: 搜索模式 UI 全链路；`api.searchItems(raw)` / `api.sendFeedback(...)`；`queryKeys.search(raw)`

- [ ] **Step 1: useSearch**

防抖 250ms；`seqRef` 请求序号（后发先至丢弃）；空查询不发请求并切回列表模式；`isFetching` 供 UI；结果缓存进 TanStack Query（key 含 raw）。

- [ ] **Step 2: SearchBar / QueryChips / ResultGrid / ExplainBar**

- SearchBar：输入框 + 清除按钮 + 提交提示（Enter 立即搜索）；`aria-label="搜索素材"`。
- QueryChips：`parsed.chips` 渲染，× 删除后从 raw 中移除对应片段重算（由 `useSearch` 维护「raw 减去已删 chip」的当前查询串）。
- ResultGrid：复用 `ItemGrid`/`ItemCard`；卡片追加命中徽标（`terms` 命中词前 3、或「语义」、或「筛选」）与摘要首行（`item.primaryColor` 留 Task 14）。
- ExplainBar：选中卡片的 `explanation` 详情：filters 列表、terms 列表、semantic 标注；图片素材显示「在图中高亮」开关。
- DetailPanel：摘要区接 `extras.summary` + 来源徽标（AI 摘要/OCR 摘录/原文摘录/无）；`ImageOverlay` 在开关开启时按 `highlightBoxes`（归一化 → CSS 百分比）叠加半透明框。
- StatusBar：`pipeline_status` 显示「后台索引中（N）」；`semantic_status` false 显示「语义检索未就绪（FTS 模式）」。

- [ ] **Step 3: api.ts / events.ts**

`searchItems`、`sendFeedback`；事件分派：`item_indexed`/`item_enriched` → `resetQueries(items)` + `invalidateQueries(search)`（+detail）；`pipeline_status`/`semantic_status` → 状态栏局部状态（不进 Query）。

- [ ] **Step 4: 测试**

- 防抖与过期丢弃（fake timers）；空查询不调 `searchItems`。
- chips 删除后重发查询且 raw 不含已删片段。
- 三类命中的徽标与 ExplainBar 内容；OCR 高亮开关渲染 N 个框（百分比定位）。
- 摘要来源徽标四态；复制按钮启用。
- `item_indexed` 事件后搜索缓存失效。

**Verify:**

```bash
pnpm test -- src/features/search && pnpm typecheck && pnpm lint
```

**Commit:** `feat(ui): add search experience with chips, explain bar and ocr highlights`

---

### Task 14: 标签云筛选、hue-dot 与模型设置 pane

**Files:**
- Modify: `src/features/shell/Sidebar.tsx`（标签点击筛选）
- Modify: `src/features/library/ItemCard.tsx`（hue-dot）
- Modify: `src/features/settings/SettingsDialog.tsx`（模型 pane）
- Modify: `src/app/App.tsx`（标签筛选状态与 SearchBar 联动）
- Modify: 各自 `*.test.tsx`

**Interfaces:**
- Consumes: `api.getModelConfig` / `setModelConfig` / `probeModel`；`ItemDto.primaryColor`；`FilterChipDto(kind="tag")`
- Produces: P3-R10 / P3-R11 的全部 UI

- [ ] **Step 1: 标签云筛选**

标签 pill `onClick` → App 状态 `activeTag` → 注入搜索上下文（`tag:<name>` 进查询或直接作为 tag chip）→ `useSearch` 触发；再次点击/删 chip 取消。移除阶段 02 的 `title="阶段 03 支持按标签筛选"`。

- [ ] **Step 2: hue-dot**

`primaryColor` 非空 → 卡片角落 8px 圆点，色值来自前端固定色板（枚举 → hex，与 VLM 枚举一一对应）；为空不渲染。色板常量放 `src/lib/colors.ts`（单一来源）。

- [ ] **Step 3: 模型 pane**

「模型」新 pane：主/备两组表单（base URL、模型名、API Key 密码框——保存后只显示「已设置」）；「探测」按钮 → `probeModel` → 行内结果（延迟/视觉能力/错误信息）；保存 → `setModelConfig` → 立即生效提示。表单校验：base URL 必须以 `http(s)://` 开头。**密钥输入框在保存成功后清空**，界面永不回显。

- [ ] **Step 4: 测试**

- 点标签 → 出现 tag chip → 删除恢复；`activeTag` 不污染普通搜索。
- 有/无 primaryColor 圆点渲染；「橙」色查询命中素材圆点为橙（数据注入）。
- 模型 pane：探测 mock 三态；保存调用 `setModelConfig` 且参数无 key 明文残留（断言 password 框已清空）；URL 非法行内报错。

**Verify:**

```bash
pnpm test -- src/features/shell src/features/settings && pnpm typecheck && pnpm lint
```

**Commit:** `feat(ui): add tag filter, hue dots and model settings pane`

---

### Task 15: 评测工具与门禁

**Files:**
- Create: `crates/eval/Cargo.toml`、`src/main.rs`
- Create: `docs/eval/queries.template.jsonl`（格式模板与说明，入库）
- Create: `docs/eval/report-2026-11-XX.md`（首轮报告，评测集本体本地保存不入库）

**Interfaces:**
- Consumes: `search::execute`、`library`、`ai`（真实嵌入）、假 VLM 不参与（评测只测检索，不重跑 VLM）
- Produces: `cargo run -p visrecall-eval -- --db <path> --queries <jsonl> --report <md>`

- [ ] **Step 1: 评测集准备**

50 条真实中文查询（分桶：截图类 ≥15 / 场景类 ≥15 / 字面类 ≥15，可交叉），对 20 条验收库人工标注相关素材 id。JSONL：`{ "query": "...", "relevant": ["id"...], "bucket": "screenshot|scene|literal" }`。模板与标注口径（「相关」的定义：能回答该查询意图的素材）写入 `docs/eval/queries.template.jsonl` 头部注释。

- [ ] **Step 2: 指标实现**

Top-3 命中率（Top-3 含 ≥1 相关）；Recall@10（每查询相关素材被召回比例的均值）；mAP（AP 按相关位次）；P95 `took_ms`；通道分布（explanation 聚合：FTS 命中 / 语义参与 / 仅过滤）；分桶上述全部。输出 Markdown 报告（含环境、库条数、模型 id、RRF 权重快照）。

- [ ] **Step 3: 门禁与调参回路**

首跑记录基线 → 若 Top-3 <80%：按回退顺序调（jieba 词典 → RRF 权重 → 记录是否需要 base-zh 升级评审），每轮变更出对比报告（报告内含 diff 摘要）。**门禁结论写入 `verification.md` §2 P3-R14 行**。

- [ ] **Step 4: 冒烟**

对空库跑（0 命中不崩溃）；对 3 条素材的小库跑通全流程。

**Verify:**

```bash
cargo run -p visrecall-eval -- --db <test-db> --queries docs/eval/queries.template.jsonl --report /tmp/smoke.md
cargo clippy -p visrecall-eval -- -D warnings
```

**Commit:** `feat(eval): add retrieval evaluation tool and first report`

---

### Task 16: 阶段验收与门禁回填

**Files:**
- Create: `docs/specs/v0.1/03-ai-search/verification.md` 全部结果（模板已建，本任务回填）
- Modify: `docs/specs/v0.1/01-capture-loop/verification.md`（如阶段 01 清单有新结论）
- Modify: `docs/specs/v0.1/README.md`（阶段 03 状态、门禁结论）

**Interfaces:**
- Consumes: Task 1–15 全部产物
- Produces: 可审计的验收记录与状态更新

- [ ] **Step 1: 跑完整自动化门禁**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test --workspace -- --ignored     # 真实模型用例（本地必跑）
pnpm lint && pnpm typecheck && pnpm test && pnpm build
```

- [ ] **Step 2: 构造 20 条验收数据并执行手工矩阵**

按 `verification.md` §0.1 构造（14 捕获 + 6 拖入 + 标签/备注/缺口处置）；执行 §2 验收矩阵：OCR 准确率抽样、新鲜度 10 次采样 P95、检索延迟采样、降级矩阵逐行（断网/错误 Key/删模型文件/无文字图）、蓝绿重建、凭据管理器核对、GUI 目视与截图。

- [ ] **Step 3: 阶段 02 遗留补测**

20 条数据视觉验收与截图证据（阶段 02 L1/L2）；仍受环境限制的项如实标注，不静默勾选。

- [ ] **Step 4: 回填文档**

- `verification.md`：§1 门禁（T0 数字已回填）、§2 矩阵逐行结论 + 证据、§3 命令输出、§4 已知限制、§5 结论。
- `README.md`：阶段 03 状态与 §1 清单结论。

**Verify:**

```bash
git diff --stat docs/
grep -c "\[ \]" docs/specs/v0.1/03-ai-search/tasks.md   # 未勾任务数应为 0
```

**Commit:** `docs(spec): record phase 03 verification results`
