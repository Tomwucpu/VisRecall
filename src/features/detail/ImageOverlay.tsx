import { useEffect, useRef, useState } from "react";
import { kindLabel } from "../../lib/format";

type Props = {
  /** 原图地址 `visrecall://original/<id>`；为 `null`（如文本素材）时退到缩略图。 */
  originalUrl: string | null;
  thumbUrl: string | null;
  title: string;
  kind: string;
  /** OCR 包围盒（归一化 0–1）：在原图上叠加高亮框（P3-R08）。 */
  boxes?: { text: string; x: number; y: number; w: number; h: number }[];
  /** 只高亮命中的词（搜索态传入 explanation.terms）；为空则高亮全部框。 */
  terms?: string[];
  onClose: () => void;
};

/**
 * 应用内大图覆盖层（D-UI-4）。
 *
 * 原图走 `visrecall://original/<id>` 协议；协议 404（文件被删）时不白屏：
 * `onError` 退到缩略图并标注「原图文件缺失」——不放大系统看图程序，也不假装
 * 缩略图就是原图。
 */
export function ImageOverlay({
  originalUrl,
  thumbUrl,
  title,
  kind,
  boxes = [],
  terms = [],
  onClose,
}: Props) {
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

  // 命中词高亮：`terms` 为空（非搜索态）时全部框都算命中，不做区分
  const matched = (box: { text: string }) =>
    terms.length === 0 || terms.some((term) => box.text.includes(term));
  const visibleBoxes = terms.length === 0 ? boxes : boxes.filter(matched);

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
            <div className="preview-stage">
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
              {/* 高亮层只在真原图上叠：缩略图尺寸与坐标不一致，画上去会错位 */}
              {showingOriginal && visibleBoxes.length > 0 && (
                <div className="ocr-overlay" aria-hidden="true">
                  {visibleBoxes.map((box, index) => (
                    <span
                      key={`${box.x}-${box.y}-${index}`}
                      className={`ocr-box${matched(box) ? " is-hit" : ""}`}
                      title={box.text}
                      style={{
                        left: `${box.x * 100}%`,
                        top: `${box.y * 100}%`,
                        width: `${box.w * 100}%`,
                        height: `${box.h * 100}%`,
                      }}
                    />
                  ))}
                </div>
              )}
            </div>
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
