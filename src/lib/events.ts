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
  | "tags_changed";

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

const ALL_EVENTS: EventName[] = [
  "item_created",
  "item_updated",
  "item_deleted",
  "items_reset",
  "items_pruned",
  "item_note_updated",
  "item_tags_updated",
  "tags_changed",
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
      if ((USAGE_EVENTS as readonly string[]).includes(name)) {
        void client.invalidateQueries({ queryKey: queryKeys.usage });
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
    }
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
