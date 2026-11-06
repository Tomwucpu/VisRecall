import { useEffect, useRef } from "react";
import { kindLabel } from "../../lib/format";

type Props = {
  /** 原图地址；stub 阶段恒为 `null`，此时退到缩略图并明确标注。 */
  originalUrl: string | null;
  thumbUrl: string | null;
  title: string;
  kind: string;
  onClose: () => void;
};

/**
 * 应用内大图覆盖层（D-UI-4）。
 *
 * 原图协议 `visrecall://original/<id>` 属阶段 02，当前 `originalUrl` 恒为 null
 * （api.ts stub）。这时显示真实缩略图并注明「原图待阶段 02 提供」——
 * 不放大系统看图程序，也不假装缩略图就是原图。
 */
export function ImageOverlay({ originalUrl, thumbUrl, title, kind, onClose }: Props) {
  const innerRef = useRef<HTMLDivElement>(null);
  const src = originalUrl ?? thumbUrl;

  useEffect(() => {
    innerRef.current?.focus();
  }, []);

  return (
    <div className="preview" onMouseDown={onClose}>
      <div
        ref={innerRef}
        className="preview-inner"
        role="dialog"
        aria-modal="true"
        aria-label="查看原图"
        tabIndex={-1}
        onMouseDown={(event) => event.stopPropagation()}
        onKeyDown={(event) => {
          if (event.key === "Escape") {
            event.stopPropagation();
            event.preventDefault();
            onClose();
          }
        }}
      >
        <div className="preview-thumb">
          {src !== null ? (
            <img src={src} alt={`${kindLabel(kind)}素材原图`} draggable={false} />
          ) : (
            <span className="thumb-fallback">{kindLabel(kind)}</span>
          )}
        </div>
        <div className="preview-bar">
          <span className="mono">{title}</span>
          <span className="mono">{originalUrl === null ? "缩略图 · 原图待阶段 02 提供" : "原图"}</span>
          <span className="grow" />
          <span className="mono">Esc 关闭</span>
          <button type="button" className="preview-close" aria-label="关闭预览" onClick={onClose}>
            <svg width="11" height="11" viewBox="0 0 12 12" stroke="currentColor" strokeWidth="1.4">
              <path d="M2.5 2.5l7 7M9.5 2.5l-7 7" />
            </svg>
          </button>
        </div>
      </div>
    </div>
  );
}
