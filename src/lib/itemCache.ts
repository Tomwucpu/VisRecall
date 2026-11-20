import type { QueryClient } from "@tanstack/react-query";
import { commands } from "./bindings";
import { toItemCardData } from "./itemDto";
import { queryKeys } from "./queryClient";
import type { ItemCardData } from "../features/library/ItemCard";

/** `useInfiniteQuery` 的缓存形状；这里只关心 `pages`，`pageParams` 必须原样保留。 */
type ListPages = {
  pages: ItemCardData[][];
  pageParams: unknown[];
};

/** 与 `invalidateLists` 使用的筛选列表前缀保持一致。 */
const FILTERED_LISTS = ["items-filtered"] as const;

function patchPages(
  data: ListPages | undefined,
  id: string,
  next: ItemCardData,
): ListPages | undefined {
  if (data === undefined) {
    return undefined;
  }
  let changed = false;
  const pages = data.pages.map((page) => {
    const index = page.findIndex((item) => item.id === id);
    if (index === -1) {
      return page;
    }
    changed = true;
    const copy = page.slice();
    copy[index] = next;
    return copy;
  });
  return changed ? { ...data, pages } : data;
}

function withoutIds(data: ListPages | undefined, ids: ReadonlySet<string>): ListPages | undefined {
  if (data === undefined) {
    return undefined;
  }
  let changed = false;
  const pages = data.pages.map((page) => {
    const kept = page.filter((item) => !ids.has(item.id));
    if (kept.length === page.length) {
      return page;
    }
    changed = true;
    return kept;
  });
  return changed ? { ...data, pages } : data;
}

/**
 * 用最新卡片替换所有已缓存列表里的同 id 项。
 *
 * 只改 `pages`、不动 `pageParams`：分页游标仍指向原页边界，后续 `fetchNextPage`
 * 不会断。未命中时返回原引用，配合结构共享避免无谓重渲染。
 */
export function patchItemEverywhere(client: QueryClient, next: ItemCardData): void {
  client.setQueriesData<ListPages>({ queryKey: queryKeys.items }, (data) =>
    patchPages(data, next.id, next),
  );
  client.setQueriesData<ListPages>({ queryKey: FILTERED_LISTS }, (data) =>
    patchPages(data, next.id, next),
  );
}

/** 从所有已缓存列表里移除已删除的项（全部素材 + 各筛选视图）。 */
export function removeItemsEverywhere(
  client: QueryClient,
  ids: ReadonlySet<string>,
): void {
  if (ids.size === 0) {
    return;
  }
  client.setQueriesData<ListPages>({ queryKey: queryKeys.items }, (data) =>
    withoutIds(data, ids),
  );
  client.setQueriesData<ListPages>({ queryKey: FILTERED_LISTS }, (data) =>
    withoutIds(data, ids),
  );
}

/**
 * 读取单条素材的最新卡片数据。
 *
 * 事件载荷只有 id，要局部更新就必须补一次单条读取。素材已不存在时返回 `null`，
 * 调用方按「删除」处理缓存。
 */
export async function fetchItemCard(id: string): Promise<ItemCardData | null> {
  try {
    return toItemCardData(await commands.getItem(id));
  } catch {
    return null;
  }
}
