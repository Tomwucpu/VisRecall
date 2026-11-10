import type { KeyboardEvent } from "react";

export type TextDraft = { title: string; body: string; tags: string };

type Props = {
  draft: TextDraft;
  disabled: boolean;
  onChange: (draft: TextDraft) => void;
  onSubmit: () => void;
};

const TEXT_LIMIT = 10_000;

/** 新建文字表单；父组件保存草稿，切换子视图不会丢失。 */
export function TextImportForm({ draft, disabled, onChange, onSubmit }: Props) {
  const submitWithKeyboard = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    if (event.key === "Enter" && (event.ctrlKey || event.metaKey)) {
      event.preventDefault();
      if (!disabled && draft.body.trim().length > 0) onSubmit();
    }
  };

  return (
    <div className="import-text-form">
      <label>
        <span>标题（可选）</span>
        <input className="field" value={draft.title} maxLength={200} disabled={disabled} onChange={(event) => onChange({ ...draft, title: event.target.value })} placeholder="为这段文字命名" />
      </label>
      <label>
        <span>正文</span>
        <textarea className="field" value={draft.body} maxLength={TEXT_LIMIT} disabled={disabled} onChange={(event) => onChange({ ...draft, body: event.target.value })} onKeyDown={submitWithKeyboard} placeholder="输入要保存和检索的文字" rows={9} autoFocus />
      </label>
      <div className="import-text-meta">
        <span>{draft.body.length.toLocaleString()} / {TEXT_LIMIT.toLocaleString()}</span>
        <span>Ctrl+Enter 保存</span>
      </div>
      <label>
        <span>标签（可选，用逗号分隔）</span>
        <input className="field" value={draft.tags} disabled={disabled} onChange={(event) => onChange({ ...draft, tags: event.target.value })} placeholder="例如：灵感, 读书笔记" />
      </label>
    </div>
  );
}
