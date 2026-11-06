import { getCurrentWindow } from "@tauri-apps/api/window";
import { useTheme } from "../../lib/theme";

/**
 * 自绘标题栏（D-UI-1）。
 *
 * 拖拽只挂在非交互容器上：Tauri 在 `mousedown` 时只检查事件目标自身是否带
 * `data-tauri-drag-region`，所以中间的文本行、品牌区都要各自带上，否则从文字上
 * 按下无法移动窗口。按钮一律不带该属性，否则点击会被系统当成拖拽吞掉。
 * 双击最大化由拖拽区自动处理，无需额外绑定。
 */

type Props = {
  /** 库状态的一句话描述，与状态栏共用同一份文案。 */
  status: string;
  /** 已加载素材数；`null` 表示列表尚未就绪。 */
  itemCount: number | null;
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

export function TitleBar({ status, itemCount }: Props) {
  const { theme, toggle } = useTheme();
  const countText = itemCount === null ? "素材读取中" : `${itemCount} 条素材`;

  return (
    <header className="titlebar" data-tauri-drag-region="">
      <div className="brand" data-tauri-drag-region="">
        <span className="brand-mark" data-tauri-drag-region="">
          目
        </span>
        <span className="brand-name" data-tauri-drag-region="">
          目记
          <span className="brand-latin" data-tauri-drag-region="">
            VisRecall
          </span>
        </span>
      </div>

      <div className="titlebar-mid" data-tauri-drag-region="">
        <span className="dot" data-tauri-drag-region="">
          ·
        </span>
        <span data-tauri-drag-region="">{status}</span>
        <span className="dot" data-tauri-drag-region="">
          ·
        </span>
        <span data-tauri-drag-region="">{countText}</span>
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

        <div className="win-controls">
          <button
            type="button"
            aria-label="最小化"
            title="最小化"
            onClick={() => void runWindowAction("minimize")}
          >
            <svg width="9" height="9" viewBox="0 0 10 10">
              <path d="M1 5h8" stroke="currentColor" />
            </svg>
          </button>
          <button
            type="button"
            aria-label="最大化"
            title="最大化"
            onClick={() => void runWindowAction("toggleMaximize")}
          >
            <svg
              width="8"
              height="8"
              viewBox="0 0 10 10"
              fill="none"
              stroke="currentColor"
            >
              <rect x="1.5" y="1.5" width="7" height="7" rx=".8" />
            </svg>
          </button>
          <button
            type="button"
            aria-label="关闭"
            title="关闭"
            onClick={() => void runWindowAction("close")}
          >
            <svg width="9" height="9" viewBox="0 0 10 10" stroke="currentColor">
              <path d="M2 2l6 6M8 2l-6 6" />
            </svg>
          </button>
        </div>
      </div>
    </header>
  );
}
