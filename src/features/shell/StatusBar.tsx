type Props = {
  /** 与标题栏同一份库状态文案。 */
  status: string;
  /** 后端版本号；读取中为 `null`。 */
  version: string | null;
};

/**
 * 状态栏：左侧库状态，右侧只列**已实现**的快捷键。
 * 原型里的 `↑↓ / Enter / Ctrl K / B` 分别属于 T7、T6 与阶段 03，未实现前不出现，
 * 避免出现按了没反应的假提示。
 */
export function StatusBar({ status, version }: Props) {
  return (
    <footer className="statusbar">
      <div className="status-left">
        <span className="live" />
        <span>{status}</span>
      </div>
      <div className="status-right">
        {version !== null && <span>v{version}</span>}
        <span>
          <kbd>T</kbd> 主题
        </span>
      </div>
    </footer>
  );
}
