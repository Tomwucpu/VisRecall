import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { QueryClient } from "@tanstack/react-query";
import { queryClient, queryKeys } from "./queryClient";

/** 会让素材列表过期的后端事件（design §11.2）。 */
const ITEM_EVENTS = ["item_created", "item_updated"] as const;

/**
 * 订阅素材事件并返回清理函数。
 *
 * 用 `resetQueries` 而不是 `invalidateQueries`：失效会逐页保留旧 `pageParams`，
 * 新素材插到顶部后，已加载页的游标会把原边界项挤到下一页而丢项。重置会丢掉
 * 分页状态、回到第一页重取，保证新素材出现在顶部且分页边界正确。
 *
 * `listen` 是异步的，而 effect 清理是同步的：先标记 `disposed`，等在途的订阅
 * 完成后立刻退订。这样 StrictMode 的「挂载→卸载→再挂载」不会留下重复监听。
 * `client` 可注入，测试里用独立实例验证调用。
 */
export function subscribeItemEvents(client: QueryClient = queryClient): () => void {
  let disposed = false;
  const unlisteners: UnlistenFn[] = [];

  const resetItems = () => {
    if (disposed) {
      return;
    }
    void client.resetQueries({ queryKey: queryKeys.items });
  };

  for (const name of ITEM_EVENTS) {
    listen(name, resetItems)
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
