import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { QueryClient } from "@tanstack/react-query";
import type { ImportTaskDto, ImportTaskEntryDto } from "./api";
import { queryClient, queryKeys } from "./queryClient";
import { fetchItemCard, patchItemEverywhere, removeItemsEverywhere } from "./itemCache";
import type { ItemCardData } from "../features/library/ItemCard";

/**
 * 事件 → 失效范围的唯一映射（design §5.3）。
 *
 * 关键点：`item_note_updated` 只失效该条详情，**不动列表**——备注保存是高频动作，
 * 整表重置会让用户正在浏览的列表跳动；标签变化则要同时刷新标签云。
 */
type EventName =
  | "item_created"
  | "item_updated"
  | "item_deleted"
  | "item_renamed"
  | "items_deleted"
  | "items_reset"
  | "items_pruned"
  | "item_note_updated"
  | "item_content_updated"
  | "item_tags_updated"
  | "tags_changed"
  | "hotkey_restored"
  | "storage_migration_progress"
  // 阶段 03：索引管线与语义通道事件
  | "item_indexed"
  | "item_enriched"
  | "pipeline_status"
  | "semantic_status"
  | "import_task_progress"
  | "import_task_finished";

/** 需要重置列表的事件（分页状态一并丢弃）。 */
const LIST_EVENTS = [
  "item_created",
  "item_updated",
  "item_deleted",
  "items_reset",
  "items_pruned",
] as const;

/** 会改变用量的列表事件。 */
const USAGE_EVENTS = ["item_created", "item_deleted", "items_reset", "items_pruned"] as const;

/** 会改变孤儿文件统计的事件：删素材只删库内引用，磁盘上可能留下文件。 */
const ORPHAN_EVENTS = ["item_deleted", "items_reset", "items_pruned"] as const;

const ALL_EVENTS: EventName[] = [
  "item_created",
  "item_updated",
  "item_deleted",
  "item_renamed",
  "items_deleted",
  "items_reset",
  "items_pruned",
  "item_note_updated",
  "item_content_updated",
  "item_tags_updated",
  "tags_changed",
  "hotkey_restored",
  "storage_migration_progress",
  "item_indexed",
  "item_enriched",
  "pipeline_status",
  "semantic_status",
  "import_task_progress",
  "import_task_finished",
];

function contentFieldOf(payload: unknown): string | null {
  if (typeof payload === "object" && payload !== null) {
    const field = (payload as { field?: unknown }).field;
    if (typeof field === "string") {
      return field;
    }
  }
  return null;
}

function itemIdOf(payload: unknown): string | null {
  if (typeof payload === "object" && payload !== null) {
    const id = (payload as { id?: unknown }).id;
    if (typeof id === "string") {
      return id;
    }
  }
  return null;
}

/**
 * 终态事件里的任务快照与失败明细。
 *
 * 导入任务一进终态就被删除（库里不留导入历史），所以 `import_task_finished`
 * 是拿到最终计数与失败原因的唯一来源，不能再靠回查任务。
 */
function finishedTaskOf(
  payload: unknown,
): { task: ImportTaskDto; failures: ImportTaskEntryDto[] } | null {
  if (typeof payload !== "object" || payload === null) {
    return null;
  }
  const task = (payload as { task?: unknown }).task;
  if (typeof task !== "object" || task === null) {
    return null;
  }
  const failures = (payload as { failures?: unknown }).failures;
  return {
    task: task as ImportTaskDto,
    failures: Array.isArray(failures) ? (failures as ImportTaskEntryDto[]) : [],
  };
}

function phaseOf(payload: unknown): string | null {
  if (typeof payload === "object" && payload !== null) {
    const phase = (payload as { phase?: unknown }).phase;
    if (typeof phase === "string") {
      return phase;
    }
  }
  return null;
}

/** 单条素材读取；测试可注入假实现，生产默认走 IPC。 */
export type ItemCardFetcher = (id: string) => Promise<ItemCardData | null>;

/**
 * 订阅全部后端事件并按事件类型分派失效范围，返回清理函数。
 *
 * 列表查询一律用 `invalidateQueries` 而不是 `resetQueries`：重置会把 `data` 清成
 * `undefined`，触发 `isPending`，让整个素材网格卸载（闪白、丢滚动位置、丢已加载分页）。
 * 失效保留现有数据与 `pageParams`，后台重取完成后再整体替换，用户只看到内容更新。
 * 能定位到单条 id 的事件（重命名、状态/缩略图变化、正文预览、删除）直接局部 patch，
 * 连整表重取都省掉。
 *
 * `listen` 是异步的，而 effect 清理是同步的：先标记 `disposed`，等在途的订阅
 * 完成后立刻退订。这样 StrictMode 的「挂载→卸载→再挂载」不会留下重复监听。
 * `client` / `fetchCard` 可注入，测试里用独立实例验证调用。
 */
export function subscribeAppEvents(
  client: QueryClient = queryClient,
  fetchCard: ItemCardFetcher = fetchItemCard,
): () => void {
  return subscribe(ALL_EVENTS, client, fetchCard);
}

/**
 * 兼容入口：`App.tsx` 只关心「列表何时该刷新」，因此只订阅列表相关事件。
 *
 * 与 [`subscribeAppEvents`] 共用同一套 `handle` 逻辑，避免两处各写一遍事件名，
 * 也避免同一事件在 App 层被重复订阅两次。
 */
export function subscribeItemEvents(
  client: QueryClient = queryClient,
  fetchCard: ItemCardFetcher = fetchItemCard,
): () => void {
  return subscribe(LIST_EVENTS, client, fetchCard);
}

function subscribe(
  names: readonly EventName[],
  client: QueryClient,
  fetchCard: ItemCardFetcher,
): () => void {
  let disposed = false;
  const unlisteners: UnlistenFn[] = [];

  /** 两种列表视图共用同一套刷新语义：失效保留数据，不再重置分页。 */
  const invalidateLists = () => {
    void client.invalidateQueries({ queryKey: queryKeys.items });
    void client.invalidateQueries({ queryKey: ["items-filtered"] });
  };

  /** 筛选视图的成员可能因状态/标签/删除变化：只重取当前活跃的筛选列表。 */
  const invalidateFilteredLists = () => {
    void client.invalidateQueries({ queryKey: ["items-filtered"] });
  };

  /** 事件载荷只有 id：补一次单条读取后原地替换；读不到按删除处理。 */
  const syncItem = (id: string) => {
    void fetchCard(id)
      .then((next) => {
        if (disposed) {
          return;
        }
        if (next === null) {
          removeItemsEverywhere(client, new Set([id]));
          return;
        }
        patchItemEverywhere(client, next);
      })
      .catch((error: unknown) => {
        console.error(`refresh item ${id} failed`, error);
      });
  };

  const handle = (name: EventName, payload: unknown) => {
    if (disposed) {
      return;
    }

    if (name === "import_task_progress") {
      const id = itemIdOf(payload);
      if (id !== null) void client.invalidateQueries({ queryKey: queryKeys.importTask(id) });
      void client.invalidateQueries({ queryKey: queryKeys.importTasks });
      return;
    }

    if (name === "import_task_finished") {
      // 终态任务行已被删除：直接把事件快照写进缓存，失效重取只会拿到 404。
      const finished = finishedTaskOf(payload);
      if (finished !== null) {
        client.setQueryData(queryKeys.importTask(finished.task.id), finished.task);
        client.setQueryData([...queryKeys.importTask(finished.task.id), "failures"], {
          entries: finished.failures,
          nextCursor: null,
        });
      }
      void client.invalidateQueries({ queryKey: queryKeys.importTasks });
      void client.invalidateQueries({ queryKey: queryKeys.importSources });
      return;
    }

    if (name === "item_renamed") {
      const id = itemIdOf(payload);
      if (id !== null) {
        // 重命名只改标题：按 id patch，既不重置也不重取全部页。
        syncItem(id);
        void client.invalidateQueries({ queryKey: queryKeys.detail(id) });
      }
      return;
    }

    if (name === "item_updated") {
      const id = itemIdOf(payload);
      if (id !== null) {
        // 重新解析、导入覆盖、缩略图回填都走这里：只更新该条。
        syncItem(id);
        void client.invalidateQueries({ queryKey: queryKeys.detail(id) });
        // 状态变化可能让该条离开「按状态筛选」的结果集。
        invalidateFilteredLists();
      }
      return;
    }

    if (name === "item_deleted") {
      const id = itemIdOf(payload);
      if (id !== null) {
        // 已删除的项不该出现在任何已缓存列表里：直接移除，不重取分页。
        removeItemsEverywhere(client, new Set([id]));
      }
      void client.invalidateQueries({ queryKey: queryKeys.usage });
      void client.invalidateQueries({ queryKey: queryKeys.orphans });
      void client.invalidateQueries({ queryKey: queryKeys.purgePreview });
      return;
    }

    if (name === "items_deleted") {
      // 批量删除：载荷只有计数，没有 ids；按列表失效兜底（数据保留，不会闪白）。
      invalidateLists();
      void client.invalidateQueries({ queryKey: queryKeys.usage });
      void client.invalidateQueries({ queryKey: queryKeys.orphans });
      void client.invalidateQueries({ queryKey: queryKeys.purgePreview });
      void client.invalidateQueries({ queryKey: queryKeys.tags });
      return;
    }

    if ((LIST_EVENTS as readonly string[]).includes(name)) {
      invalidateLists();
      // 清空确认词里的条数依赖当前库内容，列表一变就过期。
      void client.invalidateQueries({ queryKey: queryKeys.purgePreview });
      if ((USAGE_EVENTS as readonly string[]).includes(name)) {
        void client.invalidateQueries({ queryKey: queryKeys.usage });
      }
      if ((ORPHAN_EVENTS as readonly string[]).includes(name)) {
        void client.invalidateQueries({ queryKey: queryKeys.orphans });
      }
      if (name === "items_reset" || name === "items_pruned") {
        void client.invalidateQueries({ queryKey: queryKeys.tags });
      }
      return;
    }

    if (name === "item_note_updated") {
      const id = itemIdOf(payload);
      if (id !== null) {
        void client.invalidateQueries({ queryKey: queryKeys.detail(id) });
      }
      return;
    }

    if (name === "item_content_updated") {
      const id = itemIdOf(payload);
      if (id !== null) {
        void client.invalidateQueries({ queryKey: queryKeys.detail(id) });
        // 正文会改变卡片预览（列表 / 筛选结果都用 preview 渲染）；
        // 摘要与 OCR 不影响列表。只读取该条，不重取分页。
        if (contentFieldOf(payload) === "bodyText") {
          syncItem(id);
        }
      }
      return;
    }

    if (name === "item_tags_updated") {
      const id = itemIdOf(payload);
      if (id !== null) {
        void client.invalidateQueries({ queryKey: queryKeys.detail(id) });
      }
      void client.invalidateQueries({ queryKey: queryKeys.tags });
      // 标签变化可能改变「按标签筛选」的成员。
      invalidateFilteredLists();
      return;
    }

    if (name === "tags_changed") {
      void client.invalidateQueries({ queryKey: queryKeys.tags });
      return;
    }

    if (name === "hotkey_restored") {
      // 后端重载了捕获热键：设置面板显示的组合键必须跟着变。
      void client.invalidateQueries({ queryKey: queryKeys.settings });
      return;
    }

    if (name === "storage_migration_progress" && phaseOf(payload) === "done") {
      // 迁移完成：库路径与「待重启」闸门都变了，用量口径也随之变化。
      void client.invalidateQueries({ queryKey: queryKeys.status });
      void client.invalidateQueries({ queryKey: queryKeys.usage });
      return;
    }

    if (name === "item_indexed" || name === "item_enriched") {
      // 阶段一完成（三路可检索）/ 阶段二完成（摘要与标签落地）。
      // 所有编辑保存都会触发重索引并补发 item_indexed：按 id 更新该条，
      // 不能整表失效或重置，否则保存备注/标题/正文后列表会闪白并丢分页。
      const id = itemIdOf(payload);
      if (id !== null) {
        syncItem(id);
        void client.invalidateQueries({ queryKey: queryKeys.detail(id) });
      }
      if (name === "item_enriched") {
        void client.invalidateQueries({ queryKey: queryKeys.tags });
      }
      // 状态/自动标签变化可能改变筛选成员。
      invalidateFilteredLists();
      return;
    }

    // pipeline_status / semantic_status 只驱动状态栏与降级标注（局部 state），
    // 它们不改变任何查询结果，刻意不进 Query 缓存。
  };

  for (const name of names) {
    listen(name, (event) => handle(name, event.payload))
      .then((unlisten) => {
        if (disposed) {
          unlisten();
        } else {
          unlisteners.push(unlisten);
        }
      })
      .catch((error: unknown) => {
        console.error(`listen ${name} failed`, error);
      });
  }

  return () => {
    disposed = true;
    for (const unlisten of unlisteners.splice(0)) {
      unlisten();
    }
  };
}
