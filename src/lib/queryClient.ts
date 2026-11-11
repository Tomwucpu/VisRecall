import { QueryClient } from "@tanstack/react-query";

/** 全应用共用的查询键；事件刷新与列表共用同一份定义，避免拼错。 */
export const queryKeys = {
  items: ["items"] as const,
  /** 带筛选的列表分页；`key` 是筛选条件的稳定序列化串。 */
  itemsFiltered: (key: string) => ["items-filtered", key] as const,
  detail: (id: string) => ["detail", id] as const,
  settings: ["settings"] as const,
  tags: ["tags"] as const,
  usage: ["usage"] as const,
  orphans: ["orphans"] as const,
  status: ["status"] as const,
  purgePreview: ["purge-preview"] as const,
  modelConfig: ["model-config"] as const,
  search: ["search"] as const,
  importTask: (id: string) => ["import-task", id] as const,
  importTasks: ["import-tasks"] as const,
  importSources: ["import-sources"] as const,
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
