import { useEffect, useRef, useState } from "react";
import { api, type FileSelectionDto } from "../../lib/api";

type Props = {
  onSelection: (selection: FileSelectionDto) => void;
};

/** 仅弹窗内这块区域注册为原生拖放目标，窗口其他位置不会接收导入。 */
export function ImportDropZone({ onSelection }: Props) {
  const zoneRef = useRef<HTMLDivElement>(null);
  const [hovered, setHovered] = useState(false);

  useEffect(() => {
    const zone = zoneRef.current;
    if (zone === null) return;

    const updateTarget = () => {
      const rect = zone.getBoundingClientRect();
      void api.setImportDropTarget({
        x: rect.left,
        y: rect.top,
        width: rect.width,
        height: rect.height,
      }).catch(() => undefined);
    };
    updateTarget();
    const observer = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(updateTarget);
    observer?.observe(zone);
    window.addEventListener("resize", updateTarget);
    const unsubscribe = api.subscribeImportDrop({ onReady: onSelection, onHover: setHovered });

    return () => {
      observer?.disconnect();
      window.removeEventListener("resize", updateTarget);
      unsubscribe();
      setHovered(false);
      void api.setImportDropTarget(null).catch(() => undefined);
    };
  }, [onSelection]);

  return (
    <div ref={zoneRef} className={`import-drop-zone${hovered ? " is-hovered" : ""}`}>
      <svg width="28" height="28" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.4" aria-hidden="true">
        <path d="M12 16V4m0 0L7.5 8.5M12 4l4.5 4.5M5 14v4.5A1.5 1.5 0 0 0 6.5 20h11a1.5 1.5 0 0 0 1.5-1.5V14" />
      </svg>
      <strong>{hovered ? "松开添加文件" : "拖拽文件到这里"}</strong>
      <span>支持图片、TXT、Markdown，每批最多 20 个文件</span>
    </div>
  );
}
