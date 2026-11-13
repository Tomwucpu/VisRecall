import type { ParsedQueryDto } from "../../lib/searchTypes";
import { isSoftChip, softWordOf } from "../../lib/searchTypes";

type Props = {
  parsed: ParsedQueryDto;
  /** 删除一个 chip：把对应的条件片段从查询串里去掉后重搜。 */
  onRemove?: (chip: string) => void;
  /** 软 chip 专用：把该词改写成 `"词"` 走字面通道（不再当参考条件）。 */
  onToLiteral?: (chip: string) => void;
};

/**
 * 解析结果 chips：后端已经把「结构化条件」解析成人类可读标签（`parsed.chips`）。
 *
 * 两种 chip 的语义不同，样式与操作也不同：
 * - **硬条件**（时间 / `app:X` / `#tag` / 引号）：会筛掉不满足的素材，只能 `×` 删除；
 * - **软命中**（颜色 / 图表 / 类型 / 公式，带「(参考)」后缀）：只加分不排除，因此除了
 *   `×`（连语义一起删掉）还提供「仅文字」——把词改写成 `"词"` 的引号字面量，
 *   只想搜这两个字、不想按颜色排序时用它。
 *
 * 删除 chip 的做法是**从原文里删掉那段词**再重搜，而不是改结构化条件：
 * 后端解析是纯函数，改了条件就与原文不一致，下次输入又会被解析回来。
 */
export function QueryChips({ parsed, onRemove, onToLiteral }: Props) {
  const chips = parsed.chips;
  const interactive = onRemove !== undefined;

  if (chips.length === 0) {
    return null;
  }

  return (
    <ul className="query-chips" aria-label="搜索条件">
      {chips.map((chip) => {
        const soft = isSoftChip(chip);
        const word = softWordOf(chip);
        return (
          <li key={chip} className={soft ? "query-chip is-soft" : "query-chip"}>
            <span className="query-chip-text">{chip}</span>
            {soft && onToLiteral !== undefined && (
              <button
                type="button"
                className="query-chip-literal"
                title={`只按文字搜「${word}」，不再当参考条件`}
                onClick={() => onToLiteral(chip)}
              >
                仅文字
              </button>
            )}
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
        );
      })}
    </ul>
  );
}
