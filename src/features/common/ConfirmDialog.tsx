import { useEffect, useRef } from "react";

type Props = {
  title: string;
  description: string;
  confirmLabel: string;
  cancelLabel?: string;
  /** 破坏性操作（删除）用印章色确认按钮。 */
  danger?: boolean;
  onConfirm: () => void;
  onCancel: () => void;
};

/**
 * 确认对话框（T5 组件，删除流程在 T4 先用上）。
 *
 * 原型没有这个组件，结构与样式都从 `.modal` / `.btn` 既有令牌推出来，
 * 不引入新色值。Esc 与点击遮罩都等同取消；打开时把焦点移到面板内，
 * 保证 Esc 事件先落在本层（并在本层 `stopPropagation`，不会误触详情关闭）。
 */
export function ConfirmDialog({
  title,
  description,
  confirmLabel,
  cancelLabel = "取消",
  danger = false,
  onConfirm,
  onCancel,
}: Props) {
  const panelRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    panelRef.current?.focus();
  }, []);

  return (
    <div className="modal" onMouseDown={onCancel}>
      <div
        ref={panelRef}
        className="confirm-panel"
        role="dialog"
        aria-modal="true"
        aria-labelledby="confirm-title"
        aria-describedby="confirm-desc"
        tabIndex={-1}
        onMouseDown={(event) => event.stopPropagation()}
        onKeyDown={(event) => {
          if (event.key === "Escape") {
            event.stopPropagation();
            event.preventDefault();
            onCancel();
          }
        }}
      >
        <h2 id="confirm-title">{title}</h2>
        <p id="confirm-desc">{description}</p>
        <div className="confirm-actions">
          <button type="button" className="btn" onClick={onCancel}>
            {cancelLabel}
          </button>
          <button
            type="button"
            className={danger ? "btn danger" : "btn primary"}
            onClick={onConfirm}
          >
            {confirmLabel}
          </button>
        </div>
      </div>
    </div>
  );
}
