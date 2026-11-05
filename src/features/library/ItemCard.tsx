import { useEffect, useState } from "react";
import {
  formatAbsoluteTime,
  formatRelativeTime,
  kindLabel,
  statusLabel,
  statusTone,
} from "../../lib/format";

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
  const hasTime = Number.isFinite(item.capturedAt);
  const timeText = hasTime ? formatRelativeTime(item.capturedAt) : "未知时间";
  // 非法时间戳直接跳过 `toISOString()`，否则会抛 RangeError。
  const isoTime = hasTime ? new Date(item.capturedAt).toISOString() : undefined;

  // 缩略图加载失败时退回占位，避免破图标；`thumbUrl` 变化后重试。
  const [failed, setFailed] = useState(false);
  useEffect(() => {
    setFailed(false);
  }, [item.thumbUrl]);

  const thumbSrc = item.thumbUrl !== null && !failed ? item.thumbUrl : null;

  return (
    <article className="item-card" data-item-id={item.id} title={item.sourceTitle ?? undefined}>
      <div className={`item-thumb item-thumb-${item.kind}`}>
        {thumbSrc !== null ? (
          <img
            className="item-thumb-img"
            src={thumbSrc}
            alt={`${kind}素材缩略图`}
            loading="lazy"
            decoding="async"
            draggable={false}
            onError={() => setFailed(true)}
          />
        ) : (
          <span className="item-thumb-placeholder">{kind}</span>
        )}
      </div>
      <div className="item-body">
        <div className="item-badges">
          <span className={`item-status item-status-${statusTone(item.status)}`}>
            {statusLabel(item.status)}
          </span>
          <span className="item-kind">{kind}</span>
        </div>
        <p className="item-title">{title}</p>
        <div className="item-meta">
          <span className="item-source">{item.sourceApp ?? "未知来源"}</span>
          <time className="item-time" dateTime={isoTime} title={formatAbsoluteTime(item.capturedAt)}>
            {timeText}
          </time>
        </div>
      </div>
    </article>
  );
}
