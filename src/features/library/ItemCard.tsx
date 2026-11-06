import { useEffect, useState, type CSSProperties } from "react";
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
  /**
   * hue-dot 的取色令牌（如 `var(--hue-ochre)`）。
   * 阶段 03 由视觉属性提供；当前一律缺省，落到 `--hue-ash` 中性灰。
   */
  hue?: string | null;
};

type Props = {
  item: ItemCardData;
};

/** 只有「待解析 / 失败」用印章色强调，其余状态保持中性徽标。 */
function badgeClass(status: string): string {
  const tone = statusTone(status);
  if (tone === "pending" || tone === "failed") {
    return `badge ${tone}`;
  }
  return "badge";
}

/** 单张素材卡：缩略图（无图时占位）、状态徽标、标题与来源/时间元信息。 */
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
  const busy = item.status === "parsing" || item.status === "pending";
  const hueStyle = { "--hue": item.hue ?? "var(--hue-ash)" } as CSSProperties;

  return (
    <article
      className={`card${busy ? " is-pending" : ""}`}
      data-item-id={item.id}
      title={item.sourceTitle ?? undefined}
    >
      <div className="card-thumb">
        {thumbSrc !== null ? (
          <img
            src={thumbSrc}
            alt={`${kind}素材缩略图`}
            loading="lazy"
            decoding="async"
            draggable={false}
            onError={() => setFailed(true)}
          />
        ) : (
          <span className="thumb-fallback">{kind}</span>
        )}
        {busy && <span className="shimmer" />}
        <span className={badgeClass(item.status)}>{statusLabel(item.status)}</span>
      </div>

      <div className="card-body">
        <h3 className="card-title">{title}</h3>
        <div className="card-meta">
          <i className="hue-dot" style={hueStyle} />
          <span className="card-kind">{kind}</span>
          {item.sourceApp !== null && (
            <>
              <span className="dot" />
              <span className="card-source">{item.sourceApp}</span>
            </>
          )}
          <span className="dot" />
          <time className="card-time" dateTime={isoTime} title={formatAbsoluteTime(item.capturedAt)}>
            {timeText}
          </time>
        </div>
      </div>
    </article>
  );
}
