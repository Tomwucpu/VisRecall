# UI 前置阶段 · 前后端接口契约草案

> 状态：**已被 `02-library-detail/design.md` §5 取代（2026-11-06）**。实现以阶段 02 spec 为准；本文与实现的差异清单见 `02-library-detail/design.md` §5.4（保留本文作为 UI 前置阶段的历史产物与 stub 设计依据）。
> 用途：重写 `02-library-detail` spec 时，`design.md` 的接口章节直接引用本文，避免二次设计。
> 口径：本文描述的是**目标形态**。当前实现里凡标 `STUB` 的，都在 `src/lib/api.ts` 有同签名占位，
> 换实现时组件零改动。类型以 TS 表述，并标注它在 `visrecall_domain` 里的对应方向。

## 1. 现状基线（阶段 01 已存在，无需新增）

| 命令 | 返回 | 前端出口 | 备注 |
|---|---|---|---|
| `get_app_status` | `AppStatus` | `api.getStatus()` | 只读内存，不碰磁盘 |
| `list_items(cursor, limit)` | `ItemDto[]` | `api.listItems()` | 游标为 `capturedAt` 严格小于；`limit` clamp 1..=100 |
| `get_item(id)` | `ItemDto` | `api.loadDetail()` 的基础部分 | 与列表同构，含 `note` |
| `open_library_folder` | `null` | `api.openLibraryFolder()` | 路径只从 `AppCore` 取，前端不能传 |
| `get_settings` | `SettingsDto` | `api.getSettings()` | 只读 |
| `update_settings(patch)` | `SettingsDto` | —— | 见 §5 的三条限制 |

事件：`item_created` / `item_updated` 已用于列表失效（`subscribeItemEvents`，重置分页而非逐页失效）。

## 2. 类型

```ts
/** 已有。`thumb_url` 只在存在缩略图时由后端拼接。 */
type ItemDto = {
  id: string;
  kind: "image" | "text" | "link";
  status: "parsing" | "ready" | "pending" | "failed";
  capturedAt: number;           // Unix ms
  sourceApp: string | null;
  sourceTitle: string | null;
  sourceUrl: string | null;
  width: number | null;
  height: number | null;
  charCount: number | null;
  note: string;                 // 阶段 02 之前恒为空串（无写入口）
  thumbUrl: string | null;      // visrecall://thumb/<id>
};

/** 新增：详情增强字段。整块按 id 读取，缺能力时返回 null / []，界面显示占位。 */
type DetailExtras = {
  summary: string | null;       // 阶段 03；SUMMARY_UNAVAILABLE 时同样为 null
  bodyText: string | null;      // 文本原文，截断到前 20_000 字符
  originalUrl: string | null;   // visrecall://original/<id>
  tags: TagChip[];
};

/** 新增。`auto` 决定 chip 「·」角标与淡色底；`hue` 为调色板令牌名。 */
type TagChip = {
  id: string;
  name: string;
  auto: boolean;                // 自动标注 vs 手动
  hue: string | null;           // 如 "ochre" / "slate"，null 表示中性
};

/** 新增。`null` 表示暂时读不到，界面显示「—」而不是 0。 */
type StorageUsage = {
  usedBytes: number;
  capacityBytes: number;
  itemCount: number;
};
```

领域方向：`TagChip` → `visrecall_domain::Tag`（需新增 `TagSource::{Auto, Manual}`）；
`StorageUsage` → `visrecall_domain::StorageUsage`；`DetailExtras` 是聚合读模型，不落 domain 实体。

## 3. 命令草案

### 3.1 详情读取

```ts
// 建议：扩展现有 get_item，而不是新增 item_detail —— 避免两条读路径不一致。
get_item_ext(id: string): Promise<{ item: ItemDto; extras: DetailExtras }>;
// 错误：NOT_FOUND / IO / DATABASE
```

**原图协议**：`visrecall://original/<id>` 与既有 `visrecall://thumb/<id>` 同构，处理器只服务
`originals/` 目录；文件缺失返回 `FILE_MISSING`（而不是 404 空白）。

### 3.2 备注

```ts
note_get(itemId: string): Promise<string>;                 // 阶段 02
note_set(itemId: string, text: string): Promise<void>;     // 阶段 02
// 规则：text.length <= 10_000，超限 INVALID_INPUT（前端已阻止，后端仍需校验）
// 事件：item_note_updated { id: string }  → 列表/详情按 id 局部失效
```

前端交互已实现（`DetailPanel`）：300ms 防抖、`Esc` 立即落盘并失焦、超限截断阻止输入。
当前 `api.saveNote` 抛 `UNSUPPORTED`，界面把保存指示器切成「待阶段 02 提供 · 仅本地草稿」。

### 3.3 标签

```ts
tags_list(): Promise<TagChip[]>;                                  // 侧栏标签云
tag_add(itemId: string, name: string): Promise<TagChip>;          // 同名复用已有标签
tag_rename(tagId: string, name: string): Promise<void>;
tag_delete(tagId: string): Promise<void>;
tag_detach(itemId: string, tagId: string): Promise<void>;
// 事件：item_tags_updated { id }（单条）、tags_changed（标签集合本身变化）
// 语义：tag_suppressions 表按 (item, tag) 记录「用户删掉过自动标签」，
//       再次解析时不得重新贴回；手动标签不进 suppressions。
```

### 3.4 原始内容复制

```ts
copy_item_original(itemId: string): Promise<void>;   // 按 kind 分支：图片 CF_DIB+PNG / 文本与链接 CF_UNICODETEXT
// 错误：CLIPBOARD_BUSY（剪贴板被占用，可重试）、CLIPBOARD_FAILED、FILE_MISSING
```

单一命令优于三条，避免前端再按 kind 分派；`kind` 的判定留在 Rust 侧与存储一致。

### 3.5 删除

```ts
item_delete(itemId: string): Promise<void>;      // 彻底删除：原图 + 缩略图 + 行 + 索引
library_purge(): Promise<void>;                  // 全库清空，需二次确认文案来自后端
// 事件：item_deleted { id }（全库清空则 items_reset）
```

### 3.6 存储

```ts
storage_usage_get(): Promise<StorageUsage | null>;
storage_capacity_set(gigabytes: number): Promise<void>;   // 1..=4096
retention_set(days: number | null): Promise<void>;        // null = 永久
storage_relocate(path: string): Promise<void>;            // 迁移，进度走事件
// 事件：storage_migration_progress { done: number; total: number; phase: string }
// 错误：STORAGE_FULL / IO / INVALID_INPUT
```

## 4. 设置（复用既有 `update_settings`，但有三条硬限制）

阶段 01 的 `update_settings` 现状（`crates/app-core/src/app.rs`）：

| 字段 | 现状 | 结论 |
|---|---|---|
| `ball_enabled` | 立即生效（`apply_ball_visibility`） | 阶段 02 可直接接管开关 |
| `ball_size` | 落盘但**需重启**才重建窗口 | 阶段 02 需补「应用即生效」 |
| `capture_hotkey` | 与当前值不同即整体拒绝（`restart required`） | 阶段 02 需实现热键热重载 |

因此设置面板本轮不接写入：快捷键与悬浮球只读展示真实配置（`get_settings`），
禁用控件 + `title` 说明原因，避免「点了没反应」。阶段 02 补完上述三项后，
把 `SettingsDialog` 的 `disabled` 与 `title` 就地替换成写调用即可。

## 5. 错误码

已有：`CONFIG_MISSING` / `INVALID_INPUT` / `NOT_FOUND` / `STORAGE_FULL` / `CAPTURE_FAILED` /
`CAPTURE_CANCELLED` / `DRM_PROTECTED` / `IO` / `DATABASE` / `INTERNAL`。

新增：

| 码 | 含义 | 前端处理 |
|---|---|---|
| `UNSUPPORTED` | 能力尚未接入（**仅 stub 使用，实现落地后应消失**） | Toast 原样显示 `message` |
| `FILE_MISSING` | 原图/缩略图文件不在库目录 | Toast + 卡片/详情退回占位 |
| `CLIPBOARD_BUSY` | 剪贴板被其他进程占用 | 允许重试的 Toast |
| `CLIPBOARD_FAILED` | 写剪贴板失败 | Toast |
| `SUMMARY_UNAVAILABLE` | AI 描述不可用（无 Key / 离线） | 详情显示「待解析」，复制保持禁用 |

## 6. 前端接入点映射

| 组件 | 现在 | 阶段 02 换成 |
|---|---|---|
| `DetailPanel` | `api.loadDetail` = `get_item` + stub extras | `get_item_ext`，删掉 stub |
| `DetailPanel` 备注 | `api.saveNote`（抛 UNSUPPORTED） | `note_set`，接 `item_note_updated` 局部失效 |
| `DetailPanel` 标签 | `api.addTag`（抛 UNSUPPORTED） | `tag_add`，成功后 `item_tags_updated` |
| `DetailPanel` 复制 | `api.copyOriginal`（抛 UNSUPPORTED） | `copy_item_original` |
| `DetailPanel` 删除 | `api.deleteItem`（抛 UNSUPPORTED） | `item_delete`，接 `item_deleted` |
| `Sidebar` 标签云 | `api.listTags()` 返回 `[]` | `tags_list`，接 `tags_changed` |
| `Sidebar` 存储条 | 静态「已用 —」 | `storage_usage_get` |
| `SettingsDialog` | 只读展示 + 禁用控件 | `update_settings` 三个字段 + `storage_*` |

替换方式：`src/lib/api.ts` 内逐个把 stub 换成 `commands.*`，**组件不感知**；
`UNSUPPORTED` 分支随之删除，`api.test.ts` 的对应用例改为真实命令的契约测试。

## 7. 仍需在新 spec 里决策

1. `get_item_ext` 的 `originalUrl` 对文本/链接是否有意义（还是只在图片上返回）。
2. 标签 `hue` 的来源：由后端按名字散列定型（跨设备一致），还是前端取色。
3. `library_purge` 是否需要「输入确认词」而非单纯对话框（不可恢复操作的强度取舍）。
4. 存储迁移期间的读写降级策略（迁移中是否只读、断点续传如何标记）。
