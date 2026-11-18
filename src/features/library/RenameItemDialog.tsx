import { useEffect, useRef, useState } from "react";
import { api } from "../../lib/api";
import { errorMessage } from "../../lib/errors";
import { pushToast } from "../../lib/toast";
import { TITLE_LIMIT } from "../../lib/itemText";

type Props = {
  item: { id: string; title?: string | null; sourceTitle: string | null };
  onClose: () => void;
  onSaved?: () => void;
};

/**
 * 重命名弹窗。
 *
 * 初始值优先取用户标题，否则取来源标题（用户通常只是「在原文上改两个字」）；
 * 提交时若输入为空**或与来源标题相同**，写 `null`——语义是「恢复显示来源标题」，
 * 这样以后来源标题变了，展示也会跟着变，而不是被一次保存永久冻结。
 */
export function RenameItemDialog({ item, onClose, onSaved }: Props) {
  const sourceTitle = item.sourceTitle ?? "";
  const [draft, setDraft] = useState(item.title ?? sourceTitle);
  const [saving, setSaving] = useState(false);
  const panelRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    panelRef.current?.focus();
  }, []);

  const trimmed = draft.trim();
  const willFallback = trimmed.length === 0 || trimmed === sourceTitle;

  const submit = async () => {
    if (saving) {
      return;
    }
    setSaving(true);
    try {
      await api.renameItem(item.id, willFallback ? null : trimmed);
      pushToast(willFallback ? "已恢复显示来源标题" : "标题已更新");
      onSaved?.();
      onClose();
    } catch (error) {
      setSaving(false);
      pushToast(errorMessage(error), "error");
    }
  };

  return (
    <div className="modal" onMouseDown={onClose}>
      <div
        ref={panelRef}
        className="rename-panel"
        role="dialog"
        aria-modal="true"
        aria-labelledby="rename-title"
        aria-describedby="rename-hint"
        tabIndex={-1}
        onMouseDown={(event) => event.stopPropagation()}
        onKeyDown={(event) => {
          if (event.key === "Escape") {
            event.stopPropagation();
            event.preventDefault();
            onClose();
          }
          if (event.key === "Enter" && !event.nativeEvent.isComposing) {
            event.preventDefault();
            void submit();
          }
        }}
      >
        <h2 id="rename-title">重命名素材</h2>
        <label className="rename-field">
          <span className="label">标题</span>
          <input
            type="text"
            autoFocus
            value={draft}
            maxLength={TITLE_LIMIT}
            placeholder={sourceTitle.length > 0 ? sourceTitle : "未命名素材"}
            onChange={(event) => setDraft(event.target.value.slice(0, TITLE_LIMIT))}
          />
        </label>
        <div className="rename-meta">
          <span className="rename-hint" id="rename-hint">
            {willFallback ? "将显示来源标题" : "Enter 保存 · Esc 取消"}
          </span>
          <span className="rename-count">
            {draft.length}/{TITLE_LIMIT}
          </span>
        </div>
        <div className="confirm-actions">
          <button type="button" className="btn" onClick={onClose} disabled={saving}>
            取消
          </button>
          <button type="button" className="btn primary" onClick={() => void submit()} disabled={saving}>
            {saving ? "保存中…" : "保存"}
          </button>
        </div>
      </div>
    </div>
  );
}
