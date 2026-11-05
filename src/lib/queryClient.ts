import { QueryClient } from "@tanstack/react-query";

/** 全应用共用的查询键；事件刷新与列表共用同一份定义，避免拼错。 */
export const queryKeys = {
  items: ["items"] as const,
};

/**
 * 默认 5 秒内视为新鲜（避免同一窗口内来回切换重复拉取），并且不在窗口聚焦时自动重取。
 * 素材变化由 `item_created` / `item_updated` 事件显式失效，比轮询可靠。
 */
export const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: 5_000,
      refetchOnWindowFocus: false,
    },
  },
});
