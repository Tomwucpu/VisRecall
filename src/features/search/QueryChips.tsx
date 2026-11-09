import type { ParsedQueryDto } from "../../lib/searchTypes";

type Props = {
  parsed: ParsedQueryDto;
  /** 删除一个 chip：把对应的条件片段从查询串里去掉后重搜。 */
  onRemove?: (chip: string) => void;
};

/**
 * 解析结果 chips：后端已经把「结构化条件」解析成人类可读标签（`parsed.chips`）。
 *
 * 删除 chip 的做法是**从原文里删掉那段词**再重搜，而不是改结构化条件：
 * 后端解析是纯函数，改了条件就与原文不一致，下次输入又会被解析回来。
 */
export function QueryChips({ parsed, onRemove }: Props) {
  const chips = parsed.chips;
  const interactive = onRemove !== undefined;

  if (chips.length === 0) {
    return null;
  }

  return (
    <ul className="query-chips" aria-label="搜索条件">
      {chips.map((chip) => (
        <li key={chip} className="query-chip">
          <span className="query-chip-text">{chip}</span>
          {interactive && (
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
