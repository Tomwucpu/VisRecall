import { ItemCard, type ItemCardData } from "./ItemCard";

type Props = {
  items: ItemCardData[];
  isPending: boolean;
  isError: boolean;
  errorMessage: string | null;
  hasNextPage: boolean;
  isFetchingNextPage: boolean;
  onLoadMore: () => void;
  onRetry: () => void;
  /** 打开详情；不传时卡片不可交互。 */
  onOpen?: (item: ItemCardData) => void;
  selectedId?: string | null;
  /** 多选模式与选择集。 */
  selectionMode?: boolean;
  selectedIds?: ReadonlySet<string>;
  onToggleSelect?: (item: ItemCardData, shift: boolean) => void;
  /** 空结果文案（有筛选时与「还没有素材」区分）。 */
  emptyText?: string;
};

/**
 * 素材网格：固定 228px 轨道的 CSS Grid（列数随可用宽度自动增减，卡片尺寸不变），
 * 阶段 01 直接渲染全部已加载项（≤100 条），不做虚拟滚动。
 * 已有数据时刷新失败只加顶部错误条，保留网格。
 */
export function ItemGrid({
  items,
  isPending,
  isError,
  errorMessage,
  hasNextPage,
  isFetchingNextPage,
  onLoadMore,
  onRetry,
  onOpen,
  selectedId = null,
  selectionMode = false,
  selectedIds,
  onToggleSelect,
  emptyText = "没有符合当前条件的素材。",
}: Props) {
  if (isPending) {
    return <p className="hint">正在加载素材…</p>;
  }

  // 没有可显示的数据时才用整块错误提示；否则降级成错误条。
  if (isError && items.length === 0) {
    return (
      <div className="alert alert-block" role="alert">
        <h2>无法读取素材</h2>
        <p>{errorMessage ?? "未知错误"}</p>
        <button type="button" className="mini-btn" onClick={onRetry}>
          重试
        </button>
      </div>
    );
  }

  return (
    <>
      {isError && (
        <div className="alert" role="alert">
          <span>刷新失败：{errorMessage ?? "未知错误"}</span>
          <button type="button" className="mini-btn" onClick={onRetry}>
            重试
          </button>
        </div>
      )}

      {items.length === 0 ? (
        <div className="search-empty" role="status">
          <p>{emptyText}</p>
          <p className="hint">试着减少一个条件，或换一个更短的关键词。</p>
        </div>
      ) : (
        <ul className="grid">
          {items.map((item) => (
            <li key={item.id} className="grid-cell">
              <ItemCard
                item={item}
                onOpen={onOpen}
                selected={
                  selectionMode ? (selectedIds?.has(item.id) ?? false) : item.id === selectedId
                }
                selectionMode={selectionMode}
                onToggleSelect={onToggleSelect}
              />
            </li>
          ))}
        </ul>
      )}

      {hasNextPage && (
        <div className="grid-more">
          <button type="button" disabled={isFetchingNextPage} onClick={onLoadMore}>
            {isFetchingNextPage ? "正在加载…" : "加载更多"}
          </button>
        </div>
      )}
    </>
  );
}
