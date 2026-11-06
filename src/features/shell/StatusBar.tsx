type Props = {
  /** 与标题栏同一份库状态文案。 */
  status: string;
  /** 后端版本号；读取中为 `null`。 */
  version: string | null;
};

/**
 * 状态栏：左侧库状态，右侧只列**已实现**的快捷键。
 * 原型里的 `↑↓ / Enter` 属 T7 的列表导航（本轮未做），`Ctrl K` 属阶段 03 搜索，
 * 两者都不出现，避免给出按了没反应的提示。
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
          <kbd>B</kbd> 侧栏
        </span>
        <span>
          <kbd>T</kbd> 主题
        </span>
      </div>
    </footer>
  );
}
