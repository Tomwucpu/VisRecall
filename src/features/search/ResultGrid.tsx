import { ItemCard, type ItemCardData } from "../library/ItemCard";
import type { SearchHit } from "../../lib/searchTypes";
import { hitSource, hitSourceLabel } from "../../lib/searchTypes";
import { ExplainBar } from "./ExplainBar";
import { hueForColor } from "../../lib/hue";

type Props = {
  hits: SearchHit[];
  status: "idle" | "ready" | "error";
  errorMessage: string | null;
  /** 语义通道是否就绪（降级时结果区顶部给一行说明）。 */
  semanticReady: boolean;
  total: number;
  onOpen: (item: ItemCardData) => void;
  onNotThis: (hit: SearchHit) => void;
  selectedId: string | null;
};

/** 命中 → 卡片数据（`hue` 来自 `primaryColor`，`hit` 用于徽标）。 */
function toCardData(hit: SearchHit): ItemCardData & { hit: string; summary: string | null } {
  const item = hit.item;
  return {
    id: item.id,
    kind: item.kind,
    status: item.status,
    capturedAt: item.capturedAt,
    // DTO 的可选字段在这里统一成 `null`，避免卡片里出现 `undefined` 分支
    sourceApp: item.sourceApp ?? null,
    sourceTitle: item.sourceTitle ?? null,
    thumbUrl: item.thumbUrl ?? null,
    hue: hueForColor(item.primaryColor),
    hit: hitSourceLabel(hitSource(hit.explanation)),
    // 摘要首行：解释里没有摘要字段，详情面板才展示完整摘要；
    // 列表只显示「命中词」，避免把 200 字摘要塞进卡片。
    summary: hit.explanation.terms.length > 0 ? hit.explanation.terms.join(" · ") : null,
  };
}

/** 检索结果网格：空结果/失败/降级各有一句明确的说明，不留白屏。 */
export function ResultGrid({
  hits,
  status,
  errorMessage,
  semanticReady,
  total,
  onOpen,
  onNotThis,
  selectedId,
}: Props) {
  if (status === "error") {
    return (
      <div className="alert" role="alert">
        <span>检索失败：{errorMessage ?? "未知错误"}</span>
      </div>
    );
  }

  if (status === "ready" && hits.length === 0) {
    return (
      <div className="search-empty" role="status">
        <p>没有符合条件的素材。</p>
        <p className="hint">
          试着去掉一个条件（比如颜色或时间），或者换一个更短的说法。
          {!semanticReady && " 语义检索当前不可用，只按字面与条件匹配。"}
        </p>
      </div>
    );
  }

  return (
    <>
      {!semanticReady && (
        <p className="search-degraded-note" role="status">
          语义检索未就绪：结果只来自字面与条件匹配。
        </p>
      )}
      <div className="grid" aria-label="检索结果">
        {hits.map((hit) => {
          const card = toCardData(hit);
          return (
            <div key={hit.item.id} className="search-result">
              <ItemCard item={card} onOpen={onOpen} selected={hit.item.id === selectedId} />
              <ExplainBar explanation={hit.explanation} semanticReady={semanticReady} />
              <button
                type="button"
                className="mini-btn not-this"
                title="标记为不相关，用于评测调参"
                onClick={() => onNotThis(hit)}
              >
                不是这个
              </button>
            </div>
          );
        })}
      </div>
      <p className="search-total">
        命中 {total} 项{hits.length < total ? `（显示前 ${hits.length} 项）` : ""}
      </p>
    </>
  );
}
