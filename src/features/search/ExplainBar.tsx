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
 * - 有 `filters` → 列出真正命中的筛选条件（只列命中的，不列用户输入的全部）；
 * - 有 `terms`   → 列出实际命中的词；
 * - `semantic`   → 语义匹配（此时通常没有字面命中）。
 */
export function ExplainBar({ explanation, semanticReady = true }: Props) {
  const source = hitSource(explanation);
  const sentence = hitSourceSentence(source, explanation.terms);
  const filters = explanation.filters;
  const highlightCount = explanation.highlightBoxes.length;

  return (
    <div className={`explain-bar is-${source}`} role="note">
      <span className="explain-sentence">{sentence}</span>
      {filters.length > 0 && (
        <span className="explain-filters">
          {filters.map((filter) => (
            <span key={filter} className="explain-filter">
              {filter}
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
