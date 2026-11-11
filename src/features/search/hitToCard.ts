import type { ItemCardData } from "../library/ItemCard";
import type { SearchHit } from "../../lib/searchTypes";
import { hitSource, hitSourceLabel } from "../../lib/searchTypes";
import { toThumbSrc } from "../../lib/format";
import { hueForColor } from "../../lib/hue";

/**
 * 检索命中 → 卡片数据。
 *
 * 单独成文件：`ResultGrid` 与 `App`（详情/多选）都要把命中转成卡片；
 * 放在组件文件里会触发 Fast Refresh 的「非组件导出」告警。
 */
export function hitToCardData(
  hit: SearchHit,
): ItemCardData & { hit: string; summary: string | null } {
  const item = hit.item;
  return {
    id: item.id,
    kind: item.kind,
    status: item.status,
    capturedAt: item.capturedAt,
    // DTO 的可选字段在这里统一成 `null`，避免卡片里出现 `undefined` 分支
    sourceApp: item.sourceApp ?? null,
    sourceTitle: item.sourceTitle ?? null,
    title: item.title ?? null,
    // 检索命中的 item 是原始 ItemDto：和 useItems 一样必须转成可加载的协议地址。
    thumbUrl: toThumbSrc(item.thumbUrl ?? null),
    hue: hueForColor(item.primaryColor),
    preview: item.preview ?? null,
    hit: hitSourceLabel(hitSource(hit.explanation)),
    // 摘要首行：解释里没有摘要字段，详情面板才展示完整摘要；
    // 列表只显示「命中词」，避免把 200 字摘要塞进卡片。
    summary: hit.explanation.terms.length > 0 ? hit.explanation.terms.join(" · ") : null,
  };
}
