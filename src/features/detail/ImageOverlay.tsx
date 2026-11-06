import { useEffect, useRef, useState } from "react";
import { kindLabel } from "../../lib/format";

type Props = {
  /** 原图地址 `visrecall://original/<id>`；为 `null`（如文本素材）时退到缩略图。 */
  originalUrl: string | null;
  thumbUrl: string | null;
  title: string;
  kind: string;
  onClose: () => void;
};

/**
 * 应用内大图覆盖层（D-UI-4）。
 *
 * 原图走 `visrecall://original/<id>` 协议；协议 404（文件被删）时不白屏：
 * `onError` 退到缩略图并标注「原图文件缺失」——不放大系统看图程序，也不假装
 * 缩略图就是原图。
 */
export function ImageOverlay({ originalUrl, thumbUrl, title, kind, onClose }: Props) {
  const innerRef = useRef<HTMLDivElement>(null);
  // 两级降级：原图 404 → 缩略图；缩略图也 404 → 类型占位。两条都记下来，
  // 避免「原图挂了就把缩略图也一起判死」。
  const [originalFailed, setOriginalFailed] = useState(false);
  const [thumbFailed, setThumbFailed] = useState(false);

  const showingOriginal = originalUrl !== null && !originalFailed;
  const src = showingOriginal ? originalUrl : thumbFailed ? null : thumbUrl;
  const label = showingOriginal
    ? "原图"
    : originalUrl === null
      ? "缩略图"
      : "原图文件缺失 · 缩略图";

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
            <img
              src={src}
              alt={`${kindLabel(kind)}素材原图`}
              draggable={false}
              onError={() => {
                if (showingOriginal) {
                  setOriginalFailed(true);
                } else {
                  setThumbFailed(true);
                }
              }}
            />
          ) : (
            <span className="thumb-fallback">{kindLabel(kind)}</span>
          )}
        </div>
        <div className="preview-bar">
          <span className="mono">{title}</span>
          <span className="mono">{label}</span>
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
