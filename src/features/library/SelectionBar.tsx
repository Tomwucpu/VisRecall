type Props = {
  count: number;
  /** 当前筛选下的总数；搜索态为命中总数，未知时为 `null`。 */
  totalCount: number | null;
  /** 已删空或正在删除时禁用按钮。 */
  busy?: boolean;
  onSelectAll: () => void;
  onClear: () => void;
  onDelete: () => void;
};

/**
 * 批量操作条：只在选择模式且有选中项时出现（`App` 负责条件渲染）。
 *
 * `role=status` 播报已选数量：键盘用户勾选时不必把视线移到条上才能确认。
 */
export function SelectionBar({
  count,
  totalCount,
  busy = false,
  onSelectAll,
  onClear,
  onDelete,
}: Props) {
  const allText =
    totalCount === null ? "全选" : `全选（${Math.max(totalCount, count)}）`;

  return (
    <div className="selection-bar" role="status" aria-live="polite">
      <span className="selection-count">已选 {count} 项</span>
      <div className="selection-actions">
        <button type="button" className="mini-btn" onClick={onSelectAll} disabled={busy}>
          {allText}
        </button>
        <button type="button" className="mini-btn" onClick={onClear} disabled={busy}>
          取消选择
        </button>
        <button type="button" className="btn danger selection-delete" onClick={onDelete} disabled={busy}>
          {busy ? "正在删除…" : "删除"}
        </button>
      </div>
    </div>
  );
}
