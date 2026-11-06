# VisRecall V0.1 阶段 02：存储与详情 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans` to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把 `src/lib/api.ts` 的 stub 全部换成真实命令，补齐备注、标签、复制、删除、容量/保留、位置迁移与悬浮球三态，并回填阶段 01 遗留门禁。

**Architecture:** 存储层（`visrecall-library`）承担 SQL 与文件操作，编排层（`visrecall-app-core`）承担事务边界、事件与长任务闸门，壳层（`visrecall-shell`）承担剪贴板与悬浮球状态机，`src-tauri` 只做 DTO 映射与命令注册；前端只改 `api.ts` 与四个组件的接线。

**Tech Stack:** Rust 1.93（MSVC）、Tauri 2、React 19、TypeScript、SQLite（rusqlite bundled）、`windows` crate、`tauri-specta`、TanStack Query、Vitest。

## Global Constraints

- 仅 Windows 10 22H2（19045）+ / Windows 11；x64。
- 单进程 `visrecall.exe`；UI 线程不做同步 IO/文件遍历/数据库写。
- 数据全本地、明文；本阶段不引入任何网络请求。
- 素材测试规模 **≤20 条**（构成见 `verification.md` §0.1）；不做万级压测、不实现虚拟滚动；20 条结论不外推为 100 条规模的性能结论。
- 详情读取 P95 ≤80ms；单次标签操作 ≤100ms；用量统计 ≤150ms。
- 托盘常驻 ≤150MB（目标 40–70MB）；空闲 CPU <1%。
- 数据库中的文件路径一律相对库根目录；协议处理器不接受任意路径。
- 所有不可恢复操作（单条删除、全库清空、孤儿清理）必须有确认，确认词判断在 Rust 侧。
- Release 二进制必须能脱离 `devUrl` 运行（`tauri/custom-protocol`）。
- 依赖锁 `Cargo.lock` / `pnpm-lock.yaml`；每个任务结束提交一次。
- 事件只携带 id / 小载荷；大文本与图片字节不进事件。

---

### Task 1: 迁移 0002 与迁移前备份

**Files:**
- Create: `crates/library/migrations/0002_library_detail.sql`
- Modify: `crates/library/src/migrations.rs`
- Modify: `crates/library/src/db.rs`（`Library::open` 传根目录给 `apply`）
- Create: `crates/library/src/settings_kv.rs`

**Interfaces:**
- Consumes: 0001 已建 `items` / `settings` / `schema_migrations`
- Produces: `migrations::apply(conn, root)`；`Library::setting_get/setting_set`；`tags` / `item_tags` / `tag_suppressions` 三张表

- [x] **Step 1: 写 0002 迁移**

```sql
-- crates/library/migrations/0002_library_detail.sql
CREATE TABLE tags (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  source TEXT NOT NULL CHECK (source IN ('auto','manual')),
  created_at INTEGER NOT NULL
);
CREATE UNIQUE INDEX idx_tags_name_nocase ON tags(name COLLATE NOCASE);

CREATE TABLE item_tags (
  item_id TEXT NOT NULL REFERENCES items(id) ON DELETE CASCADE,
  tag_id TEXT NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
  origin TEXT NOT NULL DEFAULT 'manual' CHECK (origin IN ('auto','manual')),
  created_at INTEGER NOT NULL,
  PRIMARY KEY (item_id, tag_id)
);
CREATE INDEX idx_item_tags_tag ON item_tags(tag_id);

CREATE TABLE tag_suppressions (
  item_id TEXT NOT NULL REFERENCES items(id) ON DELETE CASCADE,
  tag_name TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (item_id, tag_name)
);
```

- [x] **Step 2: 迁移前备份 + 新签名**

```rust
pub const MIGRATIONS: &[(i64, &str)] = &[
    (1, include_str!("../migrations/0001_core.sql")),
    (2, include_str!("../migrations/0002_library_detail.sql")),
];

pub fn apply(conn: &mut Connection, root: &Path) -> Result<(), AppError> {
    conn.execute_batch(MIGRATIONS_TABLE).map_err(db_error)?;
    let applied = applied_versions(conn)?;
    let pending: Vec<_> = MIGRATIONS.iter().filter(|(v, _)| !applied.contains(v)).collect();
    if !pending.is_empty() {
        backup_before_migration(conn, root);   // 失败只 warn，不阻断
    }
    // …原有逐条事务逻辑
}

/// `backups/visrecall.db.bak-<version>`，保留最近 2 份。
fn backup_before_migration(conn: &Connection, root: &Path) -> Option<PathBuf> {
    let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
    let backups = root.join("backups");
    fs::create_dir_all(&backups).ok()?;
    let target = backups.join(format!("visrecall.db.bak-{}", pending_version()));
    fs::copy(root.join(DB_FILE), &target).ok()?;
    prune_backups(&backups, 2);
    Some(target)
}
```

`db.rs` 的 `Library::open` 改为 `migrations::apply(&mut conn, &root)?`；`migrations.rs` 的两个既有测试传 `tempfile::tempdir()` 的路径。

> 备份路径里的 checkpoint 是**最佳努力**：启动时还没有其他连接，且备份失败只 `warn`，所以这里不判定 `busy`。严格判定（`busy != 0` 即失败）只用于位置迁移，见 Task 6 的 `checkpoint()`。

- [x] **Step 3: `settings` 键值读写**

```rust
// crates/library/src/settings_kv.rs
impl Library {
    pub fn setting_get<T: DeserializeOwned>(&self, key: &str) -> Option<T>;
    pub fn setting_set<T: Serialize>(&self, key: &str, value: &T) -> Result<(), AppError>;  // INSERT OR REPLACE
}
```

解析失败返回 `None` 并 `warn!`（不返回错误，避免一条坏配置卡住设置页）。

- [x] **Step 4: 补测试**

- 连续 `apply` 两次幂等，`schema_migrations` 有 2 行。
- 在已有 0001 库上应用 0002 后，`tags` 等三表可写；`PRAGMA foreign_keys` 打开时删除 `items` 行会级联清空 `item_tags`。
- 迁移前备份：预置一个 0001 库 → `apply` → `backups/` 出现 1 个 `.bak-2`。
- `setting_set` 后 `setting_get` 往返一致；写入非法 JSON 的键后 `setting_get` 返回 `None`。

**Verify:**

```bash
cargo test -p visrecall-library
cargo clippy -p visrecall-library --all-targets -- -D warnings
```

**Commit:** `feat(library): add tag schema and migration backup`

---

### Task 2: 领域类型与错误码

**Files:**
- Create: `crates/domain/src/tag.rs`
- Create: `crates/domain/src/storage.rs`
- Modify: `crates/domain/src/error.rs`
- Modify: `crates/domain/src/lib.rs`

**Interfaces:**
- Consumes: 无
- Produces: `Tag` / `TagSource` / `ItemTag` / `normalize_tag_name` / `hue_for` / `StorageUsage` / `RetentionPolicy` / `OrphanStats` / `RelocateReport` / 4 个新错误变体

- [x] **Step 1: 标签类型与规范化**

```rust
pub const TAG_HUES: [&str; 7] = ["ochre", "slate", "jade", "ash", "indigo", "plum", "paper"];
pub const MAX_TAG_CHARS: usize = 32;

pub fn normalize_tag_name(raw: &str) -> Result<String, AppError> {
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return Err(AppError::InvalidInput("tag name is empty".into()));
    }
    if collapsed.chars().count() > MAX_TAG_CHARS {
        return Err(AppError::InvalidInput(format!("tag name longer than {MAX_TAG_CHARS} chars")));
    }
    if collapsed.chars().any(|c| c.is_control()) {
        return Err(AppError::InvalidInput("tag name has control chars".into()));
    }
    Ok(collapsed)
}

/// FNV-1a 32 位，对 NFKC 后的小写名称散列；同一名称在任何机器上颜色一致。
pub fn hue_for(name: &str) -> &'static str {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in name.to_lowercase().as_bytes() {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    TAG_HUES[(hash as usize) % TAG_HUES.len()]
}
```

- [x] **Step 2: 存储类型**

按 `design.md` §4.4 写 `StorageUsage` / `RetentionPolicy` / `OrphanStats` / `RelocateReport`，全部 `Clone, Debug, PartialEq, Eq, Serialize, Deserialize`。

- [x] **Step 3: 错误码**

```rust
#[error("file missing: {0}")]       FileMissing(String),
#[error("clipboard busy")]          ClipboardBusy,
#[error("clipboard failed: {0}")]   ClipboardFailed(String),
#[error("busy: {0}")]               Busy(String),
```

`error.rs` 的 `display_messages_are_stable` 补 4 条断言；`src-tauri/src/ipc.rs` 的映射与测试本任务先不改（Task 10 统一改）。

- [x] **Step 4: 测试**

`normalize_tag_name`：`"  a   b  "` → `"a b"`；空串/全空白/33 个字符/含 `\u{7}` 全部拒绝；`hue_for` 对同一名称稳定、对 1000 个不同名称至少命中 5 个不同令牌。

**Verify:**

```bash
cargo test -p visrecall-domain
cargo clippy -p visrecall-domain --all-targets -- -D warnings
```

**Commit:** `feat(domain): add tag and storage types`

---

### Task 3: 库层标签 CRUD

**Files:**
- Create: `crates/library/src/tags.rs`
- Create: `crates/library/tests/tags_db.rs`
- Modify: `crates/library/src/lib.rs`

**Interfaces:**
- Consumes: `tags` / `item_tags` / `tag_suppressions`、`normalize_tag_name`、`hue_for`
- Produces:
  - `Library::tags_list() -> Result<Vec<ItemTag>, AppError>`（含 `item_count`）
  - `Library::tags_for_item(item_id) -> Result<Vec<ItemTag>, AppError>`
  - `Library::tag_add(item_id, name) -> Result<ItemTag, AppError>`
  - `Library::tag_detach(item_id, tag_id) -> Result<(), AppError>`
  - `Library::tag_rename(tag_id, name) -> Result<(), AppError>`
  - `Library::tag_delete(tag_id) -> Result<u64, AppError>`（返回解除的关联数）

- [x] **Step 1: 实现查询**

```sql
-- tags_list
SELECT t.id, t.name, t.source, t.created_at, COUNT(it.item_id)
FROM tags t LEFT JOIN item_tags it ON it.tag_id = t.id
GROUP BY t.id ORDER BY COUNT(it.item_id) DESC, t.name ASC;

-- tags_for_item
SELECT t.id, t.name, t.source, t.created_at, it.origin
FROM item_tags it JOIN tags t ON t.id = it.tag_id
WHERE it.item_id = ?1 ORDER BY it.created_at ASC;
```

`ItemTag.item_count` 在按素材查询时填 0；`hue` 由 `hue_for(&tag.name)` 填。

- [x] **Step 2: 实现写入（严格按 design §6.3 的事务边界）**

`tag_add` 必须先 `get_item` 校验素材存在（否则外键报错会变成 `DATABASE`，前端无法区分）；同名查找用 `WHERE name = ?1 COLLATE NOCASE`；插入关联用 `INSERT OR IGNORE`，已存在时仍返回现有 chip。

`tag_detach` 必须先读 `origin`，`auto` 时把标签名写入 `tag_suppressions`；随后删除无关联的 `source='auto'` 标签。

`tag_rename` 与 `tag_delete` 对不存在的 id 返回 `NOT_FOUND`；改名撞唯一索引时把 `rusqlite::Error::SqliteFailure` 的 `ConstraintViolation` 映射为 `InvalidInput("tag name already exists")`。

- [x] **Step 3: 事件由调用方发（本任务不发）**

库层只返回结果，`app-core` 负责 `item_tags_updated` / `tags_changed`（Task 10）。

- [x] **Step 4: 测试**

- 添加同名标签到两条素材 → `tags` 只有 1 行，`item_tags` 有 2 行。
- `"Work"` 与 `"work"` 复用同一条标签。
- detach 后 `item_tags` 清空；手动标签仍在 `tags`；`source='auto'` 的标签被删除且 `tag_suppressions` 有记录。
- rename 撞名返回 `InvalidInput`；delete 返回解除条数。
- 删除 `items` 行后 `item_tags` 自动级联清空。

**Verify:**

```bash
cargo test -p visrecall-library --test tags_db
```

**Commit:** `feat(library): add tag storage and suppressions`

---

### Task 4: 备注与详情读模型

**Files:**
- Modify: `crates/library/src/items.rs`
- Create: `crates/library/tests/detail_db.rs`

**Interfaces:**
- Consumes: `items.note`
- Produces: `Library::set_note(id, text)`、`Library::loading_note(id)`、`Library::count_items()`、`Library::list_expired(cutoff, limit)`

- [x] **Step 1: `set_note`**

```rust
pub const MAX_NOTE_CHARS: usize = 10_000;

pub fn set_note(&self, id: &str, text: &str) -> Result<(), AppError> {
    if text.chars().count() > MAX_NOTE_CHARS {
        return Err(AppError::InvalidInput(format!("note longer than {MAX_NOTE_CHARS} chars")));
    }
    // UPDATE items SET note=?2, updated_at=?3 WHERE id=?1；0 行 → NotFound
}
```

- [x] **Step 2: 计数与过期选取**

```rust
pub fn count_items(&self) -> Result<u64, AppError>;
pub fn count_items_before(&self, cutoff: i64) -> Result<u64, AppError>;
pub fn list_expired(&self, cutoff: i64, limit: u32) -> Result<Vec<Item>, AppError>;
```

`list_expired` 按 `captured_at ASC, rowid ASC` 排序，`limit` clamp 1..=500。

- [x] **Step 3: 单条事务删除的内部方法**

```rust
/// 删除一条：jobs + items（标签靠外键级联）。调用方负责文件删除与事件。
pub(crate) fn delete_item_row(&self, id: &str) -> Result<Option<(String, Option<String>)>, AppError>;
```

返回 `(original_path, thumb_path)` 供上层删文件；`None` 表示记录不存在。**必须在同一个连接 guard 内完成两条 DELETE 与提交**。

- [x] **Step 4: 测试**

- 10000 字（含 emoji，按字符计）写入成功；10001 字返回 `InvalidInput` 且库内值未变。
- 空串备注写入成功（清空备注）。
- 删不存在的 id → `NotFound`；删除后 `jobs` 无残留（预先 `enqueue_job`）。
- `list_expired` 只返回早于 cutoff 的项，按时间升序。

**Verify:**

```bash
cargo test -p visrecall-library --test detail_db
cargo test -p visrecall-library --test items_db
```

**Commit:** `feat(library): add note writes and detail queries`

---

### Task 5: 单条删除与全库清空

**Files:**
- Create: `crates/library/src/purge.rs`
- Create: `crates/library/tests/purge_db.rs`
- Modify: `crates/library/src/lib.rs`

**Interfaces:**
- Consumes: `delete_item_row`、`Library::root`、`files`
- Produces:
  - `Library::delete_item(id) -> Result<DeletedItem, AppError>`（`DeletedItem { original_path, thumb_path }`）
  - `Library::purge_library() -> Result<PurgeReport, AppError>`（`PurgeReport { items, tags, files_removed, files_failed }`）
  - `Library::clear_content_dirs() -> Result<(u64, u64), AppError>`

- [x] **Step 1: 单条删除（库删完再删文件）**

```rust
pub fn delete_item(&self, id: &str) -> Result<DeletedItem, AppError> {
    let paths = self.delete_item_row(id)?.ok_or(AppError::NotFound)?;
    let mut failed = 0;
    for rel in [Some(paths.0.clone()), paths.1.clone()].into_iter().flatten() {
        if let Ok(abs) = self.relative_to_root(&rel) {
            match fs::remove_file(&abs) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => { failed += 1; tracing::warn!(path = %abs.display(), %err, "delete file failed"); }
            }
        }
    }
    Ok(paths)
}
```

`relative_to_root` 要求文件存在（内部 `canonicalize`）；文件已被手动删除时它会返回 `Io`，因此**先用 `self.root().join(rel)` 拼绝对路径并做组件校验**，不要直接依赖 `relative_to_root`。为此新增 `Library::resolve_existing_or_join(rel)`。

- [x] **Step 2: 清空**

事务内按 `jobs → item_tags → tag_suppressions → tags → items` 顺序 DELETE（先子后父，避免外键顺序问题）；随后 `clear_content_dirs()` 删除 `originals/`、`texts/`、`thumbs/` 下普通文件（复用 `cleanup.rs` 的 `collect_files`，把它提到 `pub(crate)`）。

- [x] **Step 3: 测试**

- 删除后文件消失、`jobs`/`item_tags` 无残留；原图文件已被手动删掉时仍返回成功。
- 清空后 5 张表计数为 0，`settings` 与 `schema_migrations` 保留，三个内容目录为空但存在。
- 清空一个空库返回全 0，不报错。

**Verify:**

```bash
cargo test -p visrecall-library --test purge_db
```

**Commit:** `feat(library): delete items and purge library`

---

### Task 6: 用量、保留与孤儿统计

**Files:**
- Create: `crates/library/src/storage.rs`
- Create: `crates/library/tests/storage_db.rs`
- Modify: `crates/library/src/lib.rs`、`crates/library/src/cleanup.rs`

**Interfaces:**
- Consumes: `walk`、`items`、`settings_kv`
- Produces:
  - `Library::usage() -> Result<StorageUsage, AppError>`
  - `Library::capacity_bytes() -> u64`（默认 50 GiB）
  - `Library::set_capacity_gb(gb: u32) -> Result<(), AppError>`
  - `Library::retention() -> RetentionPolicy`
  - `Library::set_retention(days: Option<u32>) -> Result<(), AppError>`
  - `Library::orphans() -> Result<OrphanStats, AppError>`
  - `Library::cleanup_orphans() -> Result<OrphanStats, AppError>`
  - `Library::checkpoint() -> Result<(), AppError>`

- [x] **Step 1: 目录遍历（与 cleanup 共用）**

```rust
pub(crate) fn walk_files(base: &Path) -> Vec<PathBuf>;      // 从 cleanup.rs 移出并改为 pub(crate)
pub(crate) fn dir_size(files: &[PathBuf]) -> u64;
```

- [x] **Step 2: 用量**

`used = originals + texts + thumbs + visrecall.db + visrecall.db-wal`；`item_count = count_items()`。`-wal` 不存在时按 0 计。

- [x] **Step 3: 策略读写与校验**

`capacity_gb` 1..=4096，`retention_days` `None | 1..=3650`；越界 `InvalidInput`，且**先校验再写库**。

- [x] **Step 4: 孤儿**

`orphans()`：`originals`、`texts` 的文件集合减去 `items.original_path` 集合；`thumbs` 只统计「`thumb_path` 指向不存在文件」的数量，放进 `OrphanStats.missing_thumbs`。

`cleanup_orphans()` 只删 `files` 部分，逐条 `remove_file` 并汇总实际删除数。

**范围白名单（硬约束）**：扫描根只有 `originals/` 与 `texts/` 两个目录，逐个文件与 `items.original_path` 比对。不得遍历库根目录，不得触碰 `visrecall.db`、`visrecall.db-wal`、`visrecall.db-shm`、`library.json`、`backups/`、`tmp/`——`tmp/` 的清理属于启动流程，`backups/` 是迁移回滚的凭据。

实现上不由调用方传目录：把根集合写成常量，新增目录必须改这一处常量，避免以后被误扩展。

- [x] **Step 5: checkpoint**

```rust
pub fn checkpoint(&self) -> Result<(), AppError> {
    let conn = self.conn()?;
    let busy: i64 = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))
        .map_err(db_error)?;
    if busy != 0 {
        return Err(AppError::Busy(format!("checkpoint incomplete ({busy} busy)")));
    }
    Ok(())
}
```

必须用 `query_row` 读第一列，不能用 `execute_batch`：后者拿不到 `busy` 计数，部分 checkpoint 会被当成成功——那正是「复制出一个缺帧的库」的入口。

- [x] **Step 6: 测试**

- 写入 3 个文件 + 1 条素材，`usage().used_bytes` 与手工求和一致；`item_count = 1`。
- 容量 0 / 4097 被拒；4096 成功。
- `retention_days` 0 / 3651 被拒；`None` 成功。
- 放一个未入库文件 → `orphans().files == 1`；清理后文件消失、返回 1。
- 删除某缩略图 → `missing_thumbs == 1`。
- **清理不越界**：`cleanup_orphans()` 之后 `visrecall.db`、`-wal`、`-shm`、`library.json` 仍然存在且 mtime 未变，`count_items()` 与清理前一致。
- **checkpoint 判定**：无其他连接时 `checkpoint()` 返回 `Ok` 且 `-wal` 归零；另开一条连接持读事务时返回 `Err(Busy)`。

**Verify:**

```bash
cargo test -p visrecall-library --test storage_db
```

**Commit:** `feat(library): track storage usage, retention and orphans`

---

### Task 7: 详情聚合（app-core）

**Files:**
- Create: `crates/app-core/src/detail.rs`
- Create: `crates/app-core/tests/detail_extras.rs`
- Modify: `crates/app-core/src/lib.rs`

**Interfaces:**
- Consumes: `Library::get_item` / `tags_for_item` / `root`
- Produces: `detail::load_detail(library, id) -> Result<ItemDetail, AppError>`，`ItemDetail { item: Item, extras: DetailExtras }`

- [x] **Step 1: 原文读取与按字符截断**

```rust
pub const BODY_TEXT_LIMIT: usize = 20_000;

fn read_body_text(library: &Library, item: &Item) -> Option<String> {
    if item.kind != ItemKind::Text { return None; }
    let path = library.root().join(&item.original_path);
    let text = fs::read_to_string(path).ok()?;           // 非 UTF-8 时 None
    let mut chars = text.char_indices();
    match chars.nth(BODY_TEXT_LIMIT) {
        Some((byte_idx, _)) => Some(format!("{}…（已截断）", &text[..byte_idx])),
        None => Some(text),
    }
}
```

`nth(BODY_TEXT_LIMIT)` 返回第 20001 个字符的位置，`&text[..byte_idx]` 恰为前 20000 个字符。

- [x] **Step 2: 原图 URL 与标签**

`original_url` 仅在 `ItemKind::Image` 时返回 `format!("visrecall://original/{}", item.id)`；标签来自 `tags_for_item`，`auto` 取 `origin == TagSource::Auto`。

- [x] **Step 3: 测试**

- 文本 19999 字 → 无截断标记；20000 字 → 无标记；20001 字 → 有标记且长度为 20000 + 标记。
- 截断点前后放 emoji：`…a😀` + 剩余 → 输出以完整 emoji 结尾，不出现替换字符 `�`。
- 图片素材 `body_text = None`、`original_url = Some`；链接素材两者皆 `None`。
- 原图文件被删：`load_detail` 仍成功，`original_url` 仍返回（协议层报 404）。
- 素材不存在 → `NotFound`。

**Verify:**

```bash
cargo test -p visrecall-app-core --test detail_extras
```

**Commit:** `feat(app-core): aggregate item detail extras`

---

### Task 8: 原图协议

**Files:**
- Modify: `src-tauri/src/protocol.rs`
- Modify: `crates/app-core/src/thumbnail.rs`（新增 `load_original_bytes`，与 `load_thumb_bytes` 同构）

**Interfaces:**
- Consumes: `Library::get_item`、`relative_to_root`
- Produces: `GET|HEAD /original/<id>` 返回原图字节；`ORIGINAL_PREFIX = "original"`

- [x] **Step 1: 泛化路径解析**

把 `parse_thumb_request_path` 改成 `parse_asset_request_path(path) -> Result<(AssetKind, &str), ProtocolError>`，`AssetKind::{Thumb, Original}`；只接受 `/<prefix>/<id>` 两段，id 规则复用 `is_valid_item_id`。保留 `parse_thumb_request_path` 作为薄封装，避免既有测试大改。

- [x] **Step 2: 读取原图**

```rust
pub fn load_original_bytes(library: &Library, item_id: &str) -> Result<(Vec<u8>, &'static str), AppError>;
```

按 `items.kind`：`Image` 读 `original_path`，扩展名映射 `png→image/png`、`jpg/jpeg→image/jpeg`、`webp→image/webp`、`gif→image/gif`、其他→`application/octet-stream`；非图片素材返回 `InvalidInput`（协议转 400）。纵深防御：`original_path` 必须以 `originals/` 开头，否则 `InvalidInput`。

- [x] **Step 3: 响应**

沿用缩略图分支：GET/HEAD、`immutable` 缓存头、错误不缓存；`Content-Type` 用上一步的 MIME。

- [x] **Step 4: 测试**

- `/original/<id>` 返回 200 + `image/png`；`/original/../config.json`、`/original/a.png`、`/original/a/b` 全部 400。
- `thumb_path` 指向 `texts/` 的素材走 `/original/<id>` 返回 400（不读出文本）。
- 文本素材 `/original/<id>` → 400；不存在 id → 404。
- 非 GET/HEAD → 405，`Allow: GET, HEAD`。

**Verify:**

```bash
cargo test -p visrecall
```

**Commit:** `feat(protocol): serve original images by item id`

---

### Task 9: 剪贴板写入

**Files:**
- Create: `crates/shell/src/clipboard.rs`
- Create: `crates/shell/tests/clipboard_bytes.rs`
- Modify: `crates/shell/src/lib.rs`

**Interfaces:**
- Consumes: `image` crate、`windows`
- Produces:
  - `clipboard::dib_bytes(rgba: &[u8], width: u32, height: u32) -> Vec<u8>`（纯函数）
  - `clipboard::utf16_crlf(text: &str) -> Vec<u16>`（纯函数）
  - `clipboard::copy_image_png(png: &[u8]) -> Result<(), AppError>`
  - `clipboard::copy_text(text: &str) -> Result<(), AppError>`

- [x] **Step 1: 纯函数**

```rust
/// 40 字节 BITMAPINFOHEADER + 自底向上 BGRA 行；不含 BITMAPFILEHEADER。
pub fn dib_bytes(rgba: &[u8], width: u32, height: u32) -> Vec<u8> {
    let row = width as usize * 4;
    let mut out = Vec::with_capacity(40 + row * height as usize);
    out.extend_from_slice(&40u32.to_le_bytes());          // biSize
    out.extend_from_slice(&(width as i32).to_le_bytes()); // biWidth
    out.extend_from_slice(&(height as i32).to_le_bytes()); // biHeight 正数 = bottom-up
    out.extend_from_slice(&1u16.to_le_bytes());           // biPlanes
    out.extend_from_slice(&32u16.to_le_bytes());          // biBitCount
    out.extend_from_slice(&0u32.to_le_bytes());           // BI_RGB
    out.extend_from_slice(&((row * height as usize) as u32).to_le_bytes());
    out.extend_from_slice(&[0u8; 16]);                    // 分辨率与调色板
    for y in (0..height as usize).rev() {
        for x in 0..width as usize {
            let i = (y * width as usize + x) * 4;
            out.extend_from_slice(&[rgba[i + 2], rgba[i + 1], rgba[i], rgba[i + 3]]);
        }
    }
    out
}

/// CRLF 化 + UTF-16LE + 结尾 NUL。
pub fn utf16_crlf(text: &str) -> Vec<u16> {
    let normalized = text.replace("\r\n", "\n").replace('\n', "\r\n");
    normalized.encode_utf16().chain(std::iter::once(0)).collect()
}
```

- [x] **Step 2: OS 写入**

```rust
struct ClipboardGuard;
impl Drop for ClipboardGuard { fn drop(&mut self) { unsafe { let _ = CloseClipboard(); } } }

fn open_clipboard_with_retry() -> Result<ClipboardGuard, AppError> {
    for _ in 0..5 {
        if unsafe { OpenClipboard(None) }.is_ok() { return Ok(ClipboardGuard); }
        thread::sleep(Duration::from_millis(20));
    }
    Err(AppError::ClipboardBusy)
}

fn set_global(format: u32, bytes: &[u8]) -> Result<(), AppError> {
    let handle = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes.len()) }.map_err(|e| AppError::ClipboardFailed(e.to_string()))?;
    let ptr = unsafe { GlobalLock(handle) };
    if ptr.is_null() { unsafe { let _ = GlobalFree(Some(handle)); } return Err(AppError::ClipboardFailed("GlobalLock".into())); }
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr as *mut u8, bytes.len()); let _ = GlobalUnlock(handle); }
    // 成功后所有权移交系统，绝不能 GlobalFree
    unsafe { SetClipboardData(format, Some(handle)) }.map_err(|e| AppError::ClipboardFailed(e.to_string()))?;
    Ok(())
}
```

（`windows` 0.62 里 `HGLOBAL` 就是 `HANDLE` 的别名；若实际签名不同按编译器提示调整，不要为此改写结构。）

`copy_image_png`：`image::load_from_memory(png)?.to_rgba8()` → `dib_bytes` → `EmptyClipboard` → `set_global(CF_DIB, &dib)` → `set_global(RegisterClipboardFormatW(w!("PNG")), png)`。

`copy_text`：`utf16_crlf` → 字节切片 → `set_global(CF_UNICODETEXT, ...)`。

- [x] **Step 3: 测试**

纯函数：DIB 前 40 字节字段正确（宽高、bitcount、size）；2×2 纯色图的第 41 字节起的第一个像素为左下角像素的 BGRA；`utf16_crlf("a\nb")` 长度为 6（`a \r \n b \0`）。OS 调用标 `#[ignore]`，手工验收执行。

**Verify:**

```bash
cargo test -p visrecall-shell --test clipboard_bytes
cargo clippy -p visrecall-shell --all-targets -- -D warnings
```

**Commit:** `feat(shell): write dib and unicode text to clipboard`

---

### Task 10: IPC 命令层

**Files:**
- Modify: `src-tauri/src/ipc.rs`
- Modify: `crates/app-core/src/events.rs`
- Modify: `crates/app-core/src/app.rs`（新增 `note/tag/delete/usage` 编排方法）
- Regenerate: `src/lib/bindings.ts`

**Interfaces:**
- Consumes: Task 3–9 的库层/壳层能力
- Produces: 事件层（8 个）+ IPC 骨架 + 详情/备注/标签/复制/删除/清空/用量/保留命令与全部 DTO；`storage_relocate` / `restart_app` / `AppStatus.restart_required`（T18）、`storage_orphans_*`（T19）、`pick_directory`（T14）由所属任务追加

- [ ] **Step 1: 事件**

```rust
pub enum AppEvent {
    // …阶段 01 五个
    ItemNoteUpdated(ItemEvent),
    ItemTagsUpdated(ItemEvent),
    TagsChanged,
    ItemDeleted(ItemEvent),
    ItemsReset,
    ItemsPruned(ItemsPrunedEvent),                    // { count }
    StorageMigrationProgress(MigrationProgressEvent), // { phase, done, total }
    HotkeyRestored,                                    // 托盘 tooltip 复原
}
```

`name()` 分别为 `item_note_updated` / `item_tags_updated` / `tags_changed` / `item_deleted` / `items_reset` / `items_pruned` / `storage_migration_progress` / `hotkey_restored`；`payload()` 对无字段变体返回 `json!({})`。`TauriEventSink` 增加 `HotkeyRestored` 分支：`tray.set_tooltip(Some("目记 VisRecall"))`。`events.rs` 的稳定性测试补全 8 个名字断言。

- [ ] **Step 2: 命令实现**

统一走 `run_blocking`（数据库与文件遍历都是同步 IO）。示例：

```rust
#[tauri::command]
#[specta::specta]
pub async fn note_set(app: tauri::AppHandle, id: String, text: String) -> Result<(), ApiError> {
    let payload_id = id.clone();
    run_blocking(app, move |core| {
        let library = core.library().map_err(ApiError::from)?;
        library.set_note(&id, &text).map_err(|err| ApiError::from(&err))?;
        core.sink().emit(AppEvent::ItemNoteUpdated(ItemEvent { id: payload_id }));
        Ok(())
    }).await
}
```

`copy_item_original` 需要先查素材与 kind，再在 blocking 线程里读文件并调 `shell::clipboard`；错误码由 `AppError` 映射。

`library_purge_preview` 返回 `PurgePreview { item_count, confirmation_word: "删除全部".into() }`；`library_purge(confirm)` 先比对确认词（`INVALID_INPUT`），再清空并发 `items_reset`。

`storage_usage_get` 返回 `StorageUsage`；`retention_preview(days)` 用 `count_items_before` + 过期项的 `sum(文件大小)`；`retention_apply` 返回 `PruneReport`。

- [ ] **Step 3: 注册与绑定**

`collect_commands![...]` 追加全部新命令；用 `VITE_` 无关的方式启动一次 debug 构建刷新绑定：

```bash
cargo test -p visrecall            # committed_bindings_are_up_to_date 会失败并提示
pnpm tauri dev                     # 启动即重写 src/lib/bindings.ts，随后 Ctrl+C
```

- [ ] **Step 4: 测试**

- `api_error_maps_stable_codes` 补 `FILE_MISSING` / `CLIPBOARD_BUSY` / `CLIPBOARD_FAILED` / `BUSY`。
- `event_names_and_payloads_are_stable` 补 8 条。
- `tail`：`ItemDetailDto` 序列化后 `extras.tags[0].auto` 是 JSON 键（不是 `auto_`）。

**Verify:**

```bash
cargo test -p visrecall
cargo clippy --workspace --all-targets -- -D warnings
git diff --stat src/lib/bindings.ts
```

**Commit:** `feat(ipc): expose detail, tags, storage and purge commands`

---

### Task 11: 前端 API 层替换

**Files:**
- Modify: `src/lib/api.ts`
- Modify: `src/lib/events.ts`
- Modify: `src/lib/queryClient.ts`
- Modify: `src/app/App.tsx`（迁移待重启横幅）
- Modify: `src/lib/api.test.ts`、`src/lib/events.test.ts`、`src/app/App.test.tsx`

**Interfaces:**
- Consumes: `commands.*`（Task 10 生成）
- Produces: 无 stub 的 `api`；`subscribeAppEvents()` 按事件分派失效范围

- [ ] **Step 1: 删除 stub**

删除 `UNSUPPORTED_CODE`、`unsupported()`、`isUnsupported()`、`stubDetailExtras`、`PHASE_02`；`loadDetail` 直接 `commands.getItemExt(id)`；`saveNote`/`addTag`/`deleteItem`/`copyOriginal`/`setCapacity`/`setRetention` 直接调命令；`listTags`/`getStorageUsage` 返回真实数据。

新增：`removeTag`、`renameTag`、`deleteTag`、`getPurgePreview`、`purgeLibrary`、`getRetentionPreview`、`applyRetention`、`getOrphans`、`cleanupOrphans`、`relocateStorage`、`subscribeMigration`。

`SUMMARY_PLACEHOLDER` 保留（阶段 03 前唯一的摘要文案）。

- [ ] **Step 2: 查询键**

```ts
export const queryKeys = {
  items: ["items"] as const,
  detail: (id: string) => ["detail", id] as const,
  settings: ["settings"] as const,
  status: ["status"] as const,
  tags: ["tags"] as const,
  usage: ["usage"] as const,
  orphans: ["orphans"] as const,
};
```

- [ ] **Step 3: 事件分派**

```ts
const RESET_ITEMS = ["item_created", "item_updated", "item_deleted", "items_reset", "items_pruned"] as const;

export function subscribeAppEvents(client: QueryClient = queryClient): () => void {
  // item_created/updated → resetQueries(items)
  // item_deleted/items_reset/items_pruned → resetQueries(items) + invalidateQueries(usage)
  // item_note_updated → invalidateQueries(detail(id))，不动列表
  // item_tags_updated → invalidateQueries(detail(id)) + invalidateQueries(tags)
  // tags_changed → invalidateQueries(tags)
  // storage_migration_progress → 交给组件订阅，不在此处理
}
```

保留 `subscribeItemEvents` 作为薄封装（`App.tsx` 已引用），内部改为调用 `subscribeAppEvents` 的列表部分，避免改动 `App.tsx` 的订阅位置。

- [ ] **Step 4: 测试**

`api.test.ts`：每条命令断言调用了对应 `commands.*` 且参数正确（mock `@tauri-apps/api/core` 的 `invoke`）。
`events.test.ts`：用 `vi.mock` 触发各事件名，断言只失效预期键（例如 `item_note_updated` 不得重置 `items`）。

- [ ] **Step 5: 迁移待重启横幅**

`getStatus()` 的类型来自生成的 `AppStatus`（新增 `restartRequired: boolean`）。`App.tsx` 用 `useQuery({ queryKey: queryKeys.status, queryFn: api.getStatus })` 读取，`restartRequired === true` 时在三栏之上渲染常驻横幅：

```text
库已迁移到 <libraryPath>，重启后生效；重启前捕获已暂停。   [立即重启]
```

按钮调 `commands.restartApp()`（期间置 loading 并禁用重复点击）。横幅只在 `restartRequired` 为真时出现，与设置弹窗、详情面板的开关无关；`getStatus` 失败时不渲染横幅、不阻塞列表。

测试：`getStatus` 返回 `restartRequired: true` → 横幅出现、文案含新路径；点击按钮调用 `restartApp` 且只调用一次；`false` → 不渲染。

**Verify:**

```bash
pnpm typecheck && pnpm test && pnpm lint
```

**Commit:** `feat(ui): replace api stubs with real commands`

---

### Task 12: 详情面板接线

**Files:**
- Modify: `src/features/detail/DetailPanel.tsx`
- Modify: `src/features/detail/ImageOverlay.tsx`
- Modify: `src/features/detail/DetailPanel.test.tsx`

**Interfaces:**
- Consumes: `api.loadDetail`（真实）、`api.saveNote`、`api.addTag`、`api.removeTag`、`api.copyOriginal`、`api.deleteItem`
- Produces: 无 stub 的详情交互

- [ ] **Step 1: 删除 `unsupported` 状态**

`SaveState` 改为 `"idle" | "saving" | "saved" | "error"`；`noteBlocked` 删除；失败时 `setSaveState("error")` + Toast，保留文本。

- [ ] **Step 2: 标签 chip 可移除**

chip 内加 `×` 按钮（`aria-label={`移除标签 ${tag.name}`}`），点击调 `api.removeTag(item.id, tag.id)`；成功后靠 `item_tags_updated` 事件刷新，不在本地删数组。chip 设置 `style={{ "--hue": tag.hue ? `var(--hue-${tag.hue})` : undefined }}`。

- [ ] **Step 3: 复制与删除反馈**

`copy()` 成功后 `pushToast("已复制" + copyLabelOf(item.kind))`；失败按 `code` 分支：`FILE_MISSING` → 「原图文件缺失」，`CLIPBOARD_BUSY` → 「剪贴板被占用，请重试」。
`removeItem()` 成功后 `onClose()`，列表由 `item_deleted` 事件刷新；`NOT_FOUND` 时提示「该素材已不存在」并关闭。

- [ ] **Step 4: 原图 overlay**

`ImageOverlay` 的 `src = originalUrl ?? thumbUrl` 回退逻辑保留，但删除「原图待阶段 02 提供」文案：`originalUrl` 非空显示「原图」，为空显示「缩略图」。给 `<img>` 加 `onError` → 切到 `.thumb-fallback` 并显示「原图文件缺失」（协议 404 时不会再走 `originalUrl` 为空的路径）。

- [ ] **Step 5: 测试**

- 备注输入后 300ms 调一次 `saveNote`；连续输入只调最后一次。
- 保存失败时文本仍在、显示「保存失败」。
- 点 chip 的 × 调用 `removeTag(item.id, tag.id)`。
- 复制成功出现「已复制原图」Toast。
- 删除确认后调用 `deleteItem` 且触发 `onClose`。

**Verify:**

```bash
pnpm test -- src/features/detail && pnpm typecheck
```

**Commit:** `feat(ui): wire detail panel to real commands`

---

### Task 13: 侧栏标签云与存储条

**Files:**
- Modify: `src/features/shell/Sidebar.tsx`
- Modify: `src/features/shell/Sidebar.test.tsx`

**Interfaces:**
- Consumes: `api.listTags`、`api.getStorageUsage`、`api.renameTag`、`api.deleteTag`
- Produces: 真实标签云 + 用量条

- [ ] **Step 1: 标签云**

`useQuery({ queryKey: queryKeys.tags, queryFn: api.listTags })`；渲染 `.tag-pill`，显示 `#name` 与数量；空态沿用 `.tag-cloud-empty`。点击**暂不筛选**（阶段 03），`title="阶段 03 支持按标签筛选"` 且不加 `onClick`，避免死按钮语义。

- [ ] **Step 2: 标签治理入口**

hover 时显示省略号按钮 → 小菜单「重命名 / 删除」；重命名用行内输入框，删除用 `ConfirmDialog` 并显示「将影响 N 条素材」。

- [ ] **Step 3: 存储条**

`useQuery({ queryKey: queryKeys.usage, queryFn: api.getStorageUsage, staleTime: 30_000 })`；显示 `已用 X / 容量 Y`，进度条宽度 = `min(used/capacity, 1)`；读取中显示「—」。格式化用 `formatBytes`（在 `src/lib/format.ts` 新增，1024 进制，保留 1 位小数）。

- [ ] **Step 4: 测试**

- 3 个标签渲染 3 个 pill，数量正确。
- 用量 1.2 GiB / 50 GiB 显示 `1.2 GB / 50 GB`（断言文案与宽度百分比）。
- 查询失败时显示「—」而不是 0。
- 重命名提交调用 `renameTag`；删除确认调用 `deleteTag`。

**Verify:**

```bash
pnpm test -- src/features/shell && pnpm typecheck
```

**Commit:** `feat(ui): show tag cloud and storage usage`

---

### Task 14: 设置面板写控件

**Files:**
- Modify: `src/features/settings/SettingsDialog.tsx`
- Modify: `src/features/settings/SettingsDialog.test.tsx`
- Modify: `src-tauri/src/ipc.rs`（新增 `pick_directory` 命令）
- Modify: `src-tauri/Cargo.toml`、`src-tauri/src/main.rs`（注册 `tauri-plugin-dialog`）

**Interfaces:**
- Consumes: `api.getSettings`、`commands.updateSettings`、`api.setCapacity`、`api.setRetention`、`api.getRetentionPreview`、`api.applyRetention`、`api.getPurgePreview`、`api.purgeLibrary`、`api.getOrphans`、`api.cleanupOrphans`、`api.relocateStorage`
- Produces: 所有设置项可写

- [ ] **Step 1: 捕获 pane**

快捷键改为「修改」按钮 → 输入态捕获组合键（`keydown` 拼 `Ctrl/Alt/Shift/Meta + Key`）→ 调 `updateSettings({ captureHotkey })`；`INVALID_INPUT` 时在行内显示后端 message，保持输入态。
悬浮球三档按钮与开关启用，改动立即 `updateSettings`；失败 Toast 并回滚选中态。新增两个开关：贴边收起、全屏隐藏。

- [ ] **Step 2: 存储 pane**

容量输入框（数字，1–4096）失焦提交；保留策略分段控件选天数后先 `getRetentionPreview(days)`，弹确认「将移除 X 条 / 释放 Y」，确认后 `setRetention`；「立即清理」调 `applyRetention` 并展示 `deleted/failed`。
「更换」需要目录选择：**用 Rust 侧 `tauri-plugin-dialog`（`DialogExt::file().pick_folder()`）暴露一条 `pick_directory() -> Option<String>` 命令**，而不是前端直调插件——这样不必新增前端依赖与 capability 权限，路径也保持「只在 Rust 侧流转」的既有约定（与 `open_library_folder` 一致）。选择后调 `relocateStorage(path)`，进度用 `listen("storage_migration_progress")` 驱动进度条，成功后展示「已迁移到 X，重启后生效」。
危险区：折叠的「清空素材库」→ 输入确认词（来自 `getPurgePreview()`）→ `purgeLibrary(word)`。
底部展示孤儿统计与「清理孤儿文件」按钮（二次确认）。危险区说明与数据库提示：

```text
库目录下的 visrecall.db / visrecall.db-wal / visrecall.db-shm 属于同一个数据库，
请勿单独删除或移动；原图与文本位于 originals/ 与 texts/，可直接用系统工具查看。
```

- [ ] **Step 3: 测试**

- 改快捷键成功后调用 `updateSettings` 且显示新键。
- 后端返回 `INVALID_INPUT` 时显示错误文案、不改显示值。
- 容量输入 0 时不发请求、显示范围错误。
- 保留天数选择后先出现确认对话框，取消则不调 `setRetention`。
- 确认词错误时提交按钮保持禁用（前端先校验）。

**Verify:**

```bash
pnpm test -- src/features/settings && pnpm typecheck && pnpm lint
```

**Commit:** `feat(ui): enable settings writes and storage maintenance`

---

### Task 15: 悬浮球三态与尺寸热更新

**Files:**
- Create: `crates/shell/src/ball/state.rs`
- Modify: `crates/shell/src/ball.rs`（改为 `ball/mod.rs` + `ball/state.rs`）
- Create: `crates/shell/tests/ball_state.rs`
- Modify: `crates/shell/src/lib.rs`

**Interfaces:**
- Consumes: 既有分层窗口绘制、位置记忆
- Produces: `BallSettings { size, edge_hide, hide_in_fullscreen, positions }`、`BallStateMachine::{new, handle}`、`BallAction`、`BallHandle::set_size`、`BallHandle::apply_settings`

- [ ] **Step 1: 纯状态机**

按 `design.md` §6.10 实现。`BallAction`：

```rust
pub enum BallAction {
    Show { rect: PhysicalRect },
    Hide,
    SetHitArea { rect: PhysicalRect },
    Resize { size: u32, center: PhysicalPoint },
    ScheduleTimer { after_ms: u64 },
}
```

`handle` 只产出动作，不碰窗口；所有时间来自 `now_ms`，可在测试里跳时间。

- [ ] **Step 2: 窗口接线**

`WM_BALL_SET_SIZE`：重建位图缓存 → `SetWindowPos` 保持中心点（`SWP_NOSENDCHANGING | SWP_NOACTIVATE`）→ 夹回工作区。
`WM_TIMER`：按 id 分流（收起 / 展开 / 回缩 / 全屏检测）。全屏检测仅在 `BallVisual::Ball | Strip` 时运行；连击 3 次全屏才隐藏，退出立即恢复。
`WM_MOUSELEAVE`：`TrackMouseEvent` 后投递 `CursorLeave`。
长条形态命中区：窗口按「长条 5px + 两侧各 6px 透明留白」创建，绘制内缩到留白内，命中由窗口矩形自然覆盖；不使用 `SetWindowRgn`。

- [ ] **Step 3: 配置**

`config.rs` 的 `BallConfig` 增加 `edge_hide: bool`（默认 true）、`hide_in_fullscreen: bool`（默认 true）、`guide_shown: bool`（默认 false）；`DEFAULT_BALL_SIZE` 保持 48，新增 `BALL_SIZE_OPTIONS: [u32; 3] = [40, 48, 56]`，`update_settings` 只接受这三档（不再是 16..=256）。

- [ ] **Step 4: 测试**

- 距边缘 12px 松手 + 800ms → `Strip`；13px → 保持 `Ball`。
- `Strip` 下进入热区 150ms → `Ball`；离开 600ms → `Strip`。
- 全屏 3 次 tick → `Hidden`；退出全屏 → 恢复 `Strip`（若仍贴边）。
- `edge_hide=false` 时永不收起。
- 尺寸从 56 → 40 时 `Resize` 的中心点等于原中心，且新矩形在 work area 内。

**Verify:**

```bash
cargo test -p visrecall-shell --test ball_state
cargo test -p visrecall-shell          # 既有 ball_position 测试不得回归
```

**Commit:** `feat(shell): add ball tri-state and live resize`

---

### Task 16: 设置热生效（快捷键重载）

**Files:**
- Modify: `crates/app-core/src/app.rs`
- Create: `crates/app-core/tests/settings_hot_reload.rs`
- Modify: `src-tauri/src/ipc.rs`（`SettingsPatch` 扩展）

**Interfaces:**
- Consumes: `spawn_hotkey_thread`、`HotkeyHandle`、`CaptureRuntime.shell_tx`
- Produces: `AppCore::spawn_capture_hotkey(spec)`（私有，抽出既有通道选择逻辑）、`AppCore::update_settings(patch: SettingsPatchInput)`

- [ ] **Step 1: 抽出热键注册helper**

把 `start_hotkey` 中「`runtime.shell_tx` 或临时通道 + fallback 消费线程 + `spawn_hotkey_thread`」这段抽成私有方法，行为不变：

```rust
fn spawn_capture_hotkey(&self, spec: HotkeySpec) -> Result<HotkeyHandle, AppError>;
```

`start_hotkey` 改为「已存在则返回 → parse 配置 → `self.spawn_capture_hotkey(spec)?` → 存入 `hotkey`」。既有 `start_hotkey` 测试必须全绿（通道选择、fallback 降级都已被覆盖）。

- [ ] **Step 2: 按 design §6.11 的顺序重写 `update_settings`**

```rust
pub fn update_settings(&self, patch: SettingsPatchInput) -> Result<AppConfig, AppError> {
    // 1. 校验（尺寸三档、快捷键 parse）→ 不落盘
    // 2. 若换键：先 spawn 新线程（失败 → InvalidInput，旧句柄不动）
    // 3. save_atomic（失败 → 停新线程 → 返回 IO）
    // 4. 更新内存 config
    // 5. hotkey.take() 停旧线程，存新句柄
    // 6. 球：apply_ball_visibility / set_size / apply_settings
    // 7. 若第 5 步发生，发 AppEvent::HotkeyRestored（清托盘冲突提示）
}
```

`SettingsPatchInput` 用结构体而不是 5 个 `Option` 参数（现有 3 参数签名调用点只有 `ipc.rs`，一并改）。

- [ ] **Step 3: 测试**

- 传入非法快捷键字符串 → `InvalidInput`，config 文件 mtime 不变。
- 传入与当前相同的键 → 不重新注册：先用 `spawn_hotkey_thread` 占用当前组合，再 `update_settings` 传同值，应返回 `Ok`（内部若重新注册必然失败，此用例即回归保护）。
- 占用路径：测试内 `spawn_hotkey_thread` 占用目标组合 → `update_settings` 返回 `InvalidInput`、配置未落盘、`hotkey().spec()` 仍是旧值。
- 尺寸 41 → `InvalidInput`；40/48/56 各自成功并调用 `set_size`。
- 改键成功（用未被占用的组合）→ 发出 `HotkeyRestored`，且 `hotkey().spec()` 为新值。
- 落盘失败（把 config 目录设为只读或用只读文件）→ 返回 `Io` 且内存值未变。

**Verify:**

```bash
cargo test -p visrecall-app-core --test settings_hot_reload
cargo test -p visrecall-app-core
```

**Commit:** `feat(app-core): hot reload hotkey and ball settings`

---

### Task 17: 容量门禁与保留清理

**Files:**
- Modify: `crates/app-core/src/capture_flow.rs`（容量门禁）
- Create: `crates/app-core/src/storage_flow.rs`
- Create: `crates/app-core/tests/retention_flow.rs`
- Modify: `crates/app-core/src/app.rs`（启动时清理）

**Interfaces:**
- Consumes: `Library::usage` / `capacity_bytes` / `list_expired` / `delete_item`、`EventSink`
- Produces: `UsageProbe` 注入点；`storage_flow::prune_expired(library, sink) -> PruneReport`；`storage_flow::check_quota(library) -> Result<(), AppError>`

- [ ] **Step 1: 容量门禁**

在 `check_free_space` 之后加：

```rust
if let Some(used) = (self.usage_probe)(library) {
    if used >= library.capacity_bytes() {
        return Err(AppError::StorageFull);
    }
}
```

`usage_probe` 默认实现 `|lib| lib.usage().ok().map(|u| u.used_bytes)`；测试注入固定值。注意：容量门禁在**抓帧前**执行，失败路径与现有 `StorageFull` 一致（托盘 tooltip + 事件）。

- [ ] **Step 2: 保留清理**

```rust
pub fn prune_expired(library: &Library, sink: &dyn EventSink) -> PruneReport {
    let Some(days) = library.retention().days else { return PruneReport::default() };
    let cutoff = now_millis() - i64::from(days) * 86_400_000;
    let ids = library.list_expired(cutoff, 500).unwrap_or_default().into_iter().map(|i| i.id);
    // 逐条 delete_item；成功计数，失败计数并 warn
    // 只有 count > 0 时才发 items_pruned
}
```

- [ ] **Step 3: 启动清理**

`AppCore::start_runtime` 成功后，`spawn_blocking` 调 `prune_expired`；失败只 `warn`。用 `Mutex<()>` 与迁移闸门互斥（`Busy`）。

- [ ] **Step 4: 测试**

- `retention = None` 时 `prune_expired` 删除 0 条（关键防呆：默认值不得清库）。
- 3 条过期 + 2 条未过期 → 删 3，事件 `items_pruned { count: 3 }` 只发一次。
- 单条删除失败（把某原图设为只读并使数据库只读无法模拟时，用删除不存在的行模拟）→ `failed = 1`，其余继续。
- 用量超过容量 → 捕获返回 `StorageFull`；用量等于容量 → 同样拒绝（`>=`）。

**Verify:**

```bash
cargo test -p visrecall-app-core --test retention_flow
cargo test -p visrecall-app-core --test capture_flow
```

**Commit:** `feat(app-core): enforce quota and prune expired items`

---

### Task 18: 存储位置迁移

**Files:**
- Create: `crates/library/src/relocate.rs`
- Create: `crates/library/tests/relocate_fs.rs`
- Create: `crates/app-core/tests/relocate_flow.rs`
- Modify: `crates/app-core/src/app.rs`（`stop_runtime`、`CaptureGate`、`relocate_library`）
- Modify: `crates/app-core/src/storage_flow.rs`（迁移编排）
- Modify: `crates/app-core/src/capture_flow.rs`（闸门检查 + `BUSY` 错误码）
- Modify: `src-tauri/src/ipc.rs`（`storage_relocate`、`restart_app`、`AppStatus.restartRequired`）

**Interfaces:**
- Consumes: `Library::root/checkpoint/usage`、`files::free_space_bytes`、`Config::save_atomic`
- Produces:
  - `relocate::copy_library(src, dst, progress) -> Result<RelocateReport, AppError>`
  - `relocate::validate_target(src, dst) -> Result<(), AppError>`
  - `relocate::verify(dst, expected_items, expected_library_id) -> Result<(), AppError>`
  - `AppCore::relocate_library(path) -> Result<RelocateResult, AppError>`
  - `AppCore::stop_runtime()`
  - `AppCore::gate() -> CaptureGate`、`AppCore::set_gate(CaptureGate)`

- [ ] **Step 1: 目标校验（组件级比较，不用字符串前缀）**

```rust
fn is_within(child: &Path, parent: &Path) -> bool {
    let (child, parent) = (normalize(child), normalize(parent));
    child.components().zip(parent.components()).take(parent.components().count()).all(|(a, b)| a == b)
        && child.components().count() >= parent.components().count()
}
```

拒绝条件：非绝对路径；`is_within(dst, src) || is_within(src, dst)`；`dst` 存在且非空；`free_space_bytes(dst) < used * 1.1`。

- [ ] **Step 2: 复制**

```rust
const COPY_ORDER: [&str; 5] = ["library.json", "visrecall.db", "originals", "texts", "thumbs"];

pub fn copy_library(src: &Path, dst: &Path, progress: &dyn Fn(&str, u64, u64)) -> Result<RelocateReport, AppError> {
    let files = collect_copy_plan(src)?;          // 先算 total，进度才准
    for (idx, rel) in files.iter().enumerate() {
        let target = dst.join(rel);
        if target.exists() && fs::metadata(&target)?.len() == fs::metadata(src.join(rel))?.len() {
            progress("copy", idx as u64 + 1, files.len() as u64);
            continue;                              // 天然续传
        }
        copy_file_atomic(&src.join(rel), &target)?; // 复制到 .tmp 再 rename
        if idx % 64 == 0 { progress("copy", idx as u64 + 1, files.len() as u64); }
    }
    Ok(RelocateReport { files_copied: … })
}
```

`visrecall.db-wal` / `-shm` 不复制（已 checkpoint）。

**连接纪律（硬约束）**：`checkpoint(TRUNCATE)` 之前不得打开任何新的 `Connection`——校验、用量、行数对账都在复制完成后只对**目标库**打开；源库的行数与 `library_id` 用现有写连接读出并作为参数传入 `verify`（复用连接，不新开）。

原因：TRUNCATE 需要所有读者处于同一快照。多一个持快照的连接，checkpoint 会退化为部分模式（`busy` 非 0），此时复制出的 `visrecall.db` 仍缺最近的帧——而 `integrity_check` 照样返回 `ok`，只有数据少了一截。

```rust
library.checkpoint()?;   // T6 内部判定 busy != 0 → Busy；此处失败即中止迁移，闸门回 Idle
```

这里不重复解析 PRAGMA 返回值：判定只有一处（`checkpoint()`），迁移编排只负责「失败就中止、不进复制」。

- [ ] **Step 3: 校验**

```rust
pub fn verify(dst: &Path, expected_items: u64, expected_id: &str) -> Result<(), AppError> {
    let conn = Connection::open_with_flags(dst.join(DB_FILE), OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let check: String = conn.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    if check != "ok" { return Err(AppError::Io(format!("integrity check failed: {check}"))); }
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM items", [], |r| r.get(0))?;
    if count as u64 != expected_items { return Err(AppError::Io(format!("item count {count} != {expected_items}"))); }
    // library_id 比对
}
```

- [ ] **Step 4: 编排与捕获闸门**

```rust
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CaptureGate { Idle, Relocating, RestartPending }   // AtomicU8 存储
```

顺序严格按 `design.md` §6.9：闸门 `Idle → Relocating`（`compare_exchange`，失败 `Busy`）→ 校验目标 → `stop_runtime()` → `checkpoint()` → 复制 → 校验 → 写 config。

两条出口的差异是本任务的核心：

```rust
// 成功
config.library_path = dst;
config.save_atomic(&self.paths.config_file)?;
self.set_gate(CaptureGate::RestartPending);   // 关键：不回 Idle
self.start_runtime()?;                         // 球/热键恢复，但捕获被闸门拒绝
self.start_hotkey()?;
sink.emit(StorageMigrationProgress { phase: "done", .. });
Ok(RelocateResult { restart_required: true, new_root: dst })

// 失败
self.set_gate(CaptureGate::Idle);
self.start_runtime().ok();
self.start_hotkey().ok();
Err(err)                                       // config 未改，目标残留保留
```

`CaptureGate::RestartPending` 时 `library_root()` 返回 `config.library_path`（新路径），否则「打开库目录」会打开旧目录。

`capture_flow.rs` / `app.rs` 两处补闸门，**主检查在捕获循环的事件分发处**（覆盖 `CaptureRequest` 与 `Drop` 两条写入口）：

```rust
// app.rs::spawn_capture_loop：分支最前面，CaptureRequest 与 Drop 都要挡
fn gate_message(gate: u8) -> Option<&'static str> {
    match CaptureGate::from_u8(gate) {
        CaptureGate::Idle => None,
        CaptureGate::Relocating => Some("库正在迁移，捕获已暂停"),
        CaptureGate::RestartPending => Some("库已迁移，请重启后再捕获"),
    }
}

// capture_flow.rs::preflight 里再查一次（纵深防御，防新入口绕过循环）
match self.gate() {
    CaptureGate::Idle => {}
    other => return Err(AppError::Busy(gate_message(other as u8).unwrap_or("busy").into())),
}
// 错误码映射（原 match 在 capture_flow.rs 的 capture_failed 构造处）
AppError::Busy(_) => "BUSY",
```

为什么不能只查 `preflight`：`ShellEvent::Drop` 由独立导入线程（`ImportFlow`）处理，不经过 `CaptureCoordinator::preflight`；只查一处会让「迁移成功后往悬浮球拖一张图」仍然写进旧库。

`restart_app` 命令直接 `app.restart()`（Tauri 2 `AppHandle::restart`）；`get_app_status` 增加 `restart_required: self.gate() == RestartPending`。

- [ ] **Step 5: 测试**

- `validate_target`：非绝对路径 / 目标在源内 / 源在目标内 / 目标非空 / 空间不足 全部拒绝；`C:\lib` 与 `C:\lib2` 视为不同（组件比较）。
- `copy_library` 两次执行：第二次全部跳过，文件数一致。
- `verify` 失败：改一行 `items` 数量后注入期望值不匹配 → 报错。
- **checkpoint 被读者阻塞**：在 `checkpoint()` 之前另开一个 `Connection` 并开始一个读事务 → `PRAGMA wal_checkpoint(TRUNCATE)` 返回 `busy != 0` → 迁移返回 `Busy`、目标目录无文件、`config.json` 未变、闸门回 `Idle`。
- **只复制主库是错的**：构造「有数据但未 checkpoint」的库，只复制 `visrecall.db` 到目标后 `verify` 必须失败（`no such table: items` 或行数不符），证明行数校验能拦住这类错误。
- **失败出口**：目标目录在复制中途设为只读 → `config.json` 内容未变、`gate() == Idle`、`start_runtime` + `start_hotkey` 已恢复（用 `ball`/`hotkey` 句柄是否 `Some` 断言）。
- **成功出口**：`config` 路径变更、返回 `restart_required = true`、`gate() == RestartPending`、源目录文件仍在、`get_app_status().restart_required == true`。
- **成功后捕获被拒**：闸门为 `RestartPending` 时调 `CaptureCoordinator::preflight`（或直接调捕获入口）返回 `AppError::Busy`，且 `list_items` 条数不变；`capture_failed` 事件的 `code == "BUSY"`。
- **成功后拖放被拒**：向捕获循环投递 `ShellEvent::Drop`，确认不产生素材、旧库条数不变、事件码为 `BUSY`（防止只查 `preflight` 的漏洞）。
- **重启语义**：用同一 `config` 重新构造 `AppCore`（模拟重启）→ `gate() == Idle`、`restart_required == false`、库根目录为新路径。
- `library_root()` 在 `RestartPending` 时等于新路径，`Idle` 时等于已打开库的根目录。

**Verify:**

```bash
cargo test -p visrecall-library --test relocate_fs
cargo test -p visrecall-app-core --test relocate_flow
```

**Commit:** `feat(library): relocate library with verify and restart`

---

### Task 19: 孤儿清理与恢复补强

**Files:**
- Modify: `crates/app-core/src/storage_flow.rs`
- Modify: `crates/library/src/cleanup.rs`（把 `collect_files` 提为 `pub(crate)`，增加 `missing_thumbs`）
- Modify: `src-tauri/src/ipc.rs`（两条命令已在 Task 10 注册，这里补齐行为）

**Interfaces:**
- Consumes: `Library::orphans/cleanup_orphans`
- Produces: `storage_orphans_get` / `storage_orphans_cleanup` 完整行为

- [ ] **Step 1: 计数与清理**

`orphans()` 已实现（Task 6）；这里补「清理后发 `items_reset`? 不需要」——孤儿文件不属于任何素材，清理后不需要刷新列表，但**用量会变**，因此发 `items_pruned { count: 0 }` 不合适；直接让前端在命令返回后 `invalidateQueries(usage)`。

- [ ] **Step 2: 启动日志**

`Library::open` 的启动清理日志增加 `missing_thumbs` 字段，便于用户报障时对账。

- [ ] **Step 3: 测试**

- 孤儿清理只删未引用文件，已引用文件保留。
- 清理返回实际删除数（含「文件已被手动删除」时计为成功）。
- 缩略图缺失计数正确。
- **扫描范围白名单**：在库根放 `junk.bin`、在 `backups/` 放一个文件、保留 `tmp/` 里的残留文件，执行清理后三者都仍在——它们不在 `originals/`、`texts/` 之下，不属于孤儿清理的职责。

**Verify:**

```bash
cargo test -p visrecall-library --test storage_db
cargo test -p visrecall-app-core
```

**Commit:** `feat(storage): expose orphan cleanup from settings`

---

### Task 20: Release 资源加载与构建收口

**Files:**
- Modify: `src-tauri/Cargo.toml`
- Modify: `src-tauri/tauri.conf.json`
- Modify: `README`/`design.md` 若配置有偏差

**Interfaces:**
- Consumes: `dist/` 构建产物
- Produces: 不依赖 `devUrl` 的 release 二进制

- [ ] **Step 1: 启用 custom-protocol**

```toml
# src-tauri/Cargo.toml
[features]
default = ["custom-protocol"]
custom-protocol = ["tauri/custom-protocol"]
```

`tauri.conf.json` 确认 `build.frontendDist = "../dist"`、`devUrl` 仅用于开发。

- [ ] **Step 2: 构建**

```bash
pnpm build
cargo build --release -p visrecall
```

- [ ] **Step 3: 验收**

关闭 `pnpm dev`，直接运行 `target/release/visrecall.exe`：主窗口能打开、列表可读、缩略图显示、捕获可用、设置可开。记录启动耗时与内存快照（供 verification）。

- [ ] **Step 4: 记录**

在 `verification.md` §2 记录二进制路径、构建时间、验收结果；不产出安装包（D2-02）。

**Verify:**

```bash
cargo build --release -p visrecall
./target/release/visrecall.exe   # 手工验收
```

**Commit:** `build(tauri): embed frontend assets for release`

---

### Task 21: 门禁回填与阶段验收

**Files:**
- Create: `docs/specs/v0.1/02-library-detail/verification.md`（若尚未建，见配套文件）
- Modify: `docs/specs/v0.1/01-capture-loop/verification.md`（回填）
- Modify: `docs/specs/v0.1/README.md`（阶段状态、门禁结论）

**Interfaces:**
- Consumes: Task 1–20 全部产物
- Produces: 可审计的验收记录

- [ ] **Step 1: 跑完整自动化门禁**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
pnpm lint && pnpm typecheck && pnpm test && pnpm build
```

- [ ] **Step 2: 阶段 01 遗留项逐项补测**

按 `verification.md` §1 清单执行：4K + PresentMon（无硬件则记「环境限制未验证」）、双屏与混合 DPI、单屏 100%/125%/200%、OLE 四来源拖放、DRM 与独占全屏、提权窗口、托盘内存与空闲 CPU、20 条滚动与翻页（原 100 条口径按现行规模下调，证据注明规模）、30 分钟 / ≥20 次捕获、重复捕获不去重、release 产物。

- [ ] **Step 3: 阶段 02 验收**

按 `verification.md` §2–§4 执行：详情/备注/标签/复制/删除/清空/用量/保留/迁移/球三态/热生效/恢复/构建。

- [ ] **Step 4: 回填文档**

- `01-capture-loop/verification.md`：勾选或标注环境限制，附日期与证据（截图路径、日志片段、命令输出）。
- `02-library-detail/verification.md`：填写全部结果与遗留问题。
- `README.md`：阶段 02 状态改为「已完成（日期）」；§1 门禁清单每项标注结论；§6 DoD 更新（release 已可运行、安装包仍在阶段 04）。

**Verify:**

```bash
git diff --stat docs/
grep -n "\[ \]" docs/specs/v0.1/01-capture-loop/verification.md
```

**Commit:** `docs(spec): record phase 02 verification results`
