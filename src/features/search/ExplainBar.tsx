import type { ExplanationDto } from "../../lib/searchTypes";
import { hitSource, hitSourceSentence } from "../../lib/searchTypes";

type Props = {
  explanation: ExplanationDto;
  /** 语义通道是否就绪：未就绪时明确写「未使用语义」，而不是让用户以为语义没命中。 */
  semanticReady?: boolean;
};

/**
 * 命中解释（P3-R08）：一行说清「为什么这条结果会被返回」。
 *
 * 三类来源与后端 `explanation` 一一对应：
 * - 有 `terms`    → 列出实际命中的词；
 * - 有 `facets`   → 列出实际命中的**软**条件（颜色/图表/类型/公式，只加分不排除）；
 * - 有 `filters`  → 列出真正命中的**硬**筛选条件；
 * - `semantic`    → 语义匹配（此时通常没有字面命中、也没有硬/软条件）。
 *
 * 软条件单独一行是必须的：混进 `filters` 会让用户以为「红色」把别的素材筛掉了。
 */
export function ExplainBar({ explanation, semanticReady = true }: Props) {
  const source = hitSource(explanation);
  const facets = explanation.facets ?? [];
  const sentence = hitSourceSentence(source, explanation.terms, facets);
  const filters = explanation.filters;
  const highlightCount = explanation.highlightBoxes.length;

  return (
    <div className={`explain-bar is-${source}`} role="note">
      <span className="explain-sentence">{sentence}</span>
      {filters.length > 0 && (
        <span className="explain-filters" title="这些条件会筛掉不满足的素材">
          {filters.map((filter) => (
            <span key={filter} className="explain-filter">
              {filter}
            </span>
          ))}
        </span>
      )}
      {facets.length > 0 && (
        <span className="explain-facets" title="软命中只加分不排除">
          {facets.map((facet) => (
            <span key={facet} className="explain-facet">
              {facet}
            </span>
          ))}
        </span>
      )}
      {highlightCount > 0 && (
        <span className="explain-boxes" title="原图上会高亮这些文字">
          {highlightCount} 处文字高亮
        </span>
      )}
      {source === "semantic" && !semanticReady && (
        <span className="explain-warn">语义通道未就绪，此结果来自缓存向量</span>
      )}
    </div>
  );
}
