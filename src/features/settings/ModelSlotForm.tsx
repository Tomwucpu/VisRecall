import type { ProbeOutcomeDto } from "../../lib/api";
import type { SlotDraft } from "../../lib/modelConfig";

type Props = {
  label: string;
  draft: SlotDraft;
  error: string | null;
  probe: ProbeOutcomeDto | null;
  probing: boolean;
  onChange: (patch: Partial<SlotDraft>) => void;
  onProbe: () => void;
};

/** 探测结果的三态文案（成功 / 配置或连接失败 / 未探测）。 */
function probeText(probe: ProbeOutcomeDto | null, probing: boolean): string {
  if (probing) {
    return "探测中…";
  }
  if (probe === null) {
    return "";
  }
  if (!probe.ok) {
    return `探测失败：${probe.message ?? "连接不可用"}`;
  }
  return `连通 ${probe.latencyMs}ms · ${probe.vision ? "视觉可用" : "仅文本"}`;
}

/** 主/备槽位表单；密码框是「只写」的（保存后由父组件清空）。 */
export function ModelSlotForm({
  label,
  draft,
  error,
  probe,
  probing,
  onChange,
  onProbe,
}: Props) {
  const namePrefix = label === "主模型" ? "primary" : "backup";

  return (
    <section className="row model-slot" aria-label={label}>
      <div className="row-t">
        {label}
        {draft.hasApiKey && (
          <span className="model-key-badge" title="密钥已保存在 Windows 凭据管理器">
            已设置
          </span>
        )}
      </div>
      <div className="model-fields">
        <label className="model-field">
          <span>接口地址</span>
          <input
            className="field"
            type="url"
            aria-label={`${label}接口地址`}
            placeholder="https://api.example.com/v1"
            value={draft.baseUrl}
            spellCheck={false}
            onChange={(event) => onChange({ baseUrl: event.target.value })}
          />
        </label>
        <label className="model-field">
          <span>模型名</span>
          <input
            className="field"
            aria-label={`${label}模型名`}
            placeholder="gpt-4o-mini"
            value={draft.model}
            spellCheck={false}
            onChange={(event) => onChange({ model: event.target.value })}
          />
        </label>
        <label className="model-field">
          <span>API Key</span>
          <input
            className="field"
            type="password"
            aria-label={`${label} API Key`}
            placeholder={draft.hasApiKey ? "已设置 · 留空不修改" : "留空表示不设置"}
            autoComplete="new-password"
            value={draft.apiKey}
            data-slot={namePrefix}
            onChange={(event) => onChange({ apiKey: event.target.value })}
          />
        </label>
        <div className="row-r">
          <button type="button" className="mini-btn" disabled={probing} onClick={onProbe}>
            探测
          </button>
        </div>
      </div>
      {error !== null && (
        <p className="model-error" role="alert">
          {error}
        </p>
      )}
      {probeText(probe, probing) !== "" && (
        <p
          className={`model-probe${probe !== null && !probe.ok ? " is-error" : ""}`}
          role="status"
        >
          {probeText(probe, probing)}
        </p>
      )}
    </section>
  );
}
