import { useId } from "react";
import type { SettingsDto } from "../../lib/api";

type StartupMode = SettingsDto["startupMode"];

type Props = {
  mode: StartupMode | undefined;
  reading: boolean;
  saving: boolean;
  onChange: (mode: StartupMode) => void;
  onRetry: () => void;
};

const OPTIONS = [
  {
    value: "silent",
    label: "静默启动",
    description: "启动后驻留系统托盘。点击托盘图标可打开主窗口。",
  },
  {
    value: "show_window",
    label: "启动时显示主窗口",
    description: "启动后自动打开主窗口，后台功能照常运行。",
  },
] as const satisfies ReadonlyArray<{ value: StartupMode; label: string; description: string }>;

/** 只展示服务端已确认的偏好；读取失败不伪装成默认选项。 */
export function StartupSettingsPane({ mode, reading, saving, onChange, onRetry }: Props) {
  const id = useId();
  const hintId = `${id}-hint`;

  return (
    <div className="pane">
      <fieldset className="startup-mode" disabled={saving} aria-describedby={hintId}>
        <legend className="row-t startup-mode-heading">启动方式</legend>
        {mode === undefined ? (
          reading ? (
            <p className="row-d" role="status">正在读取启动方式…</p>
          ) : (
            <div className="row">
              <span className="row-d" role="alert">无法读取启动方式。</span>
              <button type="button" className="btn" onClick={onRetry}>重试</button>
            </div>
          )
        ) : (
          OPTIONS.map(({ value, label, description }) => {
            const labelId = `${id}-${value}-label`;
            const descriptionId = `${id}-${value}-description`;
            return (
              <label key={value} className="row startup-mode-option">
                <input
                  type="radio"
                  name={id}
                  value={value}
                  checked={mode === value}
                  aria-labelledby={labelId}
                  aria-describedby={descriptionId}
                  onChange={() => onChange(value)}
                />
                <span className="startup-mode-copy">
                  <span id={labelId} className="row-t">{label}</span>
                  <span id={descriptionId} className="row-d">{description}</span>
                </span>
              </label>
            );
          })
        )}
      </fieldset>
      <p id={hintId} className="pane-desc startup-mode-hint">
        下次启动生效。关闭主窗口不会退出应用；如需立即验证，请从托盘退出后重新启动。
      </p>
      {saving && <p className="row-d" role="status">正在保存启动方式…</p>}
    </div>
  );
}
