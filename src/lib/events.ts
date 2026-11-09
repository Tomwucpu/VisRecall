import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { QueryClient } from "@tanstack/react-query";
import { queryClient, queryKeys } from "./queryClient";

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
  | "items_reset"
  | "items_pruned"
  | "item_note_updated"
  | "item_tags_updated"
  | "tags_changed"
  | "hotkey_restored"
  | "storage_migration_progress"
  // 阶段 03：索引管线与语义通道事件
  | "item_indexed"
  | "item_enriched"
  | "pipeline_status"
  | "semantic_status";

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
  "items_reset",
  "items_pruned",
  "item_note_updated",
  "item_tags_updated",
  "tags_changed",
  "hotkey_restored",
  "storage_migration_progress",
  "item_indexed",
  "item_enriched",
  "pipeline_status",
  "semantic_status",
];

function itemIdOf(payload: unknown): string | null {
  if (typeof payload === "object" && payload !== null) {
    const id = (payload as { id?: unknown }).id;
    if (typeof id === "string") {
      return id;
    }
  }
  return null;
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

/**
 * 订阅全部后端事件并按事件类型分派失效范围，返回清理函数。
 *
 * 用 `resetQueries` 而不是 `invalidateQueries`：失效会逐页保留旧 `pageParams`，
 * 新素材插到顶部后，已加载页的游标会把原边界项挤到下一页而丢项。重置会丢掉
 * 分页状态、回到第一页重取，保证新素材出现在顶部且分页边界正确。
 *
 * `listen` 是异步的，而 effect 清理是同步的：先标记 `disposed`，等在途的订阅
 * 完成后立刻退订。这样 StrictMode 的「挂载→卸载→再挂载」不会留下重复监听。
 * `client` 可注入，测试里用独立实例验证调用。
 */
export function subscribeAppEvents(client: QueryClient = queryClient): () => void {
  return subscribe(ALL_EVENTS, client);
}

/**
 * 兼容入口：`App.tsx` 只关心「列表何时该刷新」，因此只订阅列表相关事件。
 *
 * 与 [`subscribeAppEvents`] 共用同一套 `handle` 逻辑，避免两处各写一遍事件名，
 * 也避免同一事件在 App 层被重复订阅两次。
 */
export function subscribeItemEvents(client: QueryClient = queryClient): () => void {
  return subscribe(LIST_EVENTS, client);
}

function subscribe(names: readonly EventName[], client: QueryClient): () => void {
  let disposed = false;
  const unlisteners: UnlistenFn[] = [];

  const handle = (name: EventName, payload: unknown) => {
    if (disposed) {
      return;
    }

    if ((LIST_EVENTS as readonly string[]).includes(name)) {
      void client.resetQueries({ queryKey: queryKeys.items });
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

    if (name === "item_tags_updated") {
      const id = itemIdOf(payload);
      if (id !== null) {
        void client.invalidateQueries({ queryKey: queryKeys.detail(id) });
      }
      void client.invalidateQueries({ queryKey: queryKeys.tags });
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
      // 阶段一完成（三路可检索）/ 阶段二完成（摘要与标签落地）：列表与检索缓存都要重取，
      // 否则用户刚捕获的素材在搜索结果里看不到，或看到的是旧的占位摘要。
      void client.resetQueries({ queryKey: queryKeys.items });
      void client.invalidateQueries({ queryKey: queryKeys.search });
      const id = itemIdOf(payload);
      if (id !== null) {
        void client.invalidateQueries({ queryKey: queryKeys.detail(id) });
      }
      if (name === "item_enriched") {
        void client.invalidateQueries({ queryKey: queryKeys.tags });
      }
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
