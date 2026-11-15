import type { ParsedQueryDto } from "../../lib/searchTypes";
import { formatTimeRangeChip } from "../../lib/format";

type Props = {
  parsed: ParsedQueryDto;
  /** 删除一个 chip：把对应的条件片段从查询串里去掉后重搜。 */
  onRemove?: (chip: string) => void;
};

/**
 * 解析结果 chips：只显示**可移除**的条件（`来自 app` / `#tag` / `"引号词"`）。
 *
 * 两条刻意不显示的条件：
 * - **软命中**（颜色 / 图表 / 类型 / 公式）：只加分不排除，用户不需要「取消」，
 *   由每条结果的解释条（`ExplainBar` 的 `facets`）说明；
 * - **时间**：原词（`今天` / `最近7天`）已被解析器从原文剥离，删不回来——所以这里
 *   直接按 `parsed.time` 渲染一个**没有 `×`** 的提示 chip，改时间请直接改文字。
 *
 * 删除 chip 的做法是**从原文里删掉那段词**再重搜，而不是改结构化条件：
 * 后端解析是纯函数，改了条件就与原文不一致，下次输入又会被解析回来。
 */
export function QueryChips({ parsed, onRemove }: Props) {
  const chips = parsed.chips;
  const timeChip = parsed.time
    ? formatTimeRangeChip(parsed.time.from, parsed.time.to)
    : "";

  if (chips.length === 0 && timeChip === "") {
    return null;
  }

  return (
    <ul className="query-chips" aria-label="搜索条件">
      {timeChip !== "" && (
        <li
          className="query-chip is-fixed"
          title="时间条件请直接在搜索框里改（如「今天」「最近7天」）"
        >
          <span className="query-chip-text">时间：{timeChip}</span>
        </li>
      )}
      {chips.map((chip) => (
        <li key={chip} className="query-chip">
          <span className="query-chip-text">{chip}</span>
          {onRemove !== undefined && (
            <button
              type="button"
              className="query-chip-remove"
              aria-label={`移除条件 ${chip}`}
              onClick={() => onRemove(chip)}
            >
              ×
            </button>
          )}
        </li>
      ))}
    </ul>
  );
}
