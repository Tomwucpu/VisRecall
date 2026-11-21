import { getCurrentWindow } from "@tauri-apps/api/window";
import { useTheme } from "../../lib/theme";

/**
 * 自绘标题栏（D-UI-1）。
 *
 * 拖拽只挂在非交互容器上：Tauri 在 `mousedown` 时只检查事件目标自身是否带
 * `data-tauri-drag-region`，所以品牌区及其文字都要各自带上，否则从文字上按下
 * 无法移动窗口。按钮一律不带该属性，否则点击会被系统当成拖拽吞掉。
 * 双击最大化由拖拽区自动处理，无需额外绑定。
 */

type Props = {
  /** 打开设置弹窗（T5）。 */
  onOpenSettings: () => void;
};

type WindowAction = "minimize" | "toggleMaximize" | "close";

/** 窗口操作失败（权限缺失/非 Tauri 环境）只记录，不让界面崩掉。 */
async function runWindowAction(action: WindowAction): Promise<void> {
  try {
    await getCurrentWindow()[action]();
  } catch (error) {
    console.error(`window.${action} failed`, error);
  }
}

export function TitleBar({ onOpenSettings }: Props) {
  const { theme, toggle } = useTheme();

  return (
    <header className="titlebar" data-tauri-drag-region="">
      <div className="brand" data-tauri-drag-region="">
        <span className="brand-word" data-tauri-drag-region="">
          VISRECALL
        </span>
      </div>

      <div className="titlebar-right">
        <button
          type="button"
          className="icon-btn"
          onClick={toggle}
          title="切换主题  T"
          aria-label="切换主题"
        >
          {theme === "dark" ? (
            <svg
              width="16"
              height="16"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              strokeWidth="1.5"
            >
              <circle cx="12" cy="12" r="4.2" />
              <path
                d="M12 2.6v2.2M12 19.2v2.2M4.4 4.4l1.6 1.6M18 18l1.6 1.6M2.6 12h2.2M19.2 12h2.2M4.4 19.6L6 18M18 6l1.6-1.6"
                strokeLinecap="round"
              />
            </svg>
          ) : (
            <svg
              width="16"
              height="16"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              strokeWidth="1.5"
            >
              <path d="M20 14.5A8.5 8.5 0 019.5 4a8.5 8.5 0 1010.5 10.5z" strokeLinejoin="round" />
            </svg>
          )}
        </button>

        <button
          type="button"
          className="icon-btn"
          onClick={onOpenSettings}
          title="设置"
          aria-label="设置"
        >
          <svg
            width="16"
            height="16"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth="1.5"
            strokeLinecap="round"
          >
            <path d="M4 7h10M18 7h2M4 12h4M12 12h8M4 17h9M17 17h3" />
            <circle cx="16" cy="7" r="1.6" />
            <circle cx="10" cy="12" r="1.6" />
            <circle cx="15" cy="17" r="1.6" />
          </svg>
        </button>

        <div className="win-controls">
          <button
            type="button"
            aria-label="最小化"
            title="最小化"
            onClick={() => void runWindowAction("minimize")}
          >
            <svg width="10" height="10" viewBox="0 0 10 10">
              <path d="M1 5h8" stroke="currentColor" strokeWidth="1" />
            </svg>
          </button>
          <button
            type="button"
            aria-label="最大化"
            title="最大化"
            onClick={() => void runWindowAction("toggleMaximize")}
          >
            <svg
              width="10"
              height="10"
              viewBox="0 0 10 10"
              fill="none"
              stroke="currentColor"
              strokeWidth="1"
            >
              <rect x="1" y="1" width="8" height="8" rx="1" />
            </svg>
          </button>
          <button
            type="button"
            aria-label="关闭"
            title="关闭"
            onClick={() => void runWindowAction("close")}
          >
            <svg width="10" height="10" viewBox="0 0 10 10" stroke="currentColor" strokeWidth="1">
              <path d="M1.8 1.8l6.4 6.4M8.2 1.8l-6.4 6.4" />
            </svg>
          </button>
        </div>
      </div>
    </header>
  );
}
