import { useInfiniteQuery } from "@tanstack/react-query";
import { commands, type ItemDto } from "../../lib/bindings";
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
    thumbUrl: toThumbSrc(dto.thumbUrl ?? null),
    // 阶段 03：主色 → 主题色令牌（未解析为 null，落到中性灰）
    hue: hueForColor(dto.primaryColor ?? null),
  };
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
    getNextPageParam: (lastPage) => {
      if (lastPage.length < ITEM_PAGE_SIZE) {
        return undefined;
      }
      return lastPage[lastPage.length - 1]?.capturedAt ?? undefined;
    },
  });
}
