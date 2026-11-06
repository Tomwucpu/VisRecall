import { useEffect, useRef, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { api } from "../../lib/api";
import { queryKeys } from "../../lib/queryClient";
import { errorMessage } from "../../lib/errors";
import { pushToast } from "../../lib/toast";

type Props = {
  onClose: () => void;
};

type PaneId = "capture" | "storage";

const PANE_META: Record<PaneId, { title: string; sub: string }> = {
  capture: { title: "捕获", sub: "快捷键 · 悬浮球" },
  storage: { title: "存储", sub: "位置 · 容量 · 保留策略" },
};

/** `Ctrl+Shift+S` → `["Ctrl","Shift","S"]`，用于渲染 kbd 芯片。 */
function splitHotkey(hotkey: string): string[] {
  return hotkey
    .split("+")
    .map((part) => part.trim())
    .filter((part) => part.length > 0);
}

const BALL_SIZES = [40, 48, 56] as const;
const RETENTION_OPTIONS = ["永久", "30 天", "90 天", "180 天", "自定义"] as const;

/**
 * 设置弹窗（T5）：只有「捕获」「存储」两个 pane。
 *
 * 原型里的模型 / 隐私 / 关于三个 pane 属于阶段 03、04 与 V0.5（规划 §7 裁剪清单），
 * 不进 DOM。本轮的边界：
 *  - 读：快捷键、悬浮球尺寸/开关、库路径都来自真实 `get_settings`；
 *  - 写：一律不可用并标注原因 —— `update_settings` 改键会直接返回
 *    `INVALID_INPUT(restart required)`，`ball_size` 需重启才生效，
 *    容量与保留策略则完全没有后端。这里宁可禁用，也不给「点了没反应」的按钮。
 */
export function SettingsDialog({ onClose }: Props) {
  const [pane, setPane] = useState<PaneId>("capture");
  const [opening, setOpening] = useState(false);
  const panelRef = useRef<HTMLDivElement>(null);

  const settings = useQuery({
    queryKey: queryKeys.settings,
    queryFn: () => api.getSettings(),
  });

  useEffect(() => {
    panelRef.current?.focus();
  }, []);

  const openLibrary = async () => {
    setOpening(true);
    try {
      await api.openLibraryFolder();
      pushToast("已在资源管理器中打开");
    } catch (error) {
      pushToast(errorMessage(error), "error");
    } finally {
      setOpening(false);
    }
  };

  const loading = settings.isPending;
  const hotkey = settings.data?.captureHotkey ?? "";
  const ballSize = settings.data?.ballSize ?? null;
  const ballEnabled = settings.data?.ballEnabled ?? false;
  const libraryPath = settings.data?.libraryPath ?? "—";

  return (
    <div className="modal" onMouseDown={onClose}>
      <div
        ref={panelRef}
        className="modal-panel"
        role="dialog"
        aria-modal="true"
        aria-labelledby="settings-title"
        tabIndex={-1}
        onMouseDown={(event) => event.stopPropagation()}
        onKeyDown={(event) => {
          if (event.key === "Escape") {
            event.stopPropagation();
            event.preventDefault();
            onClose();
          }
        }}
      >
        <div className="modal-nav">
          <span className="label">设置</span>
          {(Object.keys(PANE_META) as PaneId[]).map((id) => (
            <button
              key={id}
              type="button"
              className={`mnav${pane === id ? " on" : ""}`}
              aria-current={pane === id ? "page" : undefined}
              onClick={() => setPane(id)}
            >
              {PANE_META[id].title}
            </button>
          ))}
          <div className="grow" />
          <button type="button" className="mnav" onClick={onClose}>
            关闭
          </button>
        </div>

        <div className="modal-body">
          <div className="modal-head">
            <div style={{ display: "flex", alignItems: "baseline" }}>
              <h2 id="settings-title">{PANE_META[pane].title}</h2>
              <span className="sub">{PANE_META[pane].sub}</span>
            </div>
            <button type="button" className="icon-btn" aria-label="关闭设置" onClick={onClose}>
              <svg width="13" height="13" viewBox="0 0 14 14" stroke="currentColor" strokeWidth="1.4">
                <path d="M2 2l10 10M12 2L2 12" />
              </svg>
            </button>
          </div>

          <div className="modal-content">
            {pane === "capture" ? (
              <div className="pane">
                <p className="pane-desc">
                  捕获只有一个动作：框选，或松手。所有分类与打标都由系统完成。
                </p>

                <div className="row">
                  <div>
                    <div className="row-t">区域截屏快捷键</div>
                    <div className="row-d">与点击悬浮球完全等价，任一可独立使用。</div>
                  </div>
                  <div className="row-r">
                    <div className="kbd-set">
                      {loading ? (
                        <span className="mono row-d">读取中…</span>
                      ) : (
                        splitHotkey(hotkey).map((key) => <kbd key={key}>{key}</kbd>)
                      )}
                    </div>
                    <button
                      type="button"
                      className="mini-btn"
                      disabled
                      title="阶段 02 提供：改动快捷键需要重新注册热键"
                    >
                      修改
                    </button>
                  </div>
                </div>

                <div className="row">
                  <div>
                    <div className="row-t">悬浮球</div>
                    <div className="row-d">
                      始终置顶但不抢输入焦点。尺寸与显示开关的读写待阶段 02 提供，这里只反映当前配置。
                    </div>
                  </div>
                  <div className="row-r">
                    <div className="seg" role="group" aria-label="悬浮球直径">
                      {BALL_SIZES.map((size) => (
                        <button
                          key={size}
                          type="button"
                          className={ballSize === size ? "on" : undefined}
                          disabled
                          title="阶段 02 提供"
                        >
                          {size}
                        </button>
                      ))}
                    </div>
                    <button
                      type="button"
                      className={`switch${ballEnabled ? " on" : ""}`}
                      role="switch"
                      aria-checked={ballEnabled}
                      aria-label="悬浮球显示"
                      disabled
                      title="阶段 02 提供"
                    >
                      <i />
                    </button>
                  </div>
                </div>
              </div>
            ) : (
              <div className="pane">
                <p className="pane-desc">
                  库文件、原图与索引均以明文存储在你可见的普通文件夹中，路径随时可以打开或更换。
                </p>

                <div className="row">
                  <div style={{ minWidth: 0 }}>
                    <div className="row-t">存储位置</div>
                    <div className="row-d mono" style={{ fontSize: 10 }} title={libraryPath}>
                      {libraryPath}
                    </div>
                  </div>
                  <div className="row-r">
                    <button
                      type="button"
                      className="mini-btn"
                      disabled={opening}
                      onClick={() => void openLibrary()}
                    >
                      {opening ? "打开中…" : "打开"}
                    </button>
                    <button
                      type="button"
                      className="mini-btn"
                      disabled
                      title="阶段 02 提供：位置迁移需要进度事件"
                    >
                      更换
                    </button>
                  </div>
                </div>

                <div className="row">
                  <div>
                    <div className="row-t">已用空间</div>
                    <div className="row-d">用量统计待阶段 02 提供。</div>
                  </div>
                  <div className="row-r">
                    <span className="mono row-d" style={{ color: "var(--ink-3)" }}>
                      —
                    </span>
                  </div>
                </div>

                <div className="row">
                  <div>
                    <div className="row-t">空间容量</div>
                    <div className="row-d">默认 50 GB，可设为 1 – 4096 GB；容量设置待阶段 02 提供。</div>
                  </div>
                  <div className="row-r">
                    <input
                      className="field"
                      style={{ width: 66, minWidth: 0, textAlign: "center" }}
                      type="text"
                      value="—"
                      readOnly
                      disabled
                      aria-label="空间容量（GB）"
                    />
                    <span className="mono row-d">GB</span>
                  </div>
                </div>

                <div className="row">
                  <div>
                    <div className="row-t">保留策略</div>
                    <div className="row-d">
                      默认永久保留，不主动打扰。修改保留天数待阶段 02 提供，当前不会自动删除任何素材。
                    </div>
                  </div>
                  <div className="row-r">
                    <div className="seg" role="group" aria-label="保留策略">
                      {RETENTION_OPTIONS.map((label, index) => (
                        <button
                          key={label}
                          type="button"
                          className={index === 0 ? "on" : undefined}
                          disabled
                          title="阶段 02 提供"
                        >
                          {label}
                        </button>
                      ))}
                    </div>
                  </div>
                </div>
              </div>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
