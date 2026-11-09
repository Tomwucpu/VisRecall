import { useEffect, useRef, useState } from "react";

import {
  PROTOCOLS,
  protocolBaseUrl,
  protocolLabel,
  type ModelDraft,
} from "../../lib/modelConfig";

type Props = {
  mode: "new" | "edit";
  draft: ModelDraft;
  error: string | null;
  busy: boolean;
  onChange: (patch: Partial<ModelDraft>) => void;
  onCancel: () => void;
  onSave: () => void;
};

/** 原型 `.custom-model`：新建 / 修改模型的表单，协议下拉为真实菜单。 */
export function ModelForm({
  mode,
  draft,
  error,
  busy,
  onChange,
  onCancel,
  onSave,
}: Props) {
  const [protoOpen, setProtoOpen] = useState(false);
  const wrapRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!protoOpen) {
      return;
    }
    const onPointerDown = (event: Event) => {
      if (wrapRef.current && !wrapRef.current.contains(event.target as Node)) {
        setProtoOpen(false);
      }
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        setProtoOpen(false);
      }
    };
    document.addEventListener("pointerdown", onPointerDown);
    document.addEventListener("keydown", onKeyDown);
    return () => {
      document.removeEventListener("pointerdown", onPointerDown);
      document.removeEventListener("keydown", onKeyDown);
    };
  }, [protoOpen]);

  return (
    <section className="custom-model show" aria-label={mode === "new" ? "新建模型" : "修改模型"}>
      <div className="cm-head">
        <span className="label">{mode === "new" ? "新建模型" : "修改模型"}</span>
        <button type="button" className="mini-btn" onClick={onCancel}>
          收起
        </button>
      </div>

      <div className="cm-grid">
        <label className="cm-field">
          <span>显示名称</span>
          <input
            className="field"
            aria-label="显示名称"
            placeholder="我的视觉模型"
            value={draft.name}
            onChange={(event) => onChange({ name: event.target.value })}
          />
        </label>
        <label className="cm-field">
          <span>模型 ID</span>
          <input
            className="field"
            aria-label="模型 ID"
            placeholder="qwen2.5-vl-72b"
            spellCheck={false}
            value={draft.model}
            onChange={(event) => onChange({ model: event.target.value })}
          />
        </label>
      </div>

      <div className="cm-grid cm-grid-gap">
        <label className="cm-field">
          <span>接口地址 / Base URL</span>
          <input
            className="field"
            aria-label="接口地址 / Base URL"
            placeholder={protocolBaseUrl(draft.protocol) || "https://your-endpoint/v1"}
            spellCheck={false}
            value={draft.baseUrl}
            onChange={(event) => onChange({ baseUrl: event.target.value })}
          />
        </label>
        <div className="cm-field">
          <span>通信协议</span>
          <div className={`proto-wrap${protoOpen ? " open" : ""}`} ref={wrapRef}>
            <button
              type="button"
              className="proto-btn"
              aria-haspopup="menu"
              aria-expanded={protoOpen}
              onClick={() => setProtoOpen((open) => !open)}
            >
              <span>{protocolLabel(draft.protocol)}</span>
              <svg className="chev" width="9" height="9" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.3">
                <path d="M2 4l3 3 3-3" strokeLinecap="round" />
              </svg>
            </button>
            {protoOpen && (
              <div className="proto-menu open" role="menu" aria-label="通信协议">
                {PROTOCOLS.map((item) => (
                  <button
                    key={item.key}
                    type="button"
                    role="menuitemradio"
                    aria-checked={draft.protocol === item.key}
                    className={`proto-opt${draft.protocol === item.key ? " on" : ""}`}
                    onClick={() => {
                      onChange({ protocol: item.key });
                      setProtoOpen(false);
                    }}
                  >
                    {item.label}
                    <i className="tick">✓</i>
                  </button>
                ))}
              </div>
            )}
          </div>
        </div>
      </div>

      {draft.protocol === "rest" && (
        <div className="cm-proto-row">
          <span className="cm-k">协议标识</span>
          <input
            className="field cm-proto-id"
            aria-label="协议标识"
            placeholder="如 my-proto/v1"
            value={draft.protocolId}
            spellCheck={false}
            onChange={(event) => onChange({ protocolId: event.target.value })}
          />
          <span className="cm-k">按此标识构造请求路径，报文沿用 chat/completions</span>
        </div>
      )}

      <div className="cm-grid cm-grid-gap">
        <label className="cm-field">
          <span>API 密钥</span>
          <input
            className="field"
            type="password"
            aria-label="API 密钥"
            placeholder={draft.hasApiKey ? "已保存 · 留空不修改" : "留空表示不需要密钥"}
            autoComplete="off"
            disabled={draft.clearApiKey}
            value={draft.apiKey}
            onChange={(event) => onChange({ apiKey: event.target.value })}
          />
        </label>
        <div className="cm-field">
          <span>密钥存放</span>
          <div className="cm-note">
            仅保存在 Windows 凭据管理器，列表中不会明文显示，也不会写进配置文件。
          </div>
          {draft.hasApiKey && (
            <button
              type="button"
              className="mini-btn cm-key-clear"
              onClick={() => onChange({ clearApiKey: !draft.clearApiKey, apiKey: "" })}
            >
              {draft.clearApiKey ? "撤销清除" : "清除已保存密钥"}
            </button>
          )}
        </div>
      </div>

      <div className="cm-actions">
        <span className="cm-msg" role={error !== null ? "alert" : undefined}>
          {error ?? ""}
        </span>
        <button type="button" className="cm-btn ghost" disabled={busy} onClick={onCancel}>
          取消
        </button>
        <button type="button" className="cm-btn primary" disabled={busy} onClick={onSave}>
          {busy ? "保存中…" : mode === "new" ? "保存并启用" : "保存修改"}
        </button>
      </div>
    </section>
  );
}
