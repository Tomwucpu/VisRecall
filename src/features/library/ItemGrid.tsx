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
};

/**
 * 素材网格：CSS 多栏瀑布流（`column-count`，与原型一致），阶段 01 直接渲染全部
 * 已加载项（≤100 条），不做虚拟滚动。已有数据时刷新失败只加顶部错误条，保留网格。
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

      <ul className="grid">
        {items.map((item) => (
          <li key={item.id} className="grid-cell">
            <ItemCard item={item} />
          </li>
        ))}
      </ul>

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
