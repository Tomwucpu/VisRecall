# 阶段 02：存储与详情 · 设计

> 对应需求：`requirements.md` P2-R01–P2-R14
> 技术依据：《VisRecall-技术架构设计 v1.2》§4.5、§5.1–5.6、§8.1–8.3、§8.6；UI 前置阶段产物 `docs/specs/v0.1/ui-contract-draft.md`
> 目标：把 UI 前置阶段留出的 stub 全部换成真实命令，用最小改动补齐存储与生命周期能力；不引入 AI、检索与网络。

## 1. 设计结论

### 1.1 阶段决策（已定稿 2026-11-06）

| ID | 事项 | 结论 | 理由与代价 |
|---|---|---|---|
| D2-01 | 实时模式 | **不实现**。抓帧重试仍失败 → `CAPTURE_FAILED`，列已知限制，阶段 04 复评 | 透明覆盖层无冻结帧会改变选区语义（画面在动），收益低于成本；用户已有「重试捕获」路径 |
| D2-02 | 打包交付 | **只修 release 资源加载**（启用 `tauri/custom-protocol`），不产出安装包 | 安装包、签名、自动更新集中在阶段 04 一次性处理，避免两次返工；release 二进制必须可手工运行 |
| D2-03 | 位置迁移 | **复制 + 校验 + 写 config + 要求重启**；失败不删源、不改 config；不做显式续传文件 | 热切换库句柄需要把 `AppCore.library` 改成可替换并处理在途任务，风险远高于收益；重启路径对用户是可理解的 |
| D2-04 | 保留策略 | **启动清理 + 手动立即清理**，含预估；不做 3 天预告 | 定时器 + 预告状态持久化约 2 人日，V0.1 测试规模（≤20 条）下用户手动清理已足够；偏差记录在 §1.2 |
| D2-05 | 策略存储位置 | 容量与保留天数存 **库内 `settings` 表**（键 `storage.capacity_gb` / `storage.retention_days`） | 策略属于「这个库」而非「这台机器」；`config.json` 跟安装走，换库后会张冠李戴；`settings` 表 0001 已建但未用 |
| D2-06 | 标签来源 | `item_tags.origin` 记「这条关联是自动还是手动加的」；`tags.source` 记标签首次创建来源 | 同一标签可能被自动贴到 A、被手动贴到 B，chip 的「自动」视觉必须按关联判定，否则语义错 |
| D2-07 | 首次引导 | 用主窗口一次性 Toast + 设置入口提示，**不做独立气泡窗口** | 第二个分层窗口带来命中/遮挡/多屏定位三类新问题，收益仅为一次性提示 |
| D2-08 | 原图协议缩放 | 只提供 `visrecall://original/<id>` 原尺寸，**不实现 `?max=` 参数** | 详情预览在 WebView2 内按容器缩放即可；4K 原图 ~5MB 在 ≤20 条规模下无内存压力。阶段 04 若测出问题再加白名单参数 |
| D2-09 | 全库清空确认 | 需输入确认词；确认词与待删条数由后端 `library_purge_preview()` 提供 | 不可恢复操作不能只靠一次点击；文案留在后端可避免前后端判断漂移 |
| D2-10 | 标签配色 | 后端按规范化名称做 FNV-1a 散列，映射到现有 7 个 `--hue-*` 令牌之一 | 令牌已在 `tokens.css` 定义（ochre/slate/jade/ash/indigo/plum/paper）；后端定型保证迁移后颜色一致，前端不重复实现散列 |

### 1.2 与架构文档的已知偏差

| 偏差 | 架构原文 | 本阶段做法 | 收口 |
|---|---|---|---|
| 清理预告 | §5.6「清理前 3 天预告」 | 不做预告，改为保存前预估 + 手动立即清理 | 阶段 04 复评 |
| 删除文件失败 | §5.4「进入 `gc` 重试队列」 | 不建 gc 表；失败记 warn，由孤儿扫描计数 + 用户手动清理入口兜底 | 阶段 03 引入任务队列时一并评估 |
| 原图协议参数 | §8.3 `?max=2048` 白名单 | 不实现，直接返回原尺寸 | 阶段 04 |
| 迁移续传 | §5.5 `migration.json` 断点续传 | 按「目标已存在且大小一致则跳过」的幂等复制天然续传 | 不排期（D2-03） |
| 标签来源 | §5.2 `tags.source` 单字段 | 增加 `item_tags.origin`，`tags.source` 保留为创建来源 | 阶段 03 自动标注按期使用 |
| 虚拟滚动 | P1-R07「超过 200 条引入虚拟滚动」 | 不实现：测试规模 ≤20 条 | 阶段 04 或素材超 200 时复评 |
| 测试规模 | §6.5「V0.1 测试规模 ≤100 条」 | **≤20 条**（2026-11-06 调整）；20 条结论不外推 | 需要 100 条口径时在阶段 04 用合成素材补测 |

## 2. 起点：既有资产

### 2.1 阶段 01 已提供（直接复用，不改语义）

| 能力 | 位置 |
|---|---|
| `get_app_status` / `list_items` / `get_item` / `open_library_folder` / `get_settings` / `update_settings` | `src-tauri/src/ipc.rs` |
| 缩略图协议 `visrecall://thumb/<id>`（路径校验、immutable 缓存、GET/HEAD） | `src-tauri/src/protocol.rs` |
| 原子写、可用空间查询、损坏备份 | `crates/library/src/files.rs` |
| 开库 → PRAGMA → 迁移 → 启动清理 | `crates/library/src/db.rs` |
| 缩略图生成（PNG 480px，有界并发 2 worker） | `crates/app-core/src/thumbnail.rs` |
| 事件总线（`EventSink` + `RecordingSink`） | `crates/app-core/src/events.rs` |
| 抓帧前最低空间门禁（<2GB → `STORAGE_FULL`） | `crates/app-core/src/capture_flow.rs:248` |
| 悬浮球窗口、拖动、位置记忆、拖放接收 | `crates/shell/src/ball.rs` |
| 热键线程（`HotkeySpec` 解析 + `RegisterHotKey`） | `crates/shell/src/hotkey.rs` |

### 2.2 UI 前置阶段已提供（本阶段只换实现，不改组件结构）

`src/lib/api.ts` 是唯一数据出口，stub 与真实命令同签名同返回类型。本阶段把它逐段替换：

| `api.ts` 方法 | 当前行为 | 本阶段替换为 |
|---|---|---|
| `loadDetail` | `get_item` + `stubDetailExtras` | `get_item_ext`（一次 IPC 返回 item + extras） |
| `saveNote` | 抛 `UNSUPPORTED` | `note_set` |
| `listTags` | 返回 `[]` | `tags_list` |
| `addTag` | 抛 `UNSUPPORTED` | `tag_add` |
| `copyOriginal` | 抛 `UNSUPPORTED` | `copy_item_original` |
| `deleteItem` | 抛 `UNSUPPORTED` | `item_delete` |
| `getStorageUsage` | 返回 `null` | `storage_usage_get` |
| `setCapacity` | 抛 `UNSUPPORTED` | `storage_capacity_set` |
| `setRetention` | 抛 `UNSUPPORTED` | `retention_set` |

替换完成后 `UNSUPPORTED_CODE`、`unsupported()`、`isUnsupported()` 一并删除（不再有 stub 使用者），`DetailPanel` 的 `unsupported` 保存态分支同步删除。

## 3. 模块与文件结构

```text
crates/domain/src/
  tag.rs                 # 新增：Tag / TagSource / 名称规范化 + 色令牌散列
  storage.rs             # 新增：StorageUsage / RetentionPolicy / RelocateReport
  error.rs               # 修改：FileMissing / ClipboardBusy / ClipboardFailed / Busy
  lib.rs                 # 修改：re-export

crates/library/
  migrations/0002_library_detail.sql   # 新增
  src/migrations.rs                    # 修改：追加版本 2；迁移前备份
  src/tags.rs                          # 新增：CRUD + suppressions
  src/items.rs                         # 修改：note_set/get、删除、过期查询、行数
  src/settings_kv.rs                   # 新增：settings 表键值读写
  src/storage.rs                       # 新增：用量、容量、保留、孤儿
  src/relocate.rs                      # 新增：库目录复制 + 校验
  src/db.rs                            # 修改：apply 新签名、连接访问收口
  src/lib.rs                           # 修改：模块导出

crates/shell/src/
  clipboard.rs           # 新增：CF_DIB + PNG + CF_UNICODETEXT
  ball/state.rs          # 新增：纯状态机（贴边/收起/全屏），可单测
  ball.rs                # 修改：改为 ball/mod.rs，接线三态与尺寸热更新
  lib.rs                 # 修改：导出

crates/app-core/src/
  detail.rs              # 新增：DetailExtras 聚合（原文截断、原图 URL、标签）
  storage_flow.rs        # 新增：用量/保留/孤儿/迁移编排
  app.rs                 # 修改：stop_runtime、update_settings 热生效、迁移闸门
  events.rs              # 修改：新增 8 个事件变体
  lib.rs                 # 修改：导出

src-tauri/src/
  ipc.rs                 # 修改：新增 20 个命令 + DTO
  protocol.rs            # 修改：/original/<id> 分支

src/
  lib/api.ts             # 修改：stub → 真实命令
  lib/bindings.ts        # 重新生成（不手写）
  lib/events.ts          # 修改：按事件类型分派失效范围
  lib/queryClient.ts     # 修改：新增 usage / orphans 键
  features/detail/DetailPanel.tsx        # 修改：接标签增删、删除后关闭、复制反馈
  features/shell/Sidebar.tsx             # 修改：标签云、存储条
  features/settings/SettingsDialog.tsx   # 修改：写控件启用 + 迁移/清理入口
```

不新增 crate。`crates/app-core` 已依赖 `shell` 与 `library`，剪贴板放在 `shell`（纯 Win32，无 UI 状态）而非 `app-core`，避免 `app-core` 引入 `windows` 依赖。

## 4. 数据模型

### 4.1 迁移 0002（`crates/library/migrations/0002_library_detail.sql`）

```sql
-- 标签：name 唯一性用 NOCASE 索引表达，避免 "Work"/"work" 分裂成两个标签。
CREATE TABLE tags (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  source TEXT NOT NULL CHECK (source IN ('auto','manual')),
  created_at INTEGER NOT NULL
);
CREATE UNIQUE INDEX idx_tags_name_nocase ON tags(name COLLATE NOCASE);

-- 关联：origin 记录「这次关联」的来源（自动标注 vs 用户手动）。
CREATE TABLE item_tags (
  item_id TEXT NOT NULL REFERENCES items(id) ON DELETE CASCADE,
  tag_id TEXT NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
  origin TEXT NOT NULL DEFAULT 'manual' CHECK (origin IN ('auto','manual')),
  created_at INTEGER NOT NULL,
  PRIMARY KEY (item_id, tag_id)
);
CREATE INDEX idx_item_tags_tag ON item_tags(tag_id);

-- 抑制：用户手动删掉的自动标签，重新解析时不得贴回（阶段 03 使用）。
CREATE TABLE tag_suppressions (
  item_id TEXT NOT NULL REFERENCES items(id) ON DELETE CASCADE,
  tag_name TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (item_id, tag_name)
);
```

要点：

1. `item_tags` / `tag_suppressions` 对 `items(id)` 建外键并 `ON DELETE CASCADE`；`items.id` 有 UNIQUE 约束，外键合法。开库时 `PRAGMA foreign_keys=ON`，单条删除的标签清理由 SQLite 完成，不需要手写级联。
2. `jobs.item_id` 在 0001 中没有外键，删除素材必须显式 `DELETE FROM jobs WHERE item_id = ?`，否则留下孤儿任务（阶段 03 会真的跑这些任务）。
3. `tags` 不因「无关联」自动消失：手动标签保留（可复用、可作为输入建议）；`source='auto'` 且无关联的标签在 detach 后清理。

### 4.2 迁移前备份

`migrations::apply` 增加前置步骤：检测到有待应用迁移时，先 `PRAGMA wal_checkpoint(TRUNCATE)`，再把 `visrecall.db` 复制到 `backups/visrecall.db.bak-<version>`，保留最近 2 份（按文件名版本号倒序删除多余）。复制失败只记 `warn` 并继续（迁移本身在事务里，失败会整条回滚），但要在日志里能看出来。

```rust
// crates/library/src/migrations.rs
pub fn apply(conn: &mut Connection, root: &Path) -> Result<(), AppError>;
```

`Library::open` 与两处迁移测试同步改签名。

### 4.3 `settings` 表键值

| key | 值（JSON） | 默认 | 校验 |
|---|---|---|---|
| `storage.capacity_gb` | `50` | 50 | 整数 1..=4096，越界 `INVALID_INPUT` |
| `storage.retention_days` | `null` / `90` | `null`（永久） | `null` 或整数 1..=3650 |

读取失败（值不是合法 JSON / 越界）按默认值处理并记 `warn`，不让一条坏配置卡住整个设置页。

### 4.4 类型

```rust
// crates/domain/src/tag.rs
pub enum TagSource { Auto, Manual }          // serde snake_case

pub struct Tag {
    pub id: String,
    pub name: String,
    pub source: TagSource,                    // 标签创建来源
    pub created_at: i64,
}

/// 标签 + 当前素材的关联来源；IPC 的 `TagChip` 由此映射。
pub struct ItemTag {
    pub tag: Tag,
    pub origin: TagSource,                    // 本次关联来源
    pub item_count: u64,                      // 标签云用；按素材查询时为 0
    pub hue: &'static str,                    // 令牌名，见 §4.5
}

pub fn normalize_tag_name(raw: &str) -> Result<String, AppError>;  // trim + 折叠空白 + 长度/控制字符校验
pub fn hue_for(name: &str) -> &'static str;                        // FNV-1a → 7 令牌
```

```rust
// crates/domain/src/storage.rs
pub struct StorageUsage {
    pub used_bytes: u64,
    pub capacity_bytes: u64,
    pub item_count: u64,
}

pub struct RetentionPolicy { pub days: Option<u32> }

pub struct RelocateReport {
    pub new_root: String,
    pub files_copied: u64,
    pub bytes_copied: u64,
}

pub struct OrphanStats {
    pub files: u64,          // 未被任何 items.original_path 引用的文件数
    pub bytes: u64,          // 上述文件的总字节数
    pub missing_thumbs: u64, // thumb_path 指向不存在文件的素材数（可重建，不删）
}
```

```rust
// crates/domain/src/error.rs 新增
#[error("file missing: {0}")]      FileMissing(String),   // FILE_MISSING
#[error("clipboard busy")]         ClipboardBusy,         // CLIPBOARD_BUSY
#[error("clipboard failed: {0}")]  ClipboardFailed(String),// CLIPBOARD_FAILED
#[error("busy: {0}")]              Busy(String),          // BUSY（迁移/清理进行中）
```

`src-tauri/src/ipc.rs` 的 `From<&AppError>` 同步加 4 个分支；`api_error_maps_stable_codes` 测试补 4 条断言。

### 4.5 标签色令牌

`tokens.css` 已定义 7 个色相令牌：`ochre / slate / jade / ash / indigo / plum / paper`。后端按规范化名称（小写）做 FNV-1a 32 位散列，取模 7；`ash` 作为中性档，散列落在它上面的标签视觉上与「无颜色」一致。前端只在 `TagChip.hue` 非空时设置 `--hue: var(--hue-<name>)`，不参与计算。

## 5. IPC 契约

### 5.1 命令（20 条新增，1 条扩展）

| 命令 | 入参 | 返回 | 说明 |
|---|---|---|---|
| `get_item_ext` | `id` | `ItemDetailDto` | 详情聚合读；`NOT_FOUND` / `IO` / `DATABASE` |
| `note_set` | `id, text` | `()` | 字符数 ≤10000；`INVALID_INPUT` / `NOT_FOUND` |
| `tags_list` | — | `TagChip[]` | 侧栏标签云（含 `itemCount`） |
| `tag_add` | `itemId, name` | `TagChip` | 同名复用；`INVALID_INPUT` |
| `tag_detach` | `itemId, tagId` | `()` | 解除关联；自动标签写抑制记录 |
| `tag_rename` | `tagId, name` | `()` | 改名后同名冲突 → `INVALID_INPUT` |
| `tag_delete` | `tagId` | `()` | 解除所有关联后删除 |
| `copy_item_original` | `itemId` | `()` | 按 kind 分支；`FILE_MISSING` / `CLIPBOARD_*` |
| `item_delete` | `id` | `()` | 级联 + 文件回收 |
| `library_purge_preview` | — | `PurgePreview` | `{ itemCount, confirmationWord }` |
| `library_purge` | `confirm` | `()` | 确认词不匹配 → `INVALID_INPUT` |
| `storage_usage_get` | — | `StorageUsage` | 目录遍历 + 计数 |
| `storage_capacity_set` | `gigabytes` | `()` | 1..=4096 |
| `retention_set` | `days: Option<u32>` | `()` | `null` = 永久 |
| `retention_preview` | `days` | `RetentionPreview` | `{ itemCount, bytes }` |
| `retention_apply` | — | `PruneReport` | `{ deleted, failed }`，走已保存的天数 |
| `storage_orphans_get` | — | `OrphanStats` | 设置 pane 展示 |
| `storage_orphans_cleanup` | — | `OrphanStats` | 返回清理结果（removed 放 files） |
| `storage_relocate` | `path` | `RelocateResult` | `{ restartRequired, newRoot }`；`INVALID_INPUT` / `BUSY` / `IO` |
| `pick_directory` | — | `Option<String>` | 目录选择器；仅在 Rust 侧调用 dialog 插件，不给前端开放文件系统权限 |
| `update_settings` | 扩展 patch | `SettingsDto` | 新增 `ballEdgeHide` / `hideInFullscreen`，且三项改动即时生效 |

`get_item_ext` 保留「扩展既有 `get_item` 读路径」的思路：内部复用 `Library::get_item`，再补 extras，不新建第二条读路径。

### 5.2 DTO

```rust
pub struct ItemDetailDto { pub item: ItemDto, pub extras: DetailExtrasDto }

pub struct DetailExtrasDto {
    pub summary: Option<String>,      // 阶段 02 恒为 None
    pub body_text: Option<String>,    // 仅 kind=text；前 20000 字符
    pub original_url: Option<String>, // 仅 kind=image：visrecall://original/<id>
    pub tags: Vec<TagChipDto>,
}

pub struct TagChipDto {
    pub id: String,
    pub name: String,
    pub auto_: bool,   // JSON 键 "auto"；= item_tags.origin == auto
    pub hue: Option<String>,
    pub item_count: u64,
}

pub struct PurgePreview { pub item_count: u64, pub confirmation_word: String }
pub struct RetentionPreview { pub item_count: u64, pub bytes: u64 }
pub struct PruneReport { pub deleted: u64, pub failed: u64 }
pub struct RelocateResult { pub restart_required: bool, pub new_root: String }
```

`auto_` 用 `#[serde(rename = "auto")]` 输出，保持与 `api.ts` 现有 `TagChip` 类型一致。

### 5.3 事件

| 事件 | 载荷 | 前端处理 |
|---|---|---|
| `item_note_updated` | `{ id }` | 失效 `detail(id)`，不动列表 |
| `item_tags_updated` | `{ id }` | 失效 `detail(id)` + `tags` |
| `tags_changed` | `{}` | 失效 `tags`（标签改名/删除/新增导致集合变化） |
| `item_deleted` | `{ id }` | 重置 `items`，关闭详情 |
| `items_reset` | `{}` | 重置 `items` + `tags` + 用量 |
| `items_pruned` | `{ count }` | 重置 `items` + 用量，`count>0` 时 Toast |
| `storage_migration_progress` | `{ phase, done, total }` | 迁移进度条 |
| `hotkey_restored` | `{}` | 无 UI 处理；`TauriEventSink` 收到后把托盘 tooltip 恢复为默认文案 |

`phase ∈ { prepare, copy, verify, done }` 为小写字符串。

### 5.4 与 `ui-contract-draft.md` 的差异（前端需同步的点）

| 草案 | 实现 | 原因 |
|---|---|---|
| `library_purge()` 无参 | `library_purge_preview()` + `library_purge(confirm)` | 不可恢复操作需服务端校验确认词（D2-09） |
| `storage_usage_get(): StorageUsage \| null` | Rust 返回 `StorageUsage`；前端类型保留 `\| null` | 读取失败走错误码，不把「读不到」伪装成数据；前端不必改类型 |
| `storage_relocate(path): void` | 返回 `{ restartRequired, newRoot }` | 需要明确告知用户「必须重启」（D2-03） |
| `tag_add` 只返回 chip | chip 增加 `itemCount` | 标签云需要数量，避免 N+1 查询 |
| 无孤儿清理命令 | `storage_orphans_get` / `storage_orphans_cleanup` | P2-R12 |
| `originalUrl` 语义未定 | 仅图片返回，文本/链接为 `null` | 文本原文走 `bodyText`，链接 URL 在 `item.sourceUrl`，避免一个字段三种含义 |
| `tag_rename` / `tag_delete` 未定 UI 入口 | 入口在侧栏标签云（右键/悬停操作） | 详情内不做标签治理，避免误删影响全库 |

## 6. 关键流程

### 6.1 详情读取

```text
get_item_ext(id)
  ├─ Library::get_item(id)            → NOT_FOUND
  ├─ tags_for_item(id)                → Vec<ItemTag>
  ├─ kind == Text  → read texts/<id>.txt，读前 20000 字符
  ├─ kind == Image → original_url = visrecall://original/<id>
  └─ summary = None（阶段 03）
```

原文截断必须按字符边界切：用 `char_indices()` 找第 20000 个字符的字节偏移后 `&text[..offset]`，禁止 `&text[..20000]`（会切断多字节）。读文件失败（不存在/无权限）返回 `bodyText = None` 而不是整条命令失败——详情其余部分仍有价值。

### 6.2 备注保存

前端已实现防抖与 flush，后端只做三件事：校验、单条 UPDATE、发事件。

```rust
pub fn set_note(&self, id: &str, text: &str) -> Result<(), AppError> {
    if text.chars().count() > 10_000 { return Err(AppError::InvalidInput("note too long".into())); }
    // UPDATE items SET note=?2, updated_at=?3 WHERE id=?1；0 行 → NotFound
}
```

并发语义：单写连接串行执行，最后一次 UPDATE 生效。前端在 `savedText` 上做「值相同不重复提交」，后端不做版本号——阶段 03 的 FTS 重建会在这个方法里追加「入队 fts 任务」，届时按 `updated_at` 单调即可。

### 6.3 标签

```text
tag_add(itemId, rawName)
  name = normalize_tag_name(rawName)              // trim、折叠空白、≤32 字符、无控制字符
  BEGIN
    SELECT id FROM tags WHERE name = ?1 COLLATE NOCASE
      命中 → 复用；命中但 source='auto' → 不改 source（保留创建来源）
      未命中 → INSERT tags(id=uuid, name, source='manual')
    INSERT OR IGNORE item_tags(item_id, tag_id, origin='manual', created_at)
    DELETE FROM tag_suppressions WHERE item_id=? AND tag_name=?   -- 手动加回即解除抑制
  COMMIT
  事件：item_tags_updated{itemId} + tags_changed
```

```text
tag_detach(itemId, tagId)
  BEGIN
    SELECT origin FROM item_tags WHERE item_id=? AND tag_id=?     -- 不存在 → NOT_FOUND
    DELETE FROM item_tags ...
    origin='auto' → INSERT OR IGNORE tag_suppressions(item_id, tags.name)
    DELETE FROM tags WHERE id=? AND source='auto'
        AND NOT EXISTS (SELECT 1 FROM item_tags WHERE tag_id=?)
  COMMIT
  事件：item_tags_updated{itemId} + tags_changed
```

`tag_rename`：`UPDATE tags SET name=?`，撞唯一索引时 SQLite 报约束冲突 → 转 `INVALID_INPUT("标签已存在")`。`tag_delete`：`DELETE FROM tags`（关联靠外键级联），事务内先统计影响条数用于提示。

### 6.4 复制原始内容

```text
copy_item_original(id)
  ├─ get_item(id) → NOT_FOUND
  ├─ kind=image → resolve original_path（相对路径校验）→ 不存在 FILE_MISSING
  │                → image::open → RGBA8 → BGRA + 垂直翻转 → CF_DIB
  │                → 同一份 PNG 字节 → 注册格式 "PNG"
  ├─ kind=text  → read texts/<id>.txt（UTF-8）→ CRLF 化 → CF_UNICODETEXT
  └─ kind=link  → source_url → CF_UNICODETEXT
```

Win32 细节：

- `OpenClipboard(None)` 可能因其他进程占用失败；重试 5 次、间隔 20ms，仍失败 → `ClipboardBusy`。
- `EmptyClipboard()` 后 `SetClipboardData`，内存用 `GlobalAlloc(GMEM_MOVEABLE)`；**成功后内存归系统所有，不得 `GlobalFree`**。用一个 `ClipboardGuard` 保证任何提前返回都执行 `CloseClipboard`。
- `CF_DIB` 结构：40 字节 `BITMAPINFOHEADER` + 自底向上的 BGRA 行（`biHeight` 为正，`biBitCount=32`，`biCompression=BI_RGB`，`biSizeImage=w*h*4`）。不写 `BITMAPFILEHEADER`——写了会变成 `CF_DIBV5` 那样的「带文件头」错误格式，粘贴到部分应用会失败。
- PNG 注册格式：`RegisterClipboardFormatW("PNG")`，值为完整 PNG 文件字节（含签名）。
- 文本行尾统一 `\r\n`，末尾补一个 `\0`（UTF-16）。
- 纯函数拆出来便于测试：`dib_bytes(&RgbaImage) -> Vec<u8>`、`utf16_text(&str) -> Vec<u16>`。OS 调用本身不做自动化测试（剪贴板是全局单例，CI 上会互相干扰），列入手工验收。

### 6.5 单条删除

```text
item_delete(id)
  ├─ SELECT original_path, thumb_path（不存在 → NOT_FOUND）
  ├─ BEGIN
  │    DELETE FROM jobs WHERE item_id=?
  │    DELETE FROM items WHERE id=?            -- item_tags / tag_suppressions 级联
  │  COMMIT
  ├─ 文件删除：original_path、thumb_path（NotFound 视为成功；其余 warn）
  └─ 事件 item_deleted{id}
```

先删库后删文件：库是真相来源。反序会在「文件删了、事务失败」时留下一条永远打不开的素材。文件删除失败留下的孤儿由 §6.8 的入口兜底。

### 6.6 全库清空

```text
library_purge(confirm)
  ├─ confirm != "删除全部" → INVALID_INPUT
  ├─ BEGIN
  │    DELETE FROM jobs; DELETE FROM item_tags; DELETE FROM tag_suppressions;
  │    DELETE FROM tags; DELETE FROM items;
  │  COMMIT
  ├─ 清空 originals/ texts/ thumbs/ 下的普通文件（保留目录与 library.json / db）
  └─ 事件 items_reset{}
```

`settings` 表不清（容量/保留策略是用户偏好，不是素材数据）；`schema_migrations` 不动。清空后 `VACUUM` 可选：≤20 条规模下不做，避免大库时长时间锁库。

### 6.7 存储用量、容量与保留

用量：

```text
used_bytes = Σ size(originals/**) + Σ size(texts/**) + Σ size(thumbs/**)
           + size(visrecall.db) + size(visrecall.db-wal)
item_count = SELECT COUNT(*) FROM items
capacity_bytes = settings("storage.capacity_gb", 50) * 1024^3   // 界面按「GB」展示，实际按 GiB 计算
```

遍历用与 `cleanup.rs` 相同的「栈式扫描 + 跳过符号链接」实现，抽成 `library::storage::walk_files`，两处共用。

容量门禁接在抓帧前的既有门禁旁（`capture_flow.rs` 的 `check_free_space` 之后）：注入一个 `usage_probe: Arc<dyn Fn(&Library) -> Option<u64>>`，返回 `Some(used)` 且 `used >= capacity` → `StorageFull`。用注入点而非直接调用，测试才能在不造 50GB 数据的情况下覆盖。

保留：

```text
retention_apply()
  ├─ days = 已保存策略；None → Ok(deleted=0)
  ├─ cutoff = now - days*86_400_000
  ├─ SELECT id FROM items WHERE captured_at < cutoff ORDER BY captured_at ASC LIMIT 500
  └─ 逐条走 delete_item（单条事务 + 文件回收），单条失败计数继续
```

启动时：`AppCore::start_runtime` 之后，在 blocking 线程跑一次 `retention_apply`（`days` 非空时），完成后发 `items_pruned { count }`。清理失败只记 `warn`，不阻止启动。

### 6.8 孤儿文件与缩略图缺失

`orphans_scan()`：扫 `originals/`、`texts/` 得到相对路径集合，减去 `items.original_path` 集合，剩下的即孤儿。**`thumbs/` 单独处理**：`thumb_path` 指向不存在的文件计为「缺失缩略图」，不计入孤儿（可重建，删除它没有收益）。

`orphans_cleanup()`：只删孤儿，逐条 `remove_file`，返回删除计数与字节数；失败逐条 warn。

### 6.9 位置迁移

```text
storage_relocate(path)
  ├─ 守卫：RelocateGuard（原子标记，第二个并发调用 → BUSY）
  ├─ 校验目标（§requirements P2-R09 四条）
  ├─ 源库大小 = usage.used_bytes（仅用于目标盘剩余空间预估）
  ├─ stop_runtime()                       // 丢弃 shell_tx + 覆盖层句柄
  ├─ Library::checkpoint()                // PRAGMA wal_checkpoint(TRUNCATE)
  ├─ copy_tree(src, dst, progress)
  │    逐文件：目标已存在且大小相同 → 跳过（天然续传）
  │    复制顺序：library.json → visrecall.db → originals → texts → thumbs
  │    每 64 个文件或每 200ms 发一次 storage_migration_progress
  ├─ verify(dst)
  │    open read-only → PRAGMA integrity_check = ok
  │    COUNT(*) FROM items 与源一致
  │    library.json.library_id 一致
  ├─ 成功：Config.library_path = dst; save_atomic → 返回 restartRequired=true
  └─ 失败：不动 config；start_runtime() 恢复；返回错误；目标残留文件保留（供诊断/重试）
```

`stop_runtime()` 需要新增（`AppCore` 目前只有 `start_runtime`）：

```rust
pub fn stop_runtime(&self) {
    self.lock_runtime().take();   // Drop：先断 shell_tx，再断覆盖层命令通道
    self.lock_hotkey().take();    // 热键线程在 spawn 时捕获了旧 shell_tx，必须重建
    self.lock_ball().take();      // 球线程持有旧 shell_tx 的 clone；留着会「点了没反应」
}
```

三条都要停，原因各不相同：`CaptureRuntime` 的 `Drop` 让捕获循环退出；热键线程在 `spawn_hotkey_thread` 时就捕获了 `Sender`，通道一断，事件永远投不进新循环；球线程同理，且它的 `Sender` 是 clone，不停会留下一个「能点但投递失败」的球。恢复顺序必须是 `start_runtime()` → `start_hotkey()`（后者依赖前者建好的通道）。迁移失败路径**必须**恢复这三者：漏掉任何一条，用户看到的就是「球没了、热键失灵」，因此这条路径必须有集成测试。

`start_runtime` 会重建球线程与覆盖层线程（`app.rs` 现有的 `*self.lock_ball() = Some(ball)` 逻辑），球窗口会重建一次，位置从配置恢复——这是可接受的可见闪动。

> 迁移失败恢复路径必须调用 `start_runtime()` + `start_hotkey()`；这条路径要有集成测试（注入「校验失败」的假目标），否则用户迁移失败后会以为应用坏了。

迁移期间写操作：`RelocateGuard` 在 `AppCore` 里以 `AtomicBool` 暴露 `is_relocating()`，捕获入口检查后返回 `Busy`。缩略图 worker 可能仍在写 `thumbs/`，这部分文件不保证进入目标库——缩略图可重建（架构 §5.4），且目标库里这些素材的 `thumb_path` 指向缺失文件时界面显示占位，不报错。此边界写入 `verification.md`，不作为缺陷。

### 6.10 悬浮球三态

抽纯状态机 `crates/shell/src/ball/state.rs`，不碰窗口：

```rust
pub enum BallVisual { Ball, Strip, Hidden }

pub enum BallEvent {
    Dragged { pos: PhysicalPoint },
    Released { pos: PhysicalPoint, work_area: PhysicalRect, now_ms: u64 },
    CursorEnter { now_ms: u64 },
    CursorLeave { now_ms: u64 },
    Tick { now_ms: u64, fullscreen: bool },
    ConfigChanged { edge_hide: bool, hide_in_fullscreen: bool, size: u32 },
}

pub struct BallStateMachine { /* visual, anchor_edge, timers, ... */ }
impl BallStateMachine {
    pub fn new(cfg: BallSettings, now_ms: u64) -> Self;
    pub fn handle(&mut self, ev: BallEvent) -> Vec<BallAction>;  // Resize/Show/Hide/MoveTo/SetHitArea
}
```

规则：

- `Released` 距工作区任一左右/上下边缘 ≤12px 且 `edge_hide=true` → 记 `anchor_edge`，启动 800ms 定时器；`Tick` 到点且鼠标不在热区 → `Strip`。
- `Strip` 下 `CursorEnter` → 150ms 后 `Ball`；`CursorLeave` → 600ms 后回 `Strip`。
- `Tick.fullscreen && hide_in_fullscreen` → `Hidden`；全屏退出 → 回到 `anchor_edge` 决定的形态。
- `ConfigChanged.size` → `BallAction::Resize`，同时把当前位置夹回工作区（球变大后可能越界）。
- 捕获会话（`set_visible(false)`）与状态机解耦：会话只覆盖「可见性」，不改状态机形态；会话结束时按状态机当前形态恢复。

窗口侧改动集中在 `ball.rs`：`WM_BALL_SET_SIZE` 消息（重建缓存位图 + `SetWindowPos` 保持中心点）、`WM_TIMER` 分流（800ms 收起、150ms 展开、600ms 回缩、1s 全屏检测）、`TrackMouseEvent` 处理 `WM_MOUSELEAVE`。长条形态的外扩热区**不新开窗口**：窗口按「长条 5px + 两侧各 6px 透明留白」创建，绘制时把长条画在留白内，命中由窗口矩形自然覆盖。不使用 `SetWindowRgn`——它会同时裁掉绘制与命中，并且与分层窗口的 per-pixel alpha 叠加后行为不可预期。

全屏检测：`SHQueryUserNotificationState()` 返回 `QUNS_BUSY` / `QUNS_RUNNING_D3D_FULL_SCREEN` / `QUNS_PRESENTATION_MODE` 视为全屏；调用失败按「非全屏」。1s 一次的定时器在球可见时才运行，隐藏时停掉，避免空闲 CPU 超标。

### 6.11 热键热重载

`AppCore::update_settings` 的新顺序（顺序即正确性）：

```text
1. parse 新快捷键            → 失败 INVALID_INPUT（不落盘、不改内存）
2. 若与当前不同：
     self.spawn_capture_hotkey(new_spec)   → 失败（占用）INVALID_INPUT（旧句柄仍有效）
3. 落盘 config（失败 → 停掉新线程，返回 IO；旧值不变）
4. 更新内存 config
5. 若换了键：hotkey.take() 停旧线程；存入新句柄
6. 球相关变更：apply_ball_visibility / BallHandle::set_size / 状态机配置
```

先起新线程再停旧线程：反序会在注册失败的瞬间丢掉唯一可用的快捷键。两个不同组合键可同时注册，所以第 2 步不会与旧线程冲突。

为此把 `start_hotkey` 里「选通道 + spawn + fallback 消费线程」的逻辑抽成私有方法，供启动与热重载共用：

```rust
fn spawn_capture_hotkey(&self, spec: HotkeySpec) -> Result<HotkeyHandle, AppError>;
```

它内部保留既有语义：运行时存在时用 `runtime.shell_tx.clone()`，否则新建临时通道并 `spawn_shell_event_consumer`（收到请求只发 `CAPTURE_UNAVAILABLE`）。不要为此新增公开的 `shell_sender()`——热重载与启动需要完全一致的降级行为，两处各写一遍必然漂移。

注册成功后必须清掉托盘上的历史冲突提示：`HotkeyConflict` 会把 tooltip 改成「快捷键不可用：…」，若不清理，改键成功后托盘会一直停留在上一次的失败文案。实现方式是在第 5 步之后发 `AppEvent::HotkeyRestored`，由 `TauriEventSink` 调 `tray.set_tooltip(Some("目记 VisRecall"))` 复原；`app-core` 不直接依赖托盘。

## 7. 线程、锁与顺序

现有锁：`config: Mutex<AppConfig>`、`ball: Arc<Mutex<Option<BallHandle>>>`、`hotkey: Mutex`、`runtime: Mutex`、`Library.write: Mutex<Connection>`。

新增约束：

1. **配置锁先于壳层锁**。`update_settings` 已在改内存后 `drop(guard)` 再碰球线程；热键重载沿用同一顺序（配置锁释放后再 `hotkey.take()`），否则与捕获循环的「球位置落盘」形成反向持锁。
2. **库连接锁不跨文件 IO**。删除顺序是「事务提交 → 释放连接 → 删文件」，`item_delete` 的文件删除在 `conn` guard 释放后进行。
3. **迁移期间只允许一个长任务**。`RelocateGuard` 同时被迁移与保留清理复用（`Busy`），避免「正在迁移时启动清理把源库删了一半」。
4. **事件发送不持锁**。所有 `sink.emit` 在锁释放后调用；`TauriEventSink::emit` 会走 Tauri 的事件通道，持锁发送容易在窗口销毁时卡住。

## 8. 失败与降级矩阵

| 场景 | 后端行为 | 前端表现 |
|---|---|---|
| 原图文件缺失 | `FILE_MISSING`（复制）/ `originalUrl` 仍返回但协议 404 | 详情占位「原图文件缺失」；复制提示失败 |
| 文本文件缺失 | `bodyText = null` | 原文区显示「原文不可读」 |
| 缩略图缺失 | 协议 404 | 卡片/详情显示类型占位 |
| 剪贴板被占用 | 重试 5×20ms → `CLIPBOARD_BUSY` | 可重试 Toast |
| 标签重名 | 复用不报错 | 无感 |
| 标签改名撞名 | `INVALID_INPUT` | 输入框下方错误，不关闭 |
| 删除时文件删不掉 | 库已删、`warn` | 删除成功；孤儿统计里能看到 |
| 容量已满 | 捕获前 `STORAGE_FULL` | 托盘提示 + 主窗口 Toast「库已满，请清理」 |
| 迁移目标非空 | `INVALID_INPUT` | 行内错误，保留对话框 |
| 迁移校验失败 | 不改 config、恢复运行时 | 错误 Toast + 「原库未受影响」说明 |
| 迁移中捕获 | `BUSY` | 捕获按钮禁用 + 提示「正在迁移」 |
| 保留清理单条失败 | 计数继续 | 汇总「成功 N / 失败 M」 |
| 策略配置损坏 | 按默认值 + `warn` | 设置页显示默认值 |
| 库打不开 | 既有 `library_ok=false` | 既有异常态；所有新命令返回 `CONFIG_MISSING` |

## 9. 测试策略

### 9.1 分层

| 层 | 范围 | 工具 |
|---|---|---|
| 纯函数单测 | 标签名规范化、色令牌散列、DIB 字节布局、UTF-16/CRLF、原文按字符截断、球状态机 | `#[test]` |
| library 集成测试 | 迁移幂等、标签 CRUD 与级联、备注长度、删除事务、清空、用量求和、保留选条、孤儿扫描、迁移复制/校验 | `tempfile` + 真实 SQLite |
| app-core 集成测试 | 详情聚合、复制编排（注入假剪贴板 trait）、容量门禁、迁移失败恢复运行时、热键重载顺序（同进程占用组合模拟冲突） | 既有的 `RecordingSink` 模式 |
| 协议测试 | `/original/<id>` 解析、越界拒绝、缺文件 404、方法限制 | 纯响应构造（沿用 `protocol.rs` 现有风格） |
| 前端组件测试 | 详情标签增删、备注四态、存储 pane 写控件、侧栏标签云 | Vitest + Testing Library |
| 手工验收 | 复制到 Word/微信、迁移到另一块盘、球三态、全屏隐藏、双屏/DPI 矩阵 | `verification.md` |

### 9.2 可测性注入点

沿用阶段 01 的做法，把 OS/环境相关调用做成可注入的闭包或 trait：

```rust
pub type UsageProbe = Arc<dyn Fn(&Library) -> Option<u64> + Send + Sync>;   // 容量门禁
pub trait ClipboardWriter: Send + Sync {
    fn write_image(&self, png: &[u8], dib: &[u8]) -> Result<(), AppError>;
    fn write_text(&self, text: &str) -> Result<(), AppError>;
}
```

生产实现落到 `shell::clipboard`；测试用记录型假实现断言「调用了几次、参数是什么、失败时是否把错误码透传」。

热键**不引入注册器抽象**：`RegisterHotKey` 是线程级的，同一个组合键在进程内第二个线程注册会直接失败，这天然就是「被占用」的模拟。测试用真实的 `spawn_hotkey_thread` 占用目标组合，再调 `update_settings`，断言错误码与旧句柄未变——比造一个 trait 更接近生产路径。

### 9.3 必须覆盖的边界

- 10000 字备注的最后一个字符（第 10001 个被拒）；含 emoji 的计数。
- 20000 字符原文截断处落在 emoji / 代理对上。
- 同名标签大小写混输；只差首尾空白的名称。
- 删除素材后 `jobs` 无残留（阶段 03 会跑这些任务，残留会导致「任务指向不存在的素材」）。
- 迁移目标路径包含当前库前缀（`C:\lib` vs `C:\lib2`）：必须用路径组件比较，不能用字符串前缀。
- 迁移中断（模拟 `copy_tree` 中途返回错误）后 `config.json` 未变、运行时恢复。
- 保留清理在 `days=null` 时不删任何东西（防止默认值把库清空）。

## 10. 风险与门禁

| 风险 | 影响 | 缓解 |
|---|---|---|
| 剪贴板 API 在无消息循环线程上失败 | 复制不可用 | 实测 `spawn_blocking` 线程；失败则把剪贴板调用放到专用线程（带消息循环） |
| `CF_DIB` 字节序/翻转写错 | 粘贴出上下颠倒或全黑 | 纯函数单测校验前两行像素位置；手工粘贴到 Word/微信 |
| 迁移期间缩略图写入源库 | 目标库缺缩略图 | 已确认可重建，界面占位；写入文档不作为缺陷 |
| 球尺寸热切换导致窗口闪烁/位置漂移 | 观感差 | 保持中心点不变 + 夹回工作区；`SetWindowPos` 一帧内完成 |
| 全屏检测误判（游戏/远程桌面） | 球意外隐藏 | 1s 采样 + 三次连续命中才隐藏；可关闭该开关 |
| release 未启用 custom-protocol | 发布版白屏 | `tauri.conf.json` 显式配置 + 验收直跑 release 二进制 |
| 迁移校验只看行数 | 半损坏库被切换 | 加 `PRAGMA integrity_check` + `library_id` 双重校验 |
| 阶段 01 门禁补齐依赖硬件 | 无法全部勾选 | 允许「环境限制未验证」并留替代证据，禁止静默勾选 |

## 11. 门禁与阶段收获

- 阶段 02 退出前必须回填 `01-capture-loop/verification.md` 的 README §1 清单（详见 `verification.md` §1）。
- 本阶段的技术债结转：gc 重试队列、迁移续传/热切换、保留预告、原图缩放参数、虚拟滚动 → 全部登记在本文件 §1.2 与 `verification.md` §4，不留在代码注释里。
