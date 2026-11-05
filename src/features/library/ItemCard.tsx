import { formatAbsoluteTime, formatRelativeTime, kindLabel, statusLabel } from "../../lib/format";

/** 卡片只需要列表展示字段；`thumbUrl` 已是可直接给 `<img>` 用的地址。 */
export type ItemCardData = {
  id: string;
  kind: string;
  status: string;
  capturedAt: number;
  sourceApp: string | null;
  sourceTitle: string | null;
  thumbUrl: string | null;
};

type Props = {
  item: ItemCardData;
};

/** 单张素材卡：缩略图（无图时占位）、状态徽标、来源与相对时间。 */
export function ItemCard({ item }: Props) {
  const kind = kindLabel(item.kind);
  const title = item.sourceTitle ?? "未命名来源";

  return (
    <article className="item-card" data-item-id={item.id} title={item.sourceTitle ?? undefined}>
      <div className={`item-thumb item-thumb-${item.kind}`}>
        {item.thumbUrl ? (
          <img
            className="item-thumb-img"
            src={item.thumbUrl}
            alt={`${kind}素材缩略图`}
            loading="lazy"
            decoding="async"
            draggable={false}
          />
        ) : (
          <span className="item-thumb-placeholder">{kind}</span>
        )}
      </div>
      <div className="item-body">
        <div className="item-badges">
          <span className={`item-status item-status-${item.status}`}>{statusLabel(item.status)}</span>
          <span className="item-kind">{kind}</span>
        </div>
        <p className="item-title">{title}</p>
        <div className="item-meta">
          <span className="item-source">{item.sourceApp ?? "未知来源"}</span>
          <time
            className="item-time"
            dateTime={new Date(item.capturedAt).toISOString()}
            title={formatAbsoluteTime(item.capturedAt)}
          >
            {formatRelativeTime(item.capturedAt)}
          </time>
        </div>
      </div>
    </article>
  );
}
