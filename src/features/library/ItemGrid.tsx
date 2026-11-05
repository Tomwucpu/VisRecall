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
 * 素材网格：CSS grid 自适应列，阶段 01 直接渲染全部已加载项（≤100 条），
 * 不做虚拟滚动。
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
    return <p className="app-hint">正在加载素材…</p>;
  }

  if (isError) {
    return (
      <section className="app-card app-card-error" role="alert">
        <h2>无法读取素材</h2>
        <p>{errorMessage ?? "未知错误"}</p>
        <button type="button" onClick={onRetry}>
          重试
        </button>
      </section>
    );
  }

  return (
    <>
      <ul className="item-grid">
        {items.map((item) => (
          <li key={item.id} className="item-grid-cell">
            <ItemCard item={item} />
          </li>
        ))}
      </ul>

      {hasNextPage && (
        <div className="item-grid-more">
          <button type="button" disabled={isFetchingNextPage} onClick={onLoadMore}>
            {isFetchingNextPage ? "正在加载…" : "加载更多"}
          </button>
        </div>
      )}
    </>
  );
}
