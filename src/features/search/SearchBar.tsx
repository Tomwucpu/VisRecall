import { useEffect, useRef } from "react";

type Props = {
  value: string;
  onChange: (value: string) => void;
  /** 防抖或请求进行中：右侧显示细进度条。 */
  busy?: boolean;
  /** 语义通道是否就绪；`false` 时显示降级提示（P3-R09）。 */
  semanticReady?: boolean;
  placeholder?: string;
};

/**
 * 搜索输入框：Ctrl/Cmd + K 聚焦、Esc 清空、输入即搜（防抖在 `useSearch` 里）。
 *
 * 键盘处理刻意不冒泡到全局：`App` 的全局快捷键遇到输入框会直接返回，
 * 所以这里自己处理 Esc/Ctrl+K，避免两处抢按键。
 */
export function SearchBar({
  value,
  onChange,
  busy = false,
  semanticReady = true,
  placeholder = "搜索素材：发票 上个月 橙色 漏斗图 #工作",
}: Props) {
  const inputRef = useRef<HTMLInputElement | null>(null);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if ((event.ctrlKey || event.metaKey) && (event.key === "k" || event.key === "K")) {
        event.preventDefault();
        inputRef.current?.focus();
        inputRef.current?.select();
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, []);

  return (
    <div className="search-bar">
      <span className="search-icon" aria-hidden="true">
        ⌕
      </span>
      <input
        ref={inputRef}
        type="search"
        className="search-input"
        value={value}
        placeholder={placeholder}
        aria-label="搜索素材"
        spellCheck={false}
        autoComplete="off"
        onChange={(event) => onChange(event.target.value)}
        onKeyDown={(event) => {
          if (event.key === "Escape" && value.length > 0) {
            event.stopPropagation();
            onChange("");
          }
        }}
      />
      {value.length > 0 && (
        <button
          type="button"
          className="mini-btn search-clear"
          aria-label="清空搜索"
          onClick={() => {
            onChange("");
            inputRef.current?.focus();
          }}
        >
          清空
        </button>
      )}
      <kbd className="search-kbd" aria-hidden="true">
        Ctrl K
      </kbd>
      {busy && <span className="search-progress" role="progressbar" aria-label="正在检索" />}
      {!semanticReady && (
        <span className="search-degraded" role="status" title="语义检索未就绪，当前为字面 + 条件检索">
          语义未就绪
        </span>
      )}
    </div>
  );
}
