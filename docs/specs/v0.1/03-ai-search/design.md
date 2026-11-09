# 阶段 03：AI 与检索 · 设计

> 对应需求：`requirements.md` P3-R01–P3-R15
> 技术依据：《VisRecall-阶段三-AI与检索架构设计 v1.0（定稿 2026-11-08）》——存储/管线/检索链路的选型论证（§2–§8）以该文档为准，本文做工程落地细化；《VisRecall-技术架构设计 v1.2》§5.2、§5.3、§6、§7
> 目标：新增 `ai` / `pipeline` / `search` 三个 crate 与 `library` 迁移 0003，打通「两阶段索引 + 三路召回 + RRF + 命中解释」；查询路径零网络；不引入任何第二存储引擎与服务进程。

## 1. 设计结论

### 1.1 阶段决策（已定稿 2026-11-08）

选型决策沿用架构文档 §0 决策表原文，不在此重复论证，只列索引：

| ID | 决策 | 出处 |
|---|---|---|
| A1 | SQLite 全家桶（FTS5 + 向量表），不引入 Qdrant/PG/LanceDB | 架构 §4.1 |
| A2 | 普通表 int8 BLOB 暴力余弦为主路径；vec0 仅可选快路径 | 架构 §4.2 |
| B1 | jieba 预分词 + FTS5 BM25（unicode61）；不自定义分词器 | 架构 §5.3 |
| B2 | bge-small-zh-v1.5（512 维 int8，ort CPU）；base-zh 评测驱动升级 | 架构 §5.2 |
| B3 | PP-OCRv5/v4 mobile ONNX 主路径 + Windows.Media.Ocr 兜底 | 架构 §5.1 |
| C1 | 两阶段索引：OCR→临时嵌入→可检索（≤3s P95）；VLM 完成后增强重建 | 架构 §5.4 |
| C2 | 复用 jobs 表 + 2 worker + 指数退避，不引入外部队列 | 架构 §5.5 |
| C3 | revision 幂等 + `model_id` 固定于库 + 向量表蓝绿重建 | 架构 §5.6 |
| D1 | 纯本地规则查询解析，chip 回显，查询路径零大模型 | 架构 §6.1 |
| D2 | 三路召回（SQL 过滤 + FTS BM25 + 语义向量）；无 CLIP（不做图搜图，含二期） | 架构 §6.2 |
| D3 | 应用层 RRF（k=60），权重配置化，评测校准 | 架构 §6.3 |
| D4 | Explanation 结构 + OCR 高亮框映射 | 架构 §6.4 |
| E1 | 50 条评测集，Top-3 ≥80% 门禁，分桶统计 | 架构 §7 |

本阶段新增的**落地层决策**（架构文档未细化、实施前必须定的）：

| ID | 事项 | 结论 | 理由与代价 |
|---|---|---|---|
| D3-01 | Provider trait 归属 | `EmbeddingProvider` / `OcrProvider` / `VisionProvider` / `TextProvider` / `VectorIndex` 定义在 **`crates/domain`**（新模块 `providers.rs`），实现在 `crates/ai` / `crates/library` | 架构文档要求「search 不依赖 ai、查询嵌入由 app-core 注入 trait 对象」；trait 放 domain 才能让 search 只见 trait 不见 ai。domain 零外部依赖，不受污染 |
| D3-02 | jobs 阶段划分 | `ocr`（仅图片）→ `index`（嵌入+FTS+insights 同事务写）→ `vlm`；另有 `rebuild`（蓝绿/修复） | 架构文档提「embed 与 fts 任务」——两者本就同事务写入，拆成两个 job 只会制造半更新状态；合并为 `index` 后 revision 失效语义也更简单 |
| D3-03 | revision 真相来源 | 迁移 0003 给 `items` 加 `revision INTEGER NOT NULL DEFAULT 0` 列，`Library::bump_revision(id)` 递增并作为 jobs.revision | 架构文档只说「元数据变更递增 revision」；`items.updated_at` 是毫秒时间戳，同毫秒两次变更会撞唯一键，显式计数列可测可读 |
| D3-04 | 模型文件布局 | 仓库 `models/` 目录 + `models/manifest.json`（sha256 清单）；运行时按「exe 资源目录 → 仓库 `models/`（dev）」顺序解析 | 单文件可复现、离线可用；NSIS 安装包内嵌留阶段 04，本阶段 release 从 resource dir 读 |
| D3-05 | 查询嵌入生命周期 | 启动即后台预热加载；未就绪期间 `search_items` 的语义通道跳过并置 `semantic_ready=false`（AppStatus + `semantic_status` 事件）；LRU 256 查询缓存 | 首查不等模型（3s 内不可接受）；降级路径与架构 §6.6 一致 |
| D3-06 | 评测工具形态 | 独立 bin crate `crates/eval`（`cargo run -p visrecall-eval`），依赖 search/library/ai 真实实现 | 评测必须走生产代码路径才有效；bin crate 不进 `src-tauri` 依赖图，不影响产物体积 |
| D3-07 | Provider 配置存储 | base URL / 模型名存 `config.json`（机器级，跟安装走）；API Key 只存 Windows 凭据管理器（`keyring`，account 分 `vlm-primary` / `vlm-backup`） | Key 是机器级秘密，绝不入库、绝不进 config；provider 端点是机器偏好而非库属性（与阶段 02 D2-05 的 settings 表分工相反） |
| D3-08 | OCR 方向分类 | PP-OCR 只跑 det+rec 两模型，不引入 cls | 截图场景文字基本水平；cls 省一个模型与一次推理；旋转文本属长尾，登记 design §1.2 |

### 1.2 与架构文档的已知偏差/澄清

| 偏差/澄清 | 架构文档原文 | 本阶段做法 | 收口 |
|---|---|---|---|
| FTS 删除语法 | §4.4「FTS 需显式删除指令 `INSERT INTO items_fts(items_fts, ...) VALUES('delete',...)`」 | `items_fts` 为 **contentful** 表（§4.3 已定），删除用普通 `DELETE FROM items_fts WHERE rowid=?` 同事务执行；`'delete'` 指令语法仅适用于 contentless/external-content 表 | 澄清（修正笔误），非偏差 |
| embed/fts 任务拆分 | §5.5「合并入队 `embed` 与 `fts` 任务」 | 合并为单一 `index` 阶段（D3-02） | 澄清 |
| 嵌入空闲释放 | §3.1「模型懒加载、空闲释放」 | 只做懒加载+启动预热（D3-05）；空闲卸载留给阶段 04 内存优化 | 阶段 04 |
| 引号短语 | §5.3「P1 加 trigram 表做引号短语」 | 本阶段不做 trigram；引号内容按必命中词组（AND）处理 | 阶段 04 或评测驱动 |
| vec0 | §4.2「可选快路径」 | 迁移 0003 不建 vec0 表；T0 spike 只验证编译与结果一致性，不接线 | 规模需要时（迁移 0004） |
| fts.tokenizer_version | §4.3 settings 键 | 记录 + 不一致 `warn`；不做自动全量重建 | 真需要时复用 rebuild 通道 |
| PP-OCR cls | §5.1「PP-OCRv5/v4 mobile ONNX」 | det+rec 两模型，无 cls（D3-08） | 长尾场景复评 |
| VLM 用户编辑 | §4.3 `summary_user_edited`/`tags_user_edited` | 字段落地、默认 0；编辑 UI 与保护逻辑不做 | V0.5 |

## 2. 起点：既有资产

### 2.1 阶段 01/02 已提供（直接复用，不改语义）

| 能力 | 位置 | 本阶段用法 |
|---|---|---|
| `items` / `jobs`（`UNIQUE(item_id,stage,revision)`）/ `settings` 表 | `crates/library/migrations/0001` | jobs 队列底座；settings 存 `embedding.*` / `vector.active_table` / `search.rrf` |
| `jobs.running→queued` 启动恢复、`tmp/` 清理 | `crates/library/src/db.rs`、`cleanup.rs` | pipeline 崩溃恢复直接受益 |
| tags / item_tags（origin）/ tag_suppressions | `crates/library/src/tags.rs` | VLM 自动标签走 `tag_add` 内部路径，suppressions 过滤 |
| 迁移前备份（`.bak-<version>`） | `crates/library/src/migrations.rs` | 0003 迁移自动受保护 |
| 删除级联（jobs 显式 + tags FK） | `crates/library/src/purge.rs` | 0003 起追加 insights/向量 FK 级联 + FTS 显式删除 |
| 事件总线（`EventSink` / `RecordingSink`） | `crates/app-core/src/events.rs` | 新增 4 个事件变体 |
| `run_blocking` IPC 模式、`tauri-specta` 绑定 | `src-tauri/src/ipc.rs` | 新命令沿用 |
| 捕获/导入提交路径 | `app-core/src/capture_flow.rs` / `import_flow.rs` | 提交成功后入队 ocr/index/vlm |
| `visrecall://thumb/<id>` / `/original/<id>` | `src-tauri/src/protocol.rs` | OCR 高亮框叠加在既有预览上 |
| 设置弹窗 pane 结构、标签云、状态栏 | `src/features/settings` / `shell` | 模型 pane、筛选点击、降级标注 |

### 2.2 阶段 02 留出的钩子

- `note_set` / `tag_add` / `tag_detach` / `tag_rename`：阶段 02 明确「阶段 03 的 FTS 重建会在这些方法里追加入队」——本阶段在这些写路径 `bump_revision` + 入队 `index`。
- `DetailExtrasDto.summary` 恒 `None`：本阶段接真实 `insights.summary` + `summary_source` 徽标。
- 侧栏标签 pill 无 `onClick`（`title="阶段 03 支持按标签筛选"`）：本阶段接筛选。

## 3. 模块与文件结构

```text
crates/domain/src/
  providers.rs          # 新增：5 个 Provider trait + OcrOutput / VisionInsight / TextInsight
  insight.rs            # 新增：Insight / OcrBox / SummarySource
  search.rs             # 新增：ParsedQuery / FilterKind / TimeRange / Explanation / ScoredItem
  vector_math.rs        # 新增：int8 量化/反量化/余弦（纯函数）
  error.rs              # 修改：ModelNotReady / ProviderConfig / SchemaViolation 等变体
  lib.rs                # 修改：re-export

crates/ai/（新增 crate：visrecall-ai）
  src/embedder.rs       # bge-small-zh int8（ort + tokenizers，懒加载，LRU 查询缓存）
  src/ocr/
    mod.rs              # OcrProvider 选择链（PP-OCR → Windows.Media.Ocr）
    ppocr.rs            # det+rec ONNX 推理、包围盒、置信度
    windows_ocr.rs      # Windows.Media.Ocr 兜底
  src/vlm/
    mod.rs              # Vision/TextProvider：OpenAI 兼容 chat/completions
    schema.rs           # 严格 JSON 校验 + 修复一次
  src/models.rs         # 模型文件定位（resource dir → 仓库 models/）、sha256 校验
  src/keyring.rs        # API Key 读写删（vlm-primary / vlm-backup）
  examples/             # T0 spike：ocr_spike.rs / embed_spike.rs（门禁取证用）

crates/pipeline/（新增 crate：visrecall-pipeline）
  src/worker.rs         # 2 worker 循环：claim / 退避 / stale-revision 取消 / 优先级
  src/handlers/
    ocr.rs              # 阶段一前半：图片 OCR → insights.ocr_*
    index.rs            # 阶段一后半+重建：search doc → 嵌入 → 同事务写三处
    vlm.rs              # 阶段二：VLM/Text → 摘要/标签/属性 → re-index
    rebuild.rs          # 蓝绿重建 / 启动修复
  src/lifecycle.rs      # revision、content_hash 复用、自检、蓝绿重建
  tests/                # 假 Provider 全链路集成测试

crates/search/（新增 crate：visrecall-search）
  src/tokenizer.rs      # jieba 封装 + TOKENIZER_VERSION（唯一分词入口）
  src/document.rs       # search document 构造（索引侧）
  src/parser.rs         # 查询解析（规则表，proptest）
  src/filters.rs        # ParsedQuery → SQL WHERE 片段与参数
  src/fts_query.rs      # 词 → MATCH 表达式（转义、AND/OR）
  src/rank.rs           # RRF（k=60、权重、平局）
  src/execute.rs        # 三路召回编排 + Explanation 装配 + OCR 框映射
  tests/                # tempdir + 真实 SQLite 的检索集成测试

crates/eval/（新增 bin crate：visrecall-eval）
  src/main.rs           # --db --queries --report；Recall@3/@10、mAP、P95、分桶、通道分布

crates/library/
  migrations/0003_ai_search.sql        # 新增
  src/insights.rs       # 新增：insights CRUD
  src/fts.rs            # 新增：items_fts 写/删/搜（BM25 + snippet）
  src/vectors.rs        # 新增：向量表读写 + 暴力余弦（VectorIndex 实现）+ 蓝绿指针
  src/queries_log.rs    # 新增：queries 表读写
  src/items.rs          # 修改：revision 列、bump_revision、primaryColor JOIN
  src/purge.rs          # 修改：删除事务补 FTS 显式删除
  src/db.rs             # 修改：启动自检（行数比对）
  src/lib.rs            # 修改：导出

crates/app-core/src/
  pipeline_host.rs      # 新增：worker 生命周期、Provider 注册表（主备/健康）、预热
  search_flow.rs        # 新增：查询嵌入（缓存）→ search::execute 编排
  app.rs                # 修改：启动接线（自检修复、预热）、模型配置
  events.rs             # 修改：4 个新事件
  capture_flow.rs / import_flow.rs      # 修改：提交后入队

src-tauri/src/
  ipc.rs                # 修改：search_items / search_feedback / model_* / embedding_rebuild + DTO
  main.rs               # 修改：注册新命令

src/
  features/search/      # 新增：SearchBar / QueryChips / ResultGrid / ExplainBar / useSearch
  features/detail/DetailPanel.tsx       # 修改：真实摘要 + 来源徽标 + OCR 高亮 overlay
  features/shell/Sidebar.tsx            # 修改：标签点击筛选
  features/library/ItemCard.tsx         # 修改：hue-dot、命中徽标、摘要首行
  features/settings/SettingsDialog.tsx  # 修改：模型 pane
  lib/api.ts / events.ts / queryClient.ts  # 修改
```

**依赖方向（单向，无环）：**

```text
src-tauri → app-core → ┬ pipeline → ai ─┐
                       │               ├→ domain
                       │     search ───┤
                       │        ↓      │
                       │      library ─┘
                       └ search（查询嵌入经 domain trait 注入，不依赖 ai）
eval → search + library + ai + pipeline（开发工具，不被生产依赖）
```

关键约束（对齐架构 §3.1）：`search` 不依赖 `ai`（查询嵌入通过 app-core 注入的 `Arc<dyn EmbeddingProvider>`）；`pipeline` 依赖 `search` 仅用 `document.rs`/`tokenizer.rs`（索引侧文本构造与查询侧必须同源，jieba 只在这一处 crate 出现）；`ai` 纯推理与 HTTP，不碰 DB。

## 4. 数据模型

### 4.1 迁移 0003（`crates/library/migrations/0003_ai_search.sql`）

```sql
-- 元数据 revision：jobs.revision 的真相来源（D3-03）。
ALTER TABLE items ADD COLUMN revision INTEGER NOT NULL DEFAULT 0;

-- 1) AI 解析结果（架构文档 §4.3 原样）
CREATE TABLE insights (
  item_id TEXT PRIMARY KEY REFERENCES items(id) ON DELETE CASCADE,
  summary TEXT,
  summary_source TEXT NOT NULL DEFAULT 'none'
    CHECK (summary_source IN ('none','vlm','ocr','extractive','user')),
  content_type TEXT, primary_color TEXT, chart_type TEXT, ui_type TEXT,
  entities_json TEXT,
  ocr_text TEXT, ocr_boxes_json TEXT, ocr_engine TEXT, ocr_confidence REAL,
  latex TEXT, has_formula INTEGER NOT NULL DEFAULT 0,
  model_provider TEXT, model_name TEXT, confidence REAL,
  summary_user_edited INTEGER NOT NULL DEFAULT 0,
  tags_user_edited INTEGER NOT NULL DEFAULT 0,
  updated_at INTEGER NOT NULL
);
CREATE INDEX idx_insights_color ON insights(primary_color);
CREATE INDEX idx_insights_chart ON insights(chart_type);
CREATE INDEX idx_insights_type  ON insights(content_type);

-- 2) 全文索引：contentful（保留分词后正文以支持 snippet()/highlight()）。
--    写入前由应用层 jieba 分词、空格连接；rowid 与 items.rowid 对齐
--    （items 为 INTEGER PRIMARY KEY AUTOINCREMENT，rowid 稳定不复用）。
CREATE VIRTUAL TABLE items_fts USING fts5(
  title, summary, ocr_text, note, tags, entities, original_text,
  tokenize = 'unicode61 remove_diacritics 2'
);

-- 3) 向量主表（int8 暴力路径；蓝绿重建时建 *_v2 后切 vector.active_table 指针）
CREATE TABLE item_vectors_int8 (
  item_rowid INTEGER PRIMARY KEY REFERENCES items(rowid) ON DELETE CASCADE,
  embedding  BLOB NOT NULL,
  dim        INTEGER NOT NULL,
  model_id   TEXT NOT NULL
);

-- 4) 检索记录（评测与反馈数据；无历史 UI）
CREATE TABLE queries (
  id TEXT PRIMARY KEY,
  raw TEXT NOT NULL,
  parsed_json TEXT,
  result_ids_json TEXT,
  feedback TEXT,
  created_at INTEGER NOT NULL
);
```

要点：

1. `insights.item_id` / `item_vectors_int8.item_rowid` 均带 `ON DELETE CASCADE`（`items.id` UNIQUE、`items.rowid` 即 PK，外键合法）；删除事务里只剩 FTS 需要显式 `DELETE FROM items_fts WHERE rowid=?`（contentful 表，见 §1.2 澄清）。
2. 迁移前备份机制（阶段 02 已建）自动覆盖 0003：应用 0003 前先 `backups/visrecall.db.bak-3`。
3. FTS5 可用性在 T0 spike 确认（rusqlite bundled 已启用 FTS5 编译）。
4. 蓝绿期间可能同时存在 `item_vectors_int8_v2` 等表——蓝绿表与主表**同构**（含 FK 级联），删除素材时两张表同时清理，互不残留；旧表数据仅为回滚快照保留，下一次重建覆盖同名表前先 DROP。

### 4.2 settings 键（`settings_kv`）

| key | 值 | 默认 | 校验 |
|---|---|---|---|
| `embedding.model_id` | `"bge-small-zh-v1.5-int8"` | 首次写入固定 | 变更必须走蓝绿重建（P3-R12） |
| `embedding.dim` | `512` | 首次写入固定 | 与 model_id 一致 |
| `fts.tokenizer_version` | `search::TOKENIZER_VERSION` | 写入时取当前 | 不一致 `warn`（§1.2） |
| `vector.active_table` | `"item_vectors_int8"` | 同左 | 只允许 `item_vectors_int8` 或 `item_vectors_int8_v<N>` |
| `search.rrf` | `{"k":60,"weights":{"fts":1.0,"semantic":1.0}}` | 同左 | k 1..=1000，权重 0..=10 |

### 4.3 领域类型（摘录；完整签名见 tasks）

```rust
// crates/domain/src/providers.rs
#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    fn id(&self) -> &str;                          // 写入 model_id
    fn dim(&self) -> usize;
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, AppError>;
    fn ready(&self) -> bool;                       // 懒加载是否完成（D3-05）
}

pub struct OcrOutput { pub text: String, pub boxes: Vec<OcrBox>, pub confidence: f32 }
pub trait OcrProvider: Send + Sync {
    fn id(&self) -> &'static str;                  // "ppocr-mobile" | "windows-ocr"
    fn recognize(&self, png: &[u8]) -> Result<OcrOutput, AppError>;
}

pub struct VisionInsight { /* summary, tags, content_type, primary_color,
                              chart_type, ui_type, entities, latex, has_formula, confidence */ }
#[async_trait]
pub trait VisionProvider: Send + Sync {
    fn id(&self) -> &str;
    async fn analyze(&self, png: &[u8]) -> Result<VisionInsight, AppError>;
}
#[async_trait]
pub trait TextProvider: Send + Sync {
    fn id(&self) -> &str;
    async fn summarize(&self, text: &str) -> Result<TextInsight, AppError>;
}

/// 检索与实现解耦：暴力 int8（V0.1）→ vec0 → HNSW（架构附录 B）。
pub trait VectorIndex: Send + Sync {
    fn add(&self, rowid: i64, vec8: &[u8]) -> Result<(), AppError>;
    fn remove(&self, rowid: i64) -> Result<(), AppError>;
    fn search(&self, query: &[f32], top_k: usize,
              candidates: Option<&[i64]>) -> Result<Vec<(i64, f32)>, AppError>;
}
```

```rust
// crates/domain/src/search.rs
pub struct TimeRange { pub start_ms: i64, pub end_ms: i64 }
pub struct ParsedQuery {
    pub time: Option<TimeRange>,
    pub app: Option<String>,            // 小写归一
    pub color: Option<Vec<String>>,     // 同义词可映射多枚举，如 深色 → [black, gray]
    pub chart: Option<String>,
    pub content_kind: Option<ItemKind>,
    pub has_formula: Option<bool>,
    pub tags: Vec<String>,
    pub semantic_text: String,          // 剩余文本
    pub lexical_terms: Vec<String>,     // jieba(semantic_text) 去停用词
    pub quoted_terms: Vec<String>,      // 引号内分词，AND 语义
}
pub struct Explanation {
    pub filters: Vec<String>,           // 实际命中的结构化条件
    pub terms: Vec<String>,             // FTS 实际命中词
    pub semantic: bool,                 // 仅语义命中
    pub highlight_boxes: Vec<OcrBox>,   // OCR 命中词 → 归一化框
}
pub struct ScoredItem { pub item: Item, pub score: f32, pub explanation: Explanation }
```

```rust
// crates/domain/src/vector_math.rs
pub fn quantize_i8(vec: &[f32]) -> Vec<u8>;        // round(x * 127 / max|x|)，余弦对逐向量缩放不变
pub fn cosine_i8(a: &[u8], b: &[u8]) -> f32;       // int8 点积 / 模长（SIMD 友好的字节循环）
```

### 4.4 分词与 search document（`crates/search`）

- `tokenizer.rs` 是**唯一** jieba 入口：`tokenize_for_fts(&str) -> String`（切分、过滤空白/单符号、空格连接）、`terms_for_query(&str) -> Vec<String>`；`pub const TOKENIZER_VERSION: &str = "jieba-rs-0.7"`。写入侧与查询侧共用，保证词形一致。
- `document.rs`：`build_search_document(fields) -> String`，字段顺序 `标题 > 摘要 > 标签 > 实体 > 备注 > OCR/原文`，按 tokenizers 的 512 预算截断（近似：按字符预算 ≈ 1600 汉字保守截断，T0 校准后固定常量）。**字段顺序即权重**——截断时排序靠前的字段必然保留。
- 颜色同义词表（parser 与 VLM 输出共用的枚举）：`red/orange/yellow/green/blue/purple/pink/brown/black/white/gray/multicolor`；查询侧映射：红→red、橙→orange、深色→[black,gray]、浅色→[white,yellow] 等。

### 4.5 VLM JSON 契约（`crates/ai/src/vlm/schema.rs`）

```json
{
  "summary": "≤120 字中文摘要",
  "tags": ["≤8 个，无停用词"],
  "content_type": "screenshot | photo | diagram | document | other",
  "primary_color": "red | orange | yellow | green | blue | purple | pink | brown | black | white | gray | multicolor",
  "chart_type": "bar | line | pie | funnel | table | none",
  "ui_type": "dialog | settings | list | article | chat | code | other",
  "entities": ["人名/产品/公司等"],
  "latex": "公式 LaTeX，无则空串",
  "has_formula": false,
  "confidence": 0.85
}
```

校验：serde 反序列化为强类型（未知字段忽略）+ 枚举合法性 + `summary` 长度上限；失败时把 serde 错误与原文回传模型请求**修复一次**；仍失败 → `SchemaViolation` 错误，按 Provider 失败处理（重试/主备）。提示词要求「只输出 JSON，不要 Markdown 代码块」，并剥离可能的 ```` ```json ```` 包裹。

## 5. IPC 契约

### 5.1 命令（6 条新增，2 条扩展）

| 命令 | 入参 | 返回 | 说明 |
|---|---|---|---|
| `search_items` | `raw: String, limit: Option<u32>` | `SearchResponseDto` | 全链路检索；limit 默认 50、clamp 1..=200 |
| `search_feedback` | `queryId, itemId, signal` | `()` | signal 目前仅 `"not-this"`；写 `queries.feedback`（P1 UI，命令先行） |
| `model_config_get` | — | `ModelConfigDto` | 不含密钥明文，只含 `hasApiKey` |
| `model_config_set` | `patch: ModelConfigPatch` | `ModelConfigDto` | Key 非空 → 写 keyring；空串 → 清除该槽位；保存即热生效 |
| `model_probe` | `slot: String` | `ProbeReportDto` | 后端发文本 ping + 1×1 PNG 视觉请求 |
| `embedding_rebuild` | — | `()` | 管理/评测用（无 UI）：触发向量蓝绿重建 |
| `get_app_status`（扩展） | — | `AppStatus` | 新增 `semanticReady: bool` |
| `list_items` / `get_item` / `get_item_ext`（扩展） | — | `ItemDto` / `ItemDetailDto` | `ItemDto` 新增 `primaryColor: string \| null`（LEFT JOIN insights） |

### 5.2 DTO

```rust
pub struct SearchResponseDto {
    pub query_id: String,
    pub took_ms: u32,
    pub parsed: ParsedQueryDto,          // chips 回显
    pub semantic_used: bool,             // 语义通道是否参与（false=降级）
    pub hits: Vec<SearchHitDto>,
}
pub struct ParsedQueryDto { pub chips: Vec<FilterChipDto>, pub semantic_text: String }
pub struct FilterChipDto { pub kind: String, pub value: String, pub label: String }
// kind ∈ time|app|color|chart|contentType|hasFormula|tag；label 为中文展示（「上个月」「橙色」「来自 chrome」）

pub struct SearchHitDto {
    pub item: ItemDto,
    pub score: f32,                      // RRF 融合分（解释用，非绝对相关度）
    pub explanation: ExplanationDto,
}
pub struct ExplanationDto {
    pub filters: Vec<String>,
    pub terms: Vec<String>,
    pub semantic: bool,
    pub highlight_boxes: Vec<OcrBoxDto>, // { text, x, y, w, h } 归一化 0..1
}

pub struct ModelConfigDto {
    pub primary: Option<ProviderConfigDto>,
    pub backup: Option<ProviderConfigDto>,
    pub embedding_ready: bool,
}
pub struct ProviderConfigDto {
    pub base_url: String,
    pub model: String,
    pub has_api_key: bool,
    pub vision_capable: Option<bool>,    // 最近一次探测结果，未探测为 None
}
pub struct ModelConfigPatch {            // None=不动；Some("")=清除
    pub primary: Option<ProviderEntryInput>,
    pub backup: Option<ProviderEntryInput>,
}
pub struct ProviderEntryInput { pub base_url: String, pub model: String, pub api_key: Option<String> }
pub struct ProbeReportDto { pub ok: bool, pub latency_ms: u32, pub vision: bool, pub message: Option<String> }
```

### 5.3 事件（4 个新增）

| 事件 | 载荷 | 前端处理 |
|---|---|---|
| `item_indexed` | `{ id }` | 阶段一完成（可检索）：`resetQueries(items)` + `invalidateQueries(search)` |
| `item_enriched` | `{ id }` | 阶段二完成：同上 + `invalidateQueries(detail(id))` |
| `pipeline_status` | `{ queued, running }` | 状态栏「后台索引中（N）」；0/0 时消失 |
| `semantic_status` | `{ ready }` | 状态栏「语义检索未就绪（FTS 模式）」标注的显隐 |

状态变化（`parsing→ready/pending/failed`）沿用既有 `item_updated`；自动标签沿用 `item_tags_updated` + `tags_changed`。

## 6. 关键流程

### 6.1 入库两阶段索引

```text
提交截图/文本（capture_flow / import_flow 提交成功后）
  ├─ 图片：enqueue ocr(rev=items.revision) ──► vlm
  └─ 文本/链接：enqueue index ──► vlm
        （ocr 完成、index 完成、vlm 完成各自独立入队下一步，见 handlers）

handlers::ocr（仅图片）
  OcrProvider.recognize(png)
    ├─ 成功且有文本 → 同事务写 insights.ocr_*（含 boxes）→ enqueue index
    ├─ 成功但无文本（无字图片——正常结果，不是失败）→ insights 建行：ocr_text=NULL、boxes=[]、
    │    ocr_confidence=NULL（**不是 0**：「没有文字」≠「识别得差」，写 0 会误触 <0.6 的
    │    「OCR 置信度低」标注）→ index / vlm 照常入队；该素材的检索走 VLM 摘要（语义）+
    │    结构化属性（过滤）+ 标题/备注（字面），离线时仅元数据/备注可搜（架构 §6.2 已接受的取舍）
    └─ 引擎失败（PP-OCR → Windows OCR 两级都 Err）→ insights 仅建行（无 ocr 字段）→ index 照常（原文空）

handlers::index（阶段一收口 + 重建共用）
  doc = search::build_search_document(title, summary, tags, entities, note, ocr/原文)
  vec = EmbeddingProvider.embed([doc])            // bge-small int8
  BEGIN
    UPSERT insights（阶段一时 summary=OCR 摘录前 280 字符，summary_source='ocr'；文本素材 source='extractive' 取原文前 280 字符；
                     OCR 文本为空/缺失时不写占位——summary 保持 NULL、source 保持 'none'，详情显示「待解析」，标题回退来源窗口标题。空串占位会让「无摘要」与「OCR 摘录为空」不可区分）
    UPSERT items_fts(rowid=items.rowid, 各列=jieba 分词后文本)
    UPSERT 向量表(item_rowid, quantize_i8(vec), dim, model_id)
  COMMIT
  emit item_indexed { id }                        // 此刻起三路可检索

handlers::vlm（阶段二）
  VisionProvider.analyze(png) / TextProvider.summarize(原文)
    ├─ 成功 → 严格 schema 校验 → 同事务写 insights 全字段
    │        → 自动标签（过滤 suppressions、复用 tags、origin='auto'）
    │        → bump_revision → enqueue index → status='ready' → emit item_enriched
    └─ 失败 → 退避重试（2/5/15s ×3）→ 仍失败：status='pending'（OCR 摘录已占位）
             → 401/403：终止重试，错误标注「配置错误」
```

新鲜度预算（P95 ≤3s 的依据，架构 §5.4）：缩略图 ≤150ms + OCR 300–1500ms + 临时嵌入 <30ms + 同事务写 <50ms ≈ **0.5–2s**；VLM 全程在预算外异步。

阶段一/阶段二的**同事务原则**：`insights`、`items_fts`、向量三处写入在一个事务里，任一失败整体回滚、job 重试——单库事务免费获得「FTS 能搜到 ⇔ 向量能搜到」的一致性（A1 的决定性理由）。

### 6.2 jobs worker（`crates/pipeline/src/worker.rs`）

```sql
-- claim：单条 UPDATE ... RETURNING 原子取任务
UPDATE jobs SET state='running', attempts=attempts+1, updated_at=?3
WHERE id = (SELECT id FROM jobs
            WHERE state='queued' AND (next_run_at IS NULL OR next_run_at <= ?1)
            ORDER BY priority ASC, next_run_at ASC LIMIT 1)
RETURNING *;
```

- **2 worker** 线程；本地推理（ort）同步执行；HTTP（VLM）经注入的 `tokio::runtime::Handle` `block_on`。
- **stale 取消**：claim 后读 `items.revision`，≠ `job.revision` → `state='cancelled'`（旧 revision 任务天然作废，即「同一素材只保留最新任务」）。
- **退避**：失败 `next_run_at = now + [2,5,15]s[attempts-1]`，attempts ≥3 → 终态 `failed`（vlm 阶段另有素材 `status='pending'` 语义，见 §6.1）。
- **优先级**：`priority` 0=新捕获链（ocr/index/vlm）、1=重试、2=rebuild；`idx_jobs_due(state,next_run_at,priority)` 已建。
- **崩溃恢复**：启动 `running→queued`（阶段 01 既有）+ `attempts` 不清零（防崩溃循环放大重试次数）。
- **水位事件**：每次入队/完成/失败后发 `pipeline_status { queued, running }`（合并 100ms 防抖，避免事件风暴）。
- worker 循环空闲时 500ms 轮询（无外部队列可通知；20 条规模足够，规模上来换 notify + 短轮询组合）。

### 6.3 检索链路（`crates/app-core/src/search_flow.rs` → `search::execute`）

```text
search_items(raw)
  ├─ parse_query(raw)                          ≤5ms，纯规则
  ├─ embed_query(semantic_text)                 30–80ms（LRU 256；未就绪 → None，semantic_used=false）
  ├─ run_blocking:
  │    candidates = filters→SQL（无过滤=全集）
  │    fts_top   = library.fts_search(MATCH 表达式, candidates, 200)   20–60ms
  │    vec_top   = vector_index.search(query_vec, 200, candidates)     30–100ms
  │    fused     = rank::rrf([fts_top, vec_top], k, weights)           ≤15ms
  │    filters_only = candidates − (fts∪vec 命中)，附加在尾部（score=0）
  │    每条命中装配 Explanation（见下）
  │    INSERT queries(raw, parsed_json, result_ids) 
  └─ 返回 SearchResponseDto { took_ms, hits, parsed, semantic_used }
```

- **MATCH 构造**：`lexical_terms` 去 FTS5 语法字符（`" * ( ) :` 等）后以 `OR` 连接；`quoted_terms` 以 `AND` 连接（引号=必命中，见 §1.2）；词形来自同一 jieba。
- **RRF**：`score = Σ_ch w_ch/(k + rank_ch)`，k=60（Cormack 2009）；只在出现于某通道 Top-200 的素材上累加；平局 `captured_at DESC`，再平局 `rowid DESC`（稳定可回放）。
- **Explanation 装配**：`filters` 只保留该素材属性真正匹配的条件（如 color=orange 且查询含颜色 → 「橙色」）；`terms` = FTS 命中词与查询词的交集（用 `highlight()` 或分词交集）；`semantic=true` 当且仅当无 FTS/过滤命中但向量命中；`highlight_boxes` = `terms` 与 `ocr_boxes_json` 逐框文本的包含匹配（失败退化为空，前端只做文字高亮）。
- **延迟预算**（架构 §6.5）：解析 5 + 嵌入 30–80 + FTS 20–60 + 向量 30–100 + 融合/解释 15 ≈ **100–260ms**，20 条规模远低于 800ms 门禁。

### 6.4 生命周期（`crates/pipeline/src/lifecycle.rs`）

1. **revision**：`Library::bump_revision(id)`（`UPDATE items SET revision=revision+1, updated_at=? WHERE id=?`）+ `enqueue(stage, revision)`；接入点：`note_set`、`tag_add/detach/rename`（rename 只 bump 受影响素材）、`vlm` 写入后、蓝绿重建除外。
2. **content_hash 复用**：`ocr`/`vlm` handler 开头查同 `content_hash` 且 `status` 非 pending 的既有 insight，命中则整段复用（拷贝字段、跳过 VLM 调用），只补缺失行；**不改变不去重**（items 仍两条）。
3. **蓝绿重建**（`rebuild` handler + `embedding_rebuild` 命令）：
   ```text
   settings 读 embedding.model_id/dim 与目标模型不一致（或显式触发）
     → DROP TABLE IF EXISTS item_vectors_int8_v2; CREATE（同 0003 结构）
     → 分批（每批 100）扫 items：embed → 写 v2 表（与 FTS/insights 无关，不动）
     → COUNT 比对 items 与 v2 → 一致 → 单事务 { settings['vector.active_table']='…_v2' }
     → 旧表保留（回滚=把指针改回）；检索全程读指针，零中断
   ```
4. **启动自检**（`db.rs` 开库后）：`COUNT(items)` vs `COUNT(items_fts)` vs `COUNT(活跃向量表)`；不一致记 `warn` 并对缺失行入队 `index`（revision 用当前值）；`fts.tokenizer_version` 不一致只 `warn`（§1.2）。
5. **解析失败不自动重试**（2026-11-09 变更）：启动时**不再**把 `status='pending'` 的素材重新入队 `vlm`；运行期也不做自动轮询。恢复解析只能由用户在详情面板点「重新解析」（`item_reparse`）触发。

### 6.5 Provider 实现（`crates/ai`）

**PP-OCR（`ocr/ppocr.rs`）**：det（DBNet，输入 960×N，动态形状）→ rec（CRNN，按 det 框逐个 crop，宽动态、高 48）；模型从 `models/` 加载并校验 sha256（`models.rs`，resource dir → 仓库目录顺序解析）；输出聚合为 `ocr_text`（按阅读序拼接）+ `ocr_boxes`（归一化到原图宽高）+ 逐框置信度均值。超长图（长边 >4096）等比降采样到 4096 再识别（丢角标级小字可接受，登记）。

**Windows.Media.Ocr（`ocr/windows_ocr.rs`）**：`BitmapDecoder` → `SoftwareBitmap` → `OcrEngine`（`zh-CN` 优先，`TryCreateFromUserProfileLanguages` 兜底）；语言包缺失即返回错误（矩阵该行落到「OCR 无文字」路径）。WinRT 异步在专用线程 block。

**bge 嵌入（`embedder.rs`）**：`tokenizers` 加载 tokenizer.json；输入按 `标题>摘要>…` 拼接后 512 token 截断；`[CLS]` 池化 + L2 归一化；ort int8 ONNX session 懒加载（`OnceLock` + 内部 `Mutex`），`ready()` 供降级判定；查询缓存 `Mutex<LruCache<String, Vec<f32>>>`（容量 256）。

**OpenAI 兼容 VLM（`vlm/mod.rs`）**：`reqwest` async POST `{base_url}/chat/completions`；图片走 `image_url` data URL（`base64`）；超时 30s + 取消令牌；主备责任链：请求失败（连接/超时/5xx）→ 标记当前槽位不健康 → 备用重试一次 → 都失败返回错误；401/403 单独错误码 `ProviderConfig`（不重试）。探测（`model_probe`）复用同一客户端：文本 ping（`max_tokens: 1`）与 1×1 PNG 视觉请求。

### 6.6 查询解析规则（`crates/search/src/parser.rs`）

| 类别 | 规则 | 归一 |
|---|---|---|
| 时间 | 今天/昨天/前天/本周/上周/本月/上个月/今年/最近 N 天 | `TimeRange`（本地时区，周一起始） |
| 颜色 | 红/橙/黄/绿/蓝/紫/粉/棕/褐/黑/白/灰/深色/浅色/多彩 | 枚举集（深色→[black,gray] 等） |
| 图表 | 柱状图/折线图/饼图/漏斗图/表格 | bar/line/pie/funnel/table |
| 类型 | 截图/图片、文本/文字、链接 | image/text/link |
| 公式 | 含「公式」「latex」 | has_formula=true |
| 应用 | `app:X`、`来自X`、`在X里/中`（X 为连续非空白词） | 小写；SQL 用 `LIKE %X%` COLLATE NOCASE |
| 标签 | `tag:X`、`#X` | 精确名 |
| 引号 | `"…"` 内文本 | jieba 分词后全部 AND |

规则表数据驱动（`&[(pattern, kind)]`），正例/反例进 proptest；未匹配文本原样进 `semantic_text`。

## 7. 线程、锁与顺序

新增并发单元：2 个 pipeline worker 线程、嵌入预热线程、VLM HTTP（tokio blocking 池）。锁纪律在阶段 02 四条之上追加：

1. **claim 事务必须短**：worker 取任务 → 立即提交 → 释放连接锁 → 再做长推理（OCR/嵌入/HTTP）→ 写结果时短事务。推理绝不持 `Library.write` 锁。
2. **模型所有权**：`Arc<dyn EmbeddingProvider>` 内部 `Mutex` 只护「加载状态与 LRU」；ort Session 本身 Send+Sync，推理不持锁（并发 embed 串行化在 session 层由 ort 内部处理；2 worker 规模无争用）。
3. **单一健康真相**：Provider 主备健康状态存 `AtomicU8`（healthy/degraded/config-error），`model_config_set` 与探测/失败路径只改这一处；`search_flow` 与 `vlm` handler 都读它，不各自缓存。
4. **事件不持锁**（沿用）：`pipeline_status` 有 100ms 合并窗口，且在锁外发送。
5. **启动顺序**：`Library::open`（自检）→ 迁移 → `pipeline_host` 启 worker（先恢复 running→queued）→ 启动自检修复 → 嵌入预热。检索在预热完成前可用（降级语义）。
6. **顺序不变量**：`bump_revision` 与 `enqueue` 必须同事务（先 bump 后入队，或同事务内完成），否则旧任务可能带着新 revision 逃逸。

## 8. 失败与降级矩阵

| 场景 | 后端行为 | 前端表现 |
|---|---|---|
| 无网络/无 Key | 捕获+OCR+临时嵌入照常；vlm 排队重试至 attempts=3 → `status=pending` | 摘要区「OCR 摘录」徽标；状态徽标 `待重试` |
| 嵌入模型未就绪 | 语义通道跳过，`semantic_used=false`，`semanticReady=false` | 状态栏「语义检索未就绪（FTS 模式）」；`semantic_status` 事件驱动显隐 |
| PP-OCR 模型缺失/校验失败 | `warn` + Windows OCR | 无感（`ocr_engine` 字段可查）；详情可看引擎名 |
| OCR 无文字（含空结果与两级引擎失败） | insights 建行：`ocr_text`/`ocr_confidence`/`boxes` 为 NULL/空，`summary` 占位不写（保持 NULL + `summary_source='none'`） | 标题回退窗口标题/时间；卡片「待解析」；不触发「置信度低」标注；index/vlm 照常 |
| VLM JSON 不合法 | 修复一次 → 仍失败按失败重试 | 无感（重试期间 OCR 摘录占位） |
| VLM 401/403 | `ProviderConfig` 错误，任务终止 | 设置 pane 红字 + Toast「密钥或地址配置错误」 |
| 主 Provider 网络失败 | 自动切备；都失败退避 | 队列状态可见；恢复后自动回主 |
| FTS/向量行丢失（外部改动） | 启动自检 `warn` + 入队修复 | 无感（修复期间该素材可能搜不到） |
| 库被锁（迁移中） | 既有 `BUSY` 闸门拦截捕获；worker 短事务重试 | 既有横幅 |
| 查询为空/全空白 | 不发起检索 | 回默认列表模式 |
| 无结果 | 空命中数组 | 空态引导文案 |

## 9. 测试策略

### 9.1 分层

| 层 | 范围 | 工具 |
|---|---|---|
| 纯函数单测 | 解析规则表、RRF、int8 量化/余弦、search document 拼接与截断、MATCH 转义、颜色同义词、OCR 摘录截断、VLM JSON 校验 | `#[test]` + proptest |
| library 集成 | 0003 迁移幂等、同事务三写回滚、FTS 写/删/搜、向量蓝绿指针切换、revision bump、自检计数、删除级联（含 FTS） | `tempfile` + 真实 SQLite |
| search 集成 | 三路召回端到端（注入假嵌入：同文本同向量、异文本异向量）、Explanation 装配、filters-only 尾部、RRF 排序稳定性 | tempdir + 假 `EmbeddingProvider` |
| pipeline 集成 | 两阶段时序、退避与 attempts、stale-revision 取消、content_hash 复用、suppressions 不回贴、崩溃恢复（杀线程模拟）、启动自检只修派生索引 | 假 Provider（记录调用次数与结果注入） |
| ai 单测/集成 | 嵌入确定性 + 语义 sanity + 缓存；OCR 用仓库内置 3–5 张 fixture 图断言关键文本出现；VLM 层用本地 mock HTTP（`wiremock` dev-dep）验证 schema/修复/主备/401 | `#[test]` / `#[ignore]` 标注需模型文件的用例（CI 无模型时跳过，本地必跑） |
| 前端组件 | SearchBar 防抖与过期丢弃、chips 删除重算、命中徽标、ExplainBar、摘要来源徽标、标签筛选、模型 pane（探测 mock） | Vitest + Testing Library |
| 手工验收 | OCR 真实准确率（20 张）、内存/延迟采样、凭据管理器、断网矩阵、GUI 目视 | `verification.md` |

### 9.2 可测性注入点

```rust
// pipeline 构造函数全部接受 trait 对象；测试注入假实现：
pub struct PipelineDeps {
    pub ocr: Arc<dyn OcrProvider>,
    pub embed: Arc<dyn EmbeddingProvider>,
    pub vision: Arc<dyn VisionProvider>,   // 测试用 FakeVision：可注入延迟/畸形 JSON/401
    pub text: Arc<dyn TextProvider>,
    pub runtime: tokio::runtime::Handle,
}
```

假嵌入的策略：`hash(text) → 确定向量`（同文本同向量、可控相似度），让语义通道在无模型环境下可精确断言排序。真实模型用例统一 `#[ignore]`（本地 `cargo test -- --ignored` 必跑，T0/验收取证）。

### 9.3 必须覆盖的边界

- 同事务回滚：构造向量表写失败（预置错误 dim 的唯一行/临时触发器），断言 insights 与 FTS 均未写入。
- **无字图片全链路**：OCR 空结果 → `ocr_text`/`ocr_confidence` 为 NULL、占位摘要不写（source 保持 `none`）→ 不触发「置信度低」标注 → index/vlm 照常完成 → 离线时仅标题/备注可搜；VLM 完成后颜色/图表过滤与语义摘要可命中。
- stale 任务：`bump_revision` 后旧 `index` 任务被 claim 即取消，不产生旧文档覆盖新数据。
- 20 条里 1 条无 FTS 行（手工删）→ 自检计数 +1，重启后修复且该素材重新可搜。
- 蓝绿切换中途检索：新表写入过程中查询始终读旧表（指针未切），无空窗。
- 引号查询：必命中词在 OCR 中但被 jieba 拆开时仍命中（AND 语义）；无 trigram 不保证语序（登记）。
- `quantize_i8` 往返：量化误差下余弦排序与全量 f32 余弦排序一致（随机 1000 对断言 Spearman 一致性 >0.99）。
- 颜色同义词：搜「深色」命中 `primary_color ∈ {black, gray}`。
- VLM 假实现返回 Markdown 包裹 JSON → 剥离成功；返回截断 JSON → 修复一次路径。
- `search_feedback` 对不存在 queryId 返回 `NOT_FOUND`。

## 10. 风险与门禁

### 10.1 启动门禁（T0 spike，Task 1；不通过不进入 Task 2+）

| 验证项 | 通过标准 | 不通过时 |
|---|---|---|
| PP-OCR ONNX 主路径 | 100–200 张真实截图字符准确率 ≥97%，典型截图 P95 ≤1.5s、4K ≤4s、内存增量达标 | 调预处理/换 v4/v5；仍不达标重新评审主备顺序 |
| bge-small-zh int8（ort） | 单条 <30ms、模型内存增量 ≤60MB、量化后语义排序不劣化 | 换量化方案/线程数；再不行评审 base-zh |
| jieba + FTS5 中文检索 | bundled SQLite FTS5 可用；≤20 条素材 10 条真实中文查询字面召回可用、snippet 正常 | 调词典/粒度；必要时重评审分词方案 |
| （可选）vec0 | 静态链接成功且 KNN 与暴力一致 | 直接搁置（主路径不受影响） |
| ort ARM64 交叉 | `cargo check --target aarch64-pc-windows-msvc` 通过 | 记录限制，阶段 04 处理 |

### 10.2 风险（编号接 v1.2 / 架构文档 §9）

| # | 风险 | 概率/影响 | 缓解 |
|---|---|---|---|
| R19 | sqlite-vec 早期版本行为变化 | 中/低 | 主路径不依赖；vec0 仅 spike（§1.2） |
| R20 | 20 条小库上语义增益有限，排序主要靠 FTS | 中/低 | 诚实预期；评测分桶验证通道贡献；权重可配置 |
| R21 | bge-small 语义质量不足 | 低/中 | 评测驱动升级 base-zh（蓝绿零中断） |
| R4' | 中文 FTS 召回质量 | 中/中 | jieba 词典评测 + 同义词层（回退顺序见 P3-R14） |
| R6' | VLM JSON 不稳定 | 中/中 | 严格 schema + 修复一次 + 重试 + 主备 |
| R22（新） | PP-OCR 模型文件来源/许可或尺寸超预期 | 低/中 | T0 确定具体模型与来源；备选 v4；manifest 锁 sha256 |
| R23（新） | Windows OCR 语言包缺失导致兜底不可用 | 中/低 | 兜底的兜底=无 OCR 路径（矩阵已定义）；详情显示引擎与置信度 |
| R24（新） | worker 长推理阻塞 2 并发下索引延迟 | 低/中 | OCR 与 VLM 分离阶段；退避不占 worker；T0 后按实测调 worker 数（≤4，记偏差） |

## 11. 门禁与阶段收获

- T0 spike 报告（准确率/延迟/内存实测数字）写入 `verification.md` §1，是后续所有性能口径的基线。
- 评测报告与调参记录入 `docs/eval/`（评测集数据本体不入库）。
- 结转阶段 04 的技术债全部登记在 `verification.md` §4：trigram、vec0、空闲卸载、ARM64、cls、tokenizer 自动重建、pending 自动轮询。
