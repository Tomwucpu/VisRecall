import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { queryClient, queryKeys } from "./queryClient";

/** 会让素材列表过期的后端事件（design §11.2）。 */
const ITEM_EVENTS = ["item_created", "item_updated"] as const;

function invalidateItems(): void {
  void queryClient.invalidateQueries({ queryKey: queryKeys.items });
}

/**
 * 订阅素材事件并返回清理函数。
 *
 * `listen` 是异步的，而 effect 清理是同步的：先标记 `disposed`，等在途的订阅
 * 完成后立刻退订。这样 StrictMode 的「挂载→卸载→再挂载」不会留下重复监听。
 */
export function subscribeItemEvents(): () => void {
  let disposed = false;
  const unlisteners: UnlistenFn[] = [];

  for (const name of ITEM_EVENTS) {
    listen(name, invalidateItems)
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
