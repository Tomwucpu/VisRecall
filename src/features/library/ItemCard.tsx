import { useEffect, useState, type CSSProperties } from "react";
import { api } from "../../lib/api";
import { errorCode, errorMessage } from "../../lib/errors";
import {
  copyLabel,
  formatAbsoluteTime,
  formatRelativeTime,
  kindLabel,
  statusLabel,
  statusTone,
} from "../../lib/format";
import { pushToast } from "../../lib/toast";

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
   * 阶段 03 由 `item.primaryColor` 经 `hueForColor` 映射；缺省落到 `--hue-ash` 中性灰。
   */
  hue?: string | null;
  /** 检索命中来源的中文标签（`字面命中`/`语义命中`/`条件命中`）；非搜索态为空。 */
  hit?: string | null;
  /** 命中词或摘要首行（搜索态显示，列表态为空）。 */
  summary?: string | null;
  /**
   * 没有缩略图时的文本预览（文本原文 / 链接 URL 的前 160 字符）。
   * 由后端列表接口填充；图片素材与读不到原文时为 `null`，回退到类型占位。
   */
  preview?: string | null;
};

type Props = {
  item: ItemCardData;
  /** 有回调时卡片才可点、可聚焦；没有回调时保持纯展示（测试与列表预览）。 */
  onOpen?: (item: ItemCardData) => void;
  selected?: boolean;
};

/** 只有「待解析 / 失败」用印章色强调，其余状态保持中性徽标。 */
function badgeClass(status: string): string {
  const tone = statusTone(status);
  if (tone === "pending" || tone === "failed") {
    return `badge ${tone}`;
  }
  return "badge";
}

/** 正常状态不挂徽标：每张都写「正常」只是噪声，异常才需要被看见。 */
function showsBadge(status: string): boolean {
  return statusTone(status) !== "ready";
}

/** 单张素材卡：缩略图（无图时占位）、状态徽标、复制按钮、标题与来源/时间元信息。 */
export function ItemCard({ item, onOpen, selected = false }: Props) {
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

  // 复制在飞行中禁用按钮，避免连点触发多次 IPC。
  const [copying, setCopying] = useState(false);

  const thumbSrc = item.thumbUrl !== null && !failed ? item.thumbUrl : null;
  const preview = item.preview ?? "";
  const busy = item.status === "parsing" || item.status === "pending";
  const hueStyle = { "--hue": item.hue ?? "var(--hue-ash)" } as CSSProperties;
  const interactive = onOpen !== undefined;
  const copyText = copyLabel(item.kind);

  const copy = async (event: { stopPropagation: () => void }) => {
    // 卡片本身也是按钮：复制不能顺手把详情打开。
    event.stopPropagation();
    if (copying) {
      return;
    }
    setCopying(true);
    try {
      await api.copyOriginal(item.id, item.kind);
      pushToast(`已复制${copyText.replace(/^复制/, "")}`);
    } catch (error) {
      // 与详情页同一套错误码文案：把「文件没了 / 剪贴板被占用」说清楚。
      const code = errorCode(error);
      if (code === "FILE_MISSING") {
        pushToast("原图文件缺失", "error");
      } else if (code === "CLIPBOARD_BUSY") {
        pushToast("剪贴板被占用，请重试", "error");
      } else {
        pushToast(errorMessage(error), "error");
      }
    } finally {
      setCopying(false);
    }
  };

  return (
    <article
      className={`card${busy ? " is-pending" : ""}${selected ? " selected" : ""}`}
      data-item-id={item.id}
      title={item.sourceTitle ?? undefined}
      role={interactive ? "button" : undefined}
      tabIndex={interactive ? 0 : undefined}
      aria-pressed={interactive ? selected : undefined}
      onClick={interactive ? () => onOpen(item) : undefined}
      onKeyDown={
        interactive
          ? (event) => {
              if (event.key === "Enter" || event.key === " ") {
                event.preventDefault();
                onOpen(item);
              }
            }
          : undefined
      }
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
        ) : preview.length > 0 ? (
          <p
            className={`card-text-preview${item.kind === "link" ? " is-link" : ""}`}
            title={preview}
            aria-hidden="true"
          >
            {preview}
          </p>
        ) : (
          <span className="thumb-fallback">{kind}</span>
        )}
        {busy && <span className="shimmer" />}
        {showsBadge(item.status) && (
          <span className={badgeClass(item.status)}>{statusLabel(item.status)}</span>
        )}
        <button
          type="button"
          className="card-copy"
          title={copyText}
          aria-label={copyText}
          disabled={copying}
          onClick={copy}
          onKeyDown={(event) => event.stopPropagation()}
        >
          <svg
            width="13"
            height="13"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth="1.5"
            strokeLinejoin="round"
            aria-hidden="true"
          >
            <rect x="8.6" y="8.6" width="11" height="11" rx="2.2" />
            <path
              d="M15.4 5.6A2.6 2.6 0 0012.8 3H6.6A2.6 2.6 0 004 5.6v6.2a2.6 2.6 0 002.6 2.6"
              strokeLinecap="round"
            />
          </svg>
        </button>
      </div>

      <div className="card-body">
        {item.hit !== undefined && item.hit !== null && (
          <span className={`card-hit hit-${item.hit}`} data-hit={item.hit}>
            {item.hit}
          </span>
        )}
        <h3 className="card-title">{title}</h3>
        {item.summary !== undefined && item.summary !== null && item.summary.length > 0 && (
          <p className="card-summary" title={item.summary}>
            {item.summary}
          </p>
        )}
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
