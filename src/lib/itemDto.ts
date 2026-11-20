import type { ItemDto } from "./bindings";
import { toThumbSrc } from "./format";
import { hueForColor } from "./hue";
import type { ItemCardData } from "../features/library/ItemCard";

/**
 * `ItemDto` → 卡片数据。
 *
 * 列表分页与事件后的单条刷新共用这份映射，保证两条路径产出的卡片字段完全一致。
 */
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
