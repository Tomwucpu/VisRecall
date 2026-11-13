# 阶段 03 迭代 R2：结构化词的词边界与软命中 · 实施计划

> 对应需求：F3.1–F3.4（查询解析 / chip 回显 / 命中解释）的解析质量修正，不新增功能范围
> 技术依据：`03-ai-search/design.md` §6.6（规则表驱动解析）、§6.3（三路召回 + RRF）；《VisRecall-阶段三-AI与检索架构设计》§6.1、§6.3
> 目标：消掉「结构化词被无条件消费成硬过滤」造成的三类假阴性——①词边界误判（`红楼` → `红` + `楼`）②语义被切割（`红色预警` → `预警` + 主色=red）③应用名误判（`存在文件里` → `app=文件`）。**时间条件维持现状，一行不改。**

**Goal:** 把颜色/图表/素材类型/公式四类词表从「无条件硬过滤」改为「token 边界识别 + 软命中（只加分不排除）」；`来自X` / `在X里` **不再解析**，按普通文本进语义通道（不剥离、不加分、不参与候选集）。时间、标签、`app:X`、引号仍走硬过滤。

**Architecture:** 改动集中在 `crates/search`（`tokenizer` / `parser` / `filters` / `execute` / `rank`）与 `crates/domain/src/search.rs`，`crates/pipeline`、`crates/library`、索引格式**不动**（不重建 FTS、不改 `TOKENIZER_VERSION`）。结构化召回由一条纯 SQL 的第三通道承担，不动 embedding 与向量表。

**Tech Stack:** 沿用现有栈，无新增依赖（`jieba-rs` 0.7 已在 `crates/search` 依赖内；`rusqlite` 复用 `Library::candidate_rowids`）。

## Global Constraints

- **时间不参与本次改动**：`TIME_WORDS`、`take_recent_days`、`take_time` 的匹配方式与硬过滤语义保持不变。
- **`app:X` 仍是硬过滤**：本次只移除 `来自X` / `在X里` 两种自然语言形式——**既不硬过滤，也不软命中**，原样留在语义文本里；显式前缀语法与 `#tag` / `tag:` 同级。
- **不重建索引**：`document.rs` 不追加主色/图表字段，`TOKENIZER_VERSION` 不变，已有库无需 rebuild。
- **查询路径零网络**；`search` 不依赖 `ai`（Provider 仍由 `app-core` 注入）。
- **软命中允许「一个词多重身份」**：软词不剥离，仍留在 `semantic_text`；这是对本模块原纪律「一个词只消费一次」的**显式豁免**，只适用于软命中层，硬过滤层仍必须剥离。
- **不新增存储引擎、不新增后台任务**；facet 通道是纯查询侧 SQL。
- 素材测试规模 ≤20 条；结论不外推为大规模性能结论。
- 依赖锁 `Cargo.lock` / `pnpm-lock.yaml`；`tauri-specta` 绑定再生成后必须提交。

---

## 1. 现状与证据

### 1.1 三个事实

| # | 事实 | 位置 |
|---|---|---|
| F1 | 结构化词用**子串 `find`** 从原文剥离，无词边界 | `crates/search/src/parser.rs:177`（`strip_once`）、`:191`（`strip_first_of`）、`:420`（`take_color`） |
| F2 | 被剥离后的剩余文本才是语义文本，软词从 `semantic_text` / `lexical_terms` / 查询嵌入中**彻底消失** | `parser.rs:154`、`execute.rs:233`（语义文本为空则跳过向量通道） |
| F3 | 颜色/图表/类型/公式条件，以及 `来自X` / `在X里` 解析出的应用名，都被翻译成 **WHERE 硬条件**，并作为唯一候选集限制 FTS 与向量 | `crates/search/src/filters.rs:95`、`:112`、`:117`、`:90`、`:82`；`parser.rs:270`（`take_app`）；`execute.rs:97`（`candidate_rowids`）→ `:105`、`:116` 都带 `candidates` |
| F4 | 主色、图表类型、素材类型**不在 FTS 索引里** | `crates/search/src/document.rs:30`（只有 6 个文本字段）、`crates/pipeline/src/handlers/index.rs:60` |

### 1.2 三类假阴性

| 用例 | 当前链路 | 结果 |
|---|---|---|
| `红楼` | `find("红")` 命中 → `color=[red]`、`semantic_text="楼"` → 候选集 = 主色 red ∩ FTS"楼" | 目标素材主色非红 → **被候选集排除，查不到**；只出现"其它红色素材" |
| `红色预警` | `find("红色")` 命中 → `semantic_text="预警"`，FTS/向量都不再看"红色" | 正文写"红色预警"但主色为蓝的素材 → **被排除** |
| `来自Chrome的报错` | `find("来自")` 命中 → `app="chrome"`、`semantic_text="的报错"` → 候选集 = `source_app LIKE %chrome%` | 非 Chrome 的同类报错 → **被排除**；「chrome」也从语义通道消失 |
| `存在文件里` | `find("在")` 命中且后接方位词 → `app="文件"`、`semantic_text` 只剩"存" → 候选集 = `source_app LIKE %文件%` | 实际想搜的是"存在"相关素材 → **被排除**（`在` / `来自` 是高频汉字组合，此类误判无法靠词表规避） |

实测（`crates/search` 的 `tokenizer::words`，jieba 与索引侧同一分词器）：

| 输入 | 切分 | 结论 |
|---|---|---|
| `红楼` | `["红楼"]` | 切不出 `红` |
| `红旗` | `["红旗"]` | 切不出 `红` |
| `红色预警` | `["红色","预警"]` | `红色` 是独立 token |
| `酒红色` | `["酒","红色"]` | 子词以「色」结尾 |
| `深色主题` | `["深色","主题"]` | 独立 token |
| `红` | `["红"]` | 孤词 |

### 1.3 为什么不能只调权重

软加权只能让被排除的素材「有机会」回来；`红楼` 的语义文本已经被切成 `楼`，FTS 召回的是楼市/楼层/楼梯。词边界是必要条件，权重调整不是替代品。

---

## 2. 设计决策

| ID | 事项 | 结论 | 理由与代价 |
|---|---|---|---|
| D3-Q01 | 结构化词的判定方式 | 由「子串 find」改为「jieba token 边界匹配」，复用查询侧同一分词器 | 实测 `红楼`/`红旗` 为单 token；索引侧与查询侧本已同源，无新依赖 |
| D3-Q02 | 单字颜色词的启用条件 | 仅三种情况生效：孤词、后邻为标点/空白/句末、token 以「色」结尾（`酒红色`）；否则不消费 | 中文单字颜色词歧义极高（红楼/红旗/红烧肉/红利/红线）；误判代价是彻底丢素材，漏判代价只是少一个过滤，两者不对称，必须偏保守 |
| D3-Q03 | 软词是否从语义文本剥离 | **不剥离**，仍参与 FTS 与查询嵌入 | 用户诉求「同时走语义搜索」；孤词场景已被 D3-Q05 兜住，噪声可控 |
| D3-Q04 | 软条件的召回方式 | 新增第三路召回通道（纯 SQL：命中数 → 时间排序），并入 RRF，权重 0.6 | 主色/图表不在 FTS 索引（F4），塞进索引需全库重建且污染 BM25；RRF 只用名次（`rank.rs:65`），通道只需有序 rowid，不需要新增 store 方法 |
| D3-Q05 | 孤词降级 | 查询中除软词外没有其它语义/条件时，软条件**退回硬过滤** | 保留「输入 `红色` = 列出红色素材」的直觉；否则纯颜色查询返回全库 |
| D3-Q06 | 时间 | 保持子串匹配 + 硬过滤 | 本期范围外（见 §7 R1） |
| D3-Q07 | 空结果放宽 | 孤词硬过滤为 0 命中时，清空软条件重跑一次，结果标注「条件已放宽」 | 与 D3-Q05 配对，避免孤词路径重新引入假阴性；成本只在 0 命中时发生 |
| D3-Q08 | 应用名的自然语言形式（`来自X` / `在X里`） | **移除解析**：不硬过滤、不软命中、不剥离 | `来自`/`在` 是高频汉字组合（存在/现在/正在/来自哪里），误判率高且无法靠词表规避；这类短语本来就该由语义与字面通道处理，多一路加分只会制造排序噪声 |
| D3-Q09 | 应用名的显式语法 `app:X` | 保留硬过滤 | 显式命令无歧义，与 `#tag` / `tag:` 同级；用户要「只搜某应用」时仍有精确通道 |

---

## 3. 判定分层

| 层 | 词类 | 是否进候选集 WHERE | 是否进召回 | 备注 |
|---|---|---|---|---|
| 硬过滤 | 时间（不动）、`#tag` / `tag:`、`app:X` | 是 | — | 显式声明意图，必须剥离 |
| 软命中 | 颜色、图表、素材类型、公式 | 否（孤词例外） | facet 第三通道 + FTS 词 + 嵌入 | 不剥离 |
| 字面 | 引号内文本 | 否 | FTS（AND 语义） | 不变 |
| 不解析 | `来自X` / `在X里` | 否 | FTS + 向量（按普通文本） | 整句原样保留；`来自` 作停用词不进 FTS |
| 语义 | 其余原文 | 否 | FTS + 向量 | 不变 |

`ParsedQuery` 字段语义调整（`color` / `chart` / `content_kind` / `has_formula` **字段名不变**，避免大范围改前端与测试，只改文档注释与使用方式）：

- `color` / `chart` / `content_kind` / `has_formula`：由「过滤条件」变为「软命中条件」；
- `app`：**收窄为「仅 `app:X` 显式语法」**（仍硬过滤）；**不新增 `app_soft`**——`来自X` / `在X里` 不产生任何结构化字段；
- 新增 `facet_words: Vec<String>`：软命中的原始词，供孤词判定与 chips 区分；
- 新增 `has_hard_filters()`（`time | app | tags`）、`has_facets()`（`color | chart | content_kind | has_formula`）、`facets_only()`；`has_filters()` 语义收窄为 `has_hard_filters()`（调用方全部更新）；
- `Explanation` 新增 `facets: Vec<String>`；`filters` 只保留硬条件与界面筛选。

---

## 4. 行为矩阵（验收基线）

| 输入 | token | 软命中 | 语义文本 | 候选集 | 预期 |
|---|---|---|---|---|---|
| `红楼` | `["红楼"]` | 无 | `红楼` | 全集 | 纯语义检索，命中标题/OCR 含「红楼」的素材 |
| `红旗的截图` | `["红旗","的","截图"]` | 类型=图片 | `红旗的截图` | 全集 | 不按主色排除 |
| `红烧肉的配方` | `["红烧肉","的","配方"]` | 无 | 全量 | 全集 | 纯语义 |
| `红色预警` | `["红色","预警"]` | 颜色=red | `红色预警` | 全集 | 蓝色但正文含「红色预警」的素材可进 Top-N |
| `深色主题` | `["深色","主题"]` | 颜色=black,gray | `深色主题` | 全集 | 不排除其它素材，红图仍可能命中「主题」 |
| `红` | `["红"]` | 颜色=red | `红` | 主色=red | 孤词降级为硬过滤 |
| `红 楼` | `["红"," ","楼"]` | 颜色=red | `红 楼` | 全集 | 后邻空白 → 视为独立颜色词 |
| `柱状图` | `["柱状图"]` | 图表=bar | `柱状图` | 全集 | 孤词 → 硬过滤（等价旧行为） |
| `来自Chrome的报错` | `["来自","Chrome","的","报错"]` | 无 | `来自Chrome的报错` | 全集 | 应用名不解析；靠 FTS/语义命中 chrome 与报错（`来自` 是停用词） |
| `在微信里的聊天记录` | `["在","微信","里","的","聊天","记录"]` | 无 | `在微信里的聊天记录` | 全集 | 同上；微信素材靠字面/语义命中，不保证排序靠前 |
| `来自Chrome` | `["来自","Chrome"]` | 无 | `来自Chrome` | 全集 | 不产生结构化条件，也不是孤词降级 |
| `存在文件里` | `["存在","文件","里"]` | 无 | `存在文件里` | 全集 | 不再误判 `app=文件`，无 0 结果风险 |
| `app:chrome 报错` | 显式语法 | 应用=chrome（硬） | `报错` | `source_app LIKE %chrome%` | 精确通道保留，行为与现状一致 |
| `上个月 营收` | 时间词照旧 | — | `营收` | 时间区间 | **时间行为与现状完全一致** |

---

## 5. 任务

### Task 1: 分词边界（tokenizer + parser）

**Files:**
- Modify: `crates/search/src/tokenizer.rs`（新增 `segments()` + 单测；`STOPWORDS` 增 `来自`）
- Modify: `crates/search/src/parser.rs`（新增 `match_facet` / `token_matches` / `is_boundary_after`；改写 `take_color` / `take_chart` / `take_kind` / `take_formula`；**删除 `take_app` 的 `来自` / `在` 分支**并清理随之失效的 `is_locative_suffix` / `is_value_terminator`；收集 `facet_words`；更新模块头注释中「一个词只消费一次」的表述）
- Modify: `crates/domain/src/search.rs`（`ParsedQuery.facet_words` 字段 + `app` 的文档注释）
- Modify: `crates/search/src/parser.rs`（内联单测）、`crates/search/tests/parser.rs`

**Interfaces:**
- Consumes: `jieba-rs`（经 `tokenizer`，本 crate 是唯一入口）
- Produces:
  - `tokenizer::segments(text: &str) -> Vec<(String, usize, usize)>`（**字符**索引区间，非字节）
  - `parser::parse_query_at(raw, now_ms) -> ParsedQuery`（签名不变；新增 `facet_words`，`app` 仅由 `app:X` 产生）

- [ ] **Step 1: `segments()` 与字符→字节换算**

`segments` 不做 `is_word` 过滤（必须覆盖标点与空白，保证区间连续）。注意 jieba 的 `Token.start/end` 是**字符索引**，`&text[start..end]` 会返回 `None`/panic；换算按 `text.chars().take(n).map(char::len_utf8).sum()`，与现有 `take_quoted`（`parser.rs:214`）保持同一手法。单测断言 `segments("红楼") == [("红楼",0,2)]`、`segments("红色预警")` 前两项，以及「区间首尾相接覆盖全文」。

- [ ] **Step 2: `match_facet` 与单字守卫**

```rust
/// token 级软命中：返回命中值，不修改 remaining（软词保留在语义文本）。
fn match_facet<T: Clone>(text: &str, table: &[(&str, T)]) -> Option<T>;
```

判定顺序：①`token == entry` 且（`entry` 多字 或 孤词 或 `is_boundary_after`）→ 命中；②单字 `entry` 且 `token` 以该字开头、以「色」结尾且长度 >1 → 命中（`酒红色`）。孤词定义为「segments 中含字母数字的 token 恰好 1 个」。

- [ ] **Step 3: 四个 `take_*` 改写为不剥离**

`take_color` / `take_chart` / `take_kind` / `take_formula` 改为调用 `match_facet`，**不再调用 `strip_first_of`**；命中原词追加进 `facet_words`。`take_tags` / `take_time` / `take_quoted` 不动；`take_app` 拆到 Step 4。

- [ ] **Step 4: 移除 `take_app` 的 `来自` / `在` 分支**

`take_app` 只保留 `app:X` 分支（`parser.rs:272-284`），删除 `来自` / `在` 循环（`:287-322`）与其辅助函数 `is_locative_suffix`（`:326`）、`is_value_terminator`（`:334`）——两者只被该分支使用，不删会触发 `clippy -D warnings` 的 `dead_code`。`来自X` / `在X里` 由此变成普通文本：

- 不写 `app`、不写 `facet_words`、不进候选集；
- `STOPWORDS` 需补 `来自`（`在` / `里` / `中` 已在表内），否则 FTS 会多出一个高频噪声词；索引侧 `tokenize_for_fts` 不受 `STOPWORDS` 影响，无需重建索引。

- [ ] **Step 5: 单测覆盖 §4 矩阵的解析列**

`parser.rs` 内联单测中，`color_synonyms_map_to_enums` 需扩展（`红色预警` 的 `semantic_text` 断言由「无颜色词」变为「保留 `红色预警`」）；`a_word_is_consumed_only_once` 改名/改写为 `facet_words_do_not_erase_semantics`；`app_patterns_are_recognized` 拆成 `app_prefix_is_hard`（`app:chrome 报错` → `app=chrome`、`semantic_text="报错"`）与 `app_phrases_are_not_parsed`（`来自Chrome的报错` → `app.is_none()`、`facet_words` 为空、`semantic_text` 完整、`has_filters()` 为 false）。

**Verify:**

```bash
cargo test -p visrecall-search parser
cargo test -p visrecall-domain search
cargo clippy -p visrecall-search --all-targets -- -D warnings
```

**Commit:** `feat(search): match structured words on token boundaries`

---

### Task 2: 硬/软拆分（domain + filters）

**Files:**
- Modify: `crates/domain/src/search.rs`（`has_hard_filters` / `has_facets` / `facets_only` / `hard_chips` / `soft_chips` / `Explanation.facets`）
- Modify: `crates/search/src/filters.rs`（删除 4 段软条件 WHERE；新增 `facet_rowids`；`passes_with_filters` 只判硬条件；新增 `matched_facets`）
- Modify: `crates/search/src/lib.rs`（导出 `facet_rowids`、`FacetMode` 如采用）
- Modify: `crates/search/src/filters.rs` 内联单测（`color_filter_uses_in_list` / `all_filters_compose_with_matching_placeholder_count` 需重写）

**Interfaces:**
- Consumes: `Library::candidate_rowids(&str, &[Value]) -> Vec<i64>`（`crates/library/src/db.rs:219`）
- Produces:
  - `ParsedQuery::has_hard_filters() -> bool`（`time | app | tags`）
  - `ParsedQuery::has_facets() -> bool`（`color | chart | content_kind | has_formula`）
  - `ParsedQuery::facets_only() -> bool`
  - `filters::facet_rowids(library, &ParsedQuery, limit) -> Result<Vec<i64>, AppError>`（**有序** rowid；RRF 只用名次，无需分数）
  - `filters::to_sql_with_filters(query, filters, FacetMode) -> Result<SqlFilter, AppError>`，`FacetMode::{Soft, Hard}`

- [ ] **Step 1: `domain` 判定方法**

`has_filters()` 收窄为 `has_hard_filters()`；`Explanation` 增 `facets`。`facets_only()` 用 `facet_words` 覆盖判断：`lexical_terms` 非空且每个词都在 `facet_words` 中，且 `quoted_terms` 为空、无硬条件。

- [ ] **Step 2: `filters` 删除软条件 WHERE**

删掉 `color`（现 95–110）、`content_kind`（90–93）、`chart`（112–115）、`has_formula`（117–120）四段。`app` 分支（`filters.rs:82`）**保持原样**——现在只有 `app:X` 会写 `parsed.app`。`FacetMode::Hard` 时把四段按旧逻辑拼回（供孤词路径复用，避免两份 SQL 漂移）。

- [ ] **Step 3: `facet_rowids`**

```sql
SELECT rowid FROM (
  SELECT i.rowid AS rowid, i.captured_at,
         (CASE WHEN ins.primary_color IN (… ) THEN 1 ELSE 0 END
        + CASE WHEN ins.chart_type = ? COLLATE NOCASE THEN 1 ELSE 0 END
        + CASE WHEN i.kind = ? THEN 1 ELSE 0 END
        + CASE WHEN ins.has_formula = 1 THEN 1 ELSE 0 END) AS hits
  FROM items i LEFT JOIN insights ins ON ins.item_id = i.id
) WHERE hits > 0 ORDER BY hits DESC, captured_at DESC, rowid DESC LIMIT ?
```

占位符编号必须与 `params.len()` 对齐（沿用 `SqlFilter::placeholder_count` 自检断言）。多软条件语义为 OR，用 `hits DESC` 实现「全部命中优先」。

- [ ] **Step 4: `passes_with_filters` / `matched_labels` 只判硬条件**

软命中改为 `matched_facets(item, insight, query) -> Vec<String>`（`主色 红` / `图表 漏斗图` / `类型 图片` / `公式`），供 `Explanation.facets`；`matched_labels` 的 app 分支只处理 `app:X`。

**Verify:**

```bash
cargo test -p visrecall-search filters
cargo test -p visrecall-domain search
cargo clippy -p visrecall-search --all-targets -- -D warnings
```

**Commit:** `refactor(search): split hard filters from soft facets`

---

### Task 3: 三路召回与孤词降级（execute + rank）

**Files:**
- Modify: `crates/search/src/execute.rs`（`FacetMode` 选择、第三通道、孤词降级、空结果放宽、`Explanation` 装配）
- Modify: `crates/search/src/rank.rs`（`weights: Vec<f32>`、`DEFAULT_WEIGHTS`、`from_settings` 长度补齐）
- Modify: `crates/search/tests/execute_search.rs`

**Interfaces:**
- Consumes: `filters::facet_rowids`、`ParsedQuery::facets_only`、`rank::rrf`
- Produces: `SearchOutcome` 结构不变；`Explanation` 增 `facets`、`parsed` 增 `facet_words`（随 IPC 自动生成）

- [ ] **Step 1: 通道编排**

```rust
let facet_mode = if parsed.facets_only() { FacetMode::Hard } else { FacetMode::Soft };
let candidates = candidate_rowids(deps.library, &parsed, &request.filters, facet_mode)?;
let facet_channel: Vec<(i64, f32)> = match facet_mode {
    FacetMode::Soft => filters::facet_rowids(deps.library, &parsed, CHANNEL_LIMIT)?
        .into_iter().map(|rowid| (rowid, 0.0)).collect(),
    FacetMode::Hard => Vec::new(),  // 已进 WHERE，不再重复计分
};
let fused = rank::rrf(&[&fts_channel[..], &vec_channel[..], &facet_channel[..]], deps.rrf, &tie_break);
```

- [ ] **Step 2: 孤词降级与放宽**

`FacetMode::Hard` 且 `hits.is_empty()` 时清空全部软条件（`color` / `chart` / `content_kind` / `has_formula`）、`facet_mode = Soft`、重跑一次；结果 `Explanation.facets` 追加「条件已放宽」。二次检索必须重算候选集（不能复用旧 `candidates`），并保留界面筛选与硬 `app`（`app:X` 不放宽）。

- [ ] **Step 3: `rank` 权重扩容**

`DEFAULT_WEIGHTS: &[f32] = &[1.0, 1.0, 0.6]`；`RrfConfig.weights: Vec<f32>`（旧 `{"k":60,"weights":[1.0,1.0]}` 可反序列化）；`from_settings` 长度不足时补 0.6 而非 `unwrap_or(1.0)`。

- [ ] **Step 4: 集成测试**

沿用 `execute_search.rs` 的 `MapEmbed`：库内造 A（主色 blue + OCR「红色预警」）、B（主色 red + OCR「季度报表」）、C（主色 blue + 无关文本）、D（标题「红楼」）、E（`source_app = chrome` + OCR「报错日志」）、F（`source_app = vscode` + OCR「报错日志」）。断言见 §6 的 G5–G7、G9–G11。

**Verify:**

```bash
cargo test -p visrecall-search
cargo test -p visrecall-eval
cargo clippy --workspace --all-targets -- -D warnings
```

**Commit:** `feat(search): recall facets through a third RRF channel`

---

### Task 4: 前端可解释性（chips / 解释条 / 绑定）

**Files:**
- Modify: `src/features/search/QueryChips.tsx`（软 chip 虚线样式 + 「仅文字」按钮）
- Modify: `src/features/search/ExplainBar.tsx`（软命中行）
- Modify: `src/app/App.tsx`（新增 `onToLiteral`：把该词改写成 `"词"` 引号形式重搜，见 `App.tsx:459`）
- Modify: `src/lib/searchTypes.ts`（`literalizeChip(raw, chip)` 工具，与 `stripChip` 同文件）
- Modify: `src/styles/search.css`
- Regenerate: `src/lib/bindings.ts`（`facetWords`、`explanation.facets`）
- Modify: `src/features/search/ExplainBar.test.tsx`、`src/features/search/useSearch.test.tsx`

**Interfaces:**
- Produces: `literalizeChip(raw: string, chip: string): string`（把软词包成 `"…"`，走既有引号字面通道，后端零改动）

- [ ] **Step 1: 软 chip 与两种操作**

软 chip（`红(参考)` / `漏斗图(参考)`）保留「×（从原文删掉）」+ 新增「仅文字（改写为引号）」。理由：`×` 会把词从语义文本一起删掉，用户若只想「不按颜色筛但搜这两个字」就会查不到。`app:` 前缀产生的 chip 仍按硬条件渲染（不加「仅文字」入口）；`来自X` / `在X里` 不再产生 chip。

- [ ] **Step 2: 解释条区分三类命中**

`筛选条件`（硬）/ `软命中`（facets）/ `命中词`（terms）；`semantic` 徽标定义收窄为「非 FTS + 无硬条件 + 无软命中」。

- [ ] **Step 3: 重新生成绑定并跑前端门禁**

**Verify:**

```bash
pnpm typecheck
pnpm lint
pnpm test
pnpm build
```

**Commit:** `feat(search-ui): distinguish soft facet chips and add literal search toggle`

---

### Task 5: 评测与验收回填

**Files:**
- Modify: `docs/eval/queries.template.jsonl`（追加 5 条：`红楼`、`红色预警`、`深色主题`、`柱状图`、`来自Chrome的报错`）
- Create: `docs/specs/v0.1/03-ai-search/verification.md` §6「迭代 R2 验收」
- Modify: `docs/specs/v0.1/README.md`（阶段 03 状态行补「R2」一句）

**Interfaces:**
- Consumes: `cargo run -p visrecall-eval -- --db <库> --queries <jsonl> --report <md>`
- Produces: R2 门禁表（§6）与 MRR / Top-3 前后对比

- [ ] **Step 1: 评测集加 5 条**（每条含 `query` 与 `expect` 素材 id 列表，格式照模板注释）
- [ ] **Step 2: 跑基线**（改动前代码上先跑一次，记录 Top-3/MRR）
- [ ] **Step 3: 回填 §6**：G1–G12 逐项勾选并附命令输出摘要，不通过项登记收口方案

**Verify:**

```bash
cargo run -p visrecall-eval -- --db <库根> --queries docs/eval/queries.template.jsonl --report docs/eval/report-r2.md
```

**Commit:** `test(eval): add facet parsing cases and record r2 gate results`

---

## 6. 验收门禁

| ID | 门禁 | 判定 |
|---|---|---|
| G1 | `parse("红楼").color.is_none()` 且 `semantic_text == "红楼"`、`lexical_terms` 含 `红楼` | Task 1 |
| G2 | `parse("红色预警").color == Some(["red"])` 且 `semantic_text == "红色预警"` | Task 1 |
| G3 | `to_sql(parse("红色预警")).is_empty()`（软条件不进 WHERE） | Task 2 |
| G4 | `!to_sql(parse("红色")).is_empty()`（孤词仍硬过滤） | Task 2 |
| G5 | 集成：搜 `红色预警` 时，主色 blue 但 OCR 含「红色预警」的素材进 Top-3 | Task 3 |
| G6 | 集成：搜 `红色` 只返回主色 red 的素材（孤词降级） | Task 3 |
| G7 | `embed: None`（语义未就绪）时 G5/G6 仍成立（P3-R09 不回归） | Task 3 |
| G8 | 既有用例全绿（Rust + 前端）且 eval Top-3 不劣于基线（阶段 03 门禁 ≥80%） | Task 5 |
| G9 | `parse("来自Chrome的报错")`：`app.is_none()`、`facet_words` 为空、`semantic_text == "来自Chrome的报错"`、`has_filters()` 为 false | Task 1 |
| G10 | `to_sql` 对 `来自Chrome的报错` / `在微信里的聊天记录` / `存在文件里` 三者均为空（不产生任何 WHERE） | Task 2 |
| G11 | 集成：搜 `来自Chrome的报错` 时 E（chrome）与 F（vscode）都被召回，不因 `source_app` 被排除 | Task 3 |
| G12 | 集成：`app:chrome 报错` 只返回 E（`app:X` 硬过滤不回归） | Task 3 |

---

## 7. 已知不做与遗留风险

| ID | 项 | 影响 | 处理 |
|---|---|---|---|
| R1 | 时间词仍是子串匹配 + 硬过滤 | 「关于上个月的费用」会被当时间条件，误判后 0 结果（与 `红楼` 同源） | 本期按要求不动；建议下一迭代对时间词做 token 边界 + 0 命中放宽 |
| R2 | ~~`take_app` 的 `find("在")` / `find("来自")` 是子串匹配~~ | 已由 D3-Q08 处理：`来自` / `在` 分支整体删除，误判路径不存在 | 关闭 |
| R3 | FTS 普通词是 OR 连接（`fts_query.rs:36`） | 软词进入 `lexical_terms` 后，`红色预警` 会召回只含「红色」的素材，精度下降 | 接受（软命中即召回优先）；靠 eval 观察，必要时给软词的 FTS 通道单独降权 |
| R4 | 候选集为空（`None`）时向量通道是**全库 top-200** | 泛词（「图」「表格」）语义噪声大；`SEMANTIC_RELATIVE_FLOOR=0.5` 是相对阈值，短查询下不稳 | 保持现值 + 用新增 5 条用例校准；若 Top-3 下降，对 `lexical_terms.len() <= 1` 的查询加绝对下限 |
| R5 | `only_filters` 尾部补位（`execute.rs:144`） | 只剩时间条件时候选集可能数百条，大量 0 分素材排在尾部，观感像「软条件没生效」 | 给尾部补位加 `limit` 上限；或本期先观察 |
| R6 | 软 chip 的「×」会连语义一起删 | 用户想「不筛颜色但搜这两个字」时删了就查不到 | 由 Task 4 的「仅文字」按钮覆盖 |
| R7 | `settings['search.rrf']` 存量是 2 元素数组 | facet 权重落到默认补齐值（0.6），不是 1.0 | `from_settings` 显式补齐，不依赖 `unwrap_or` |
| R8 | facet SQL 全表 `LEFT JOIN insights` | 万级以上才有感 | 可选 `CREATE INDEX idx_insights_primary_color`（不进迁移，规模需要时再加） |
| R9 | `Explanation.semantic` 语义收窄 | 结果卡片的「仅语义命中」徽标定义变化 | Task 4 Step 2 统一口径，不新增徽标 |
| R10 | `facet_words` 新增字段 | 旧 `queries.parsed_json` 反序列化 | `#[serde(default)]`，旧记录读出为空数组 |
| R11 | 自然语言应用条件能力归零 | `来自微信的截图` 不再按应用筛；若素材正文不含「微信」，该素材只能靠语义命中，且不保证靠前 | 接受：用户需要精确筛选时改用 `app:微信`；建议在空结果提示里给出这条引导（Task 4 可加一行文案） |

---

## 8. 回滚

- Task 1 可独立回滚（只影响词边界与 `take_app`，不影响召回结构）：`git revert` 该提交后，`红楼` 与 `来自X` 回到旧行为。
- Task 2/3 需成对回滚：`filters` 恢复软条件 WHERE、`execute` 去掉第三通道、`rank.weights` 回 `[f32; 2]`。
- 若只想恢复「应用名硬过滤」而保留颜色/图表改造：恢复 `take_app` 的 `来自` / `在` 分支与 `is_locative_suffix` / `is_value_terminator` 即可，与 facet 通道无关。
- `facet_words` 与 `Explanation.facets` 为附加字段（`#[serde(default)]`），保留不删除也不影响旧逻辑。
- 索引与库结构无变化，**不需要数据迁移或重建**；回滚后无需重新索引即可恢复正常检索。
