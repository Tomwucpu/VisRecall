import { useInfiniteQuery } from "@tanstack/react-query";
import { commands, type ItemDto } from "../../lib/bindings";
import type { ItemFilters } from "../../lib/api";
import { toThumbSrc } from "../../lib/format";
import { hueForColor } from "../../lib/hue";
import { queryKeys } from "../../lib/queryClient";
import type { ItemCardData } from "./ItemCard";

/** 与后端 `list_items` 的分页上限一致（命令内 clamp 到 1..=100，这里固定 50）。 */
export const ITEM_PAGE_SIZE = 50;

/** `ItemDto` → 卡片数据；缩略图 URL 在这里一次性转成可加载地址。 */
export function toItemCardData(dto: ItemDto): ItemCardData {
  return {
    id: dto.id,
    kind: dto.kind,
    status: dto.status,
    capturedAt: dto.capturedAt,
    sourceApp: dto.sourceApp ?? null,
    sourceTitle: dto.sourceTitle ?? null,
    title: dto.title ?? null,
    thumbUrl: toThumbSrc(dto.thumbUrl ?? null),
    // 阶段 03：主色 → 主题色令牌（未解析为 null，落到中性灰）
    hue: hueForColor(dto.primaryColor ?? null),
    preview: dto.preview ?? null,
  };
}

/** 无限分页 → 去重后的有序卡片数组（同一条可能出现在相邻页边界）。 */
export function flattenItemPages(pages: ItemCardData[][] | undefined): ItemCardData[] {
  const loaded = new Map<string, ItemCardData>();
  for (const page of pages ?? []) {
    for (const item of page) {
      loaded.set(item.id, item);
    }
  }
  return [...loaded.values()];
}

/** 游标：本页满 `ITEM_PAGE_SIZE` 才认为后面还有，未满页直接结束。 */
function nextCursor(lastPage: ItemCardData[]): number | undefined {
  if (lastPage.length < ITEM_PAGE_SIZE) {
    return undefined;
  }
  return lastPage[lastPage.length - 1]?.capturedAt ?? undefined;
}

/** 素材列表：按 `capturedAt` 倒序游标分页。 */
export function useItems() {
  return useInfiniteQuery({
    queryKey: queryKeys.items,
    queryFn: async ({ pageParam }) => {
      const page = await commands.listItems(pageParam, ITEM_PAGE_SIZE);
      return page.map(toItemCardData);
    },
    initialPageParam: null as number | null,
    // 后端游标是「capturedAt 严格小于」：本页满 50 才认为后面还有，
    // 未满页直接结束，避免多打一次空请求。
    getNextPageParam: nextCursor,
  });
}

/**
 * 带结构化筛选的素材列表（无关键词时使用）。
 *
 * `key` 必须与 `filters` 同源（`filtersKey`），否则同一份缓存可能被不同条件复用；
 * `enabled=false` 时保留 hook 调用顺序但不开请求（关键词检索态下不需要它）。
 */
export function useFilteredItems(filters: ItemFilters, key: string, enabled = true) {
  return useInfiniteQuery({
    queryKey: queryKeys.itemsFiltered(key),
    queryFn: async ({ pageParam }) => {
      const page = await commands.listItemsFiltered(filters, pageParam, ITEM_PAGE_SIZE);
      return page.map(toItemCardData);
    },
    initialPageParam: null as number | null,
    getNextPageParam: nextCursor,
    enabled,
  });
}
