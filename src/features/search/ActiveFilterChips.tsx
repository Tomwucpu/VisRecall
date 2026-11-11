import type { FilterChip } from "../../lib/filters";

type Props = {
  chips: FilterChip[];
  onRemove: (chipId: string) => void;
  onClear: () => void;
};

/**
 * 生效条件行：每个 chip 精确移除一个值，行尾「清空全部」一次性回到无筛选。
 *
 * 与 `QueryChips` 分开渲染：关键词条件由检索解析器产生（移除要改写原始查询串），
 * 结构条件由前端状态驱动，两套语义混在一行会互相污染。
 */
export function ActiveFilterChips({ chips, onRemove, onClear }: Props) {
  if (chips.length === 0) {
    return null;
  }

  return (
    <div className="active-filters" role="group" aria-label="生效的筛选条件">
      <span className="label">筛选</span>
      {chips.map((chip) => (
        <span key={chip.id} className="filter-chip">
          <span>{chip.label}</span>
          <button
            type="button"
            className="filter-chip-remove"
            aria-label={`移除筛选 ${chip.label}`}
            title={`移除筛选 ${chip.label}`}
            onClick={() => onRemove(chip.id)}
          >
            ×
          </button>
        </span>
      ))}
      <button type="button" className="filter-clear-all" onClick={onClear}>
        清空全部
      </button>
    </div>
  );
}
