import { useEffect, useRef, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { api, type MigrationProgress, type SettingsPatch } from "../../lib/api";
import { formatBytes } from "../../lib/format";
import { queryKeys } from "../../lib/queryClient";
import { errorMessage } from "../../lib/errors";
import { pushToast } from "../../lib/toast";
import { ConfirmDialog } from "../common/ConfirmDialog";
import { ModelSettingsPane } from "./ModelSettingsPane";
import { ImportSourcesPane } from "./ImportSourcesPane";

type Props = {
  onClose: () => void;
};

type PaneId = "capture" | "sources" | "storage" | "models";

const PANE_META: Record<PaneId, { title: string; sub: string }> = {
  capture: { title: "捕获", sub: "快捷键 · 悬浮球" },
  sources: { title: "导入目录", sub: "引用 · 同步 · 重新定位" },
  storage: { title: "存储", sub: "位置 · 容量 · 保留策略" },
  models: { title: "模型", sub: "自定义模型与协议" },
};

/** `Ctrl+Shift+S` → `["Ctrl","Shift","S"]`，用于渲染 kbd 芯片。 */
function splitHotkey(hotkey: string): string[] {
  return hotkey
    .split("+")
    .map((part) => part.trim())
    .filter((part) => part.length > 0);
}

const BALL_SIZES = [40, 48, 56] as const;
const RETENTION_PRESETS = [30, 90, 180] as const;
const CAPACITY_MIN = 1;
const CAPACITY_MAX = 4096;

const MODIFIER_CODES = new Set([
  "ControlLeft",
  "ControlRight",
  "ShiftLeft",
  "ShiftRight",
  "AltLeft",
  "AltRight",
  "MetaLeft",
  "MetaRight",
]);

/** 浏览器 `KeyboardEvent.code` → 后端 `HotkeySpec` 认识的主键名。 */
function mainKeyOf(code: string): string | null {
  if (/^Key[A-Z]$/.test(code)) {
    return code.slice(3);
  }
  if (/^Digit[0-9]$/.test(code)) {
    return code.slice(5);
  }
  if (/^F([1-9]|1[0-2])$/.test(code)) {
    return code;
  }
  const named: Record<string, string> = {
    Space: "Space",
    Enter: "Enter",
    Tab: "Tab",
    ArrowUp: "Up",
    ArrowDown: "Down",
    ArrowLeft: "Left",
    ArrowRight: "Right",
    Home: "Home",
    End: "End",
    PageUp: "PageUp",
    PageDown: "PageDown",
    Insert: "Insert",
    Delete: "Delete",
    Backquote: "`",
    Minus: "-",
    Equal: "=",
    BracketLeft: "[",
    BracketRight: "]",
    Backslash: "\\",
    Semicolon: ";",
    Quote: "'",
    Comma: ",",
    Period: ".",
    Slash: "/",
  };
  return named[code] ?? null;
}

/** 拼一个后端能解析的组合键；只有修饰键或没有修饰键时返回 `null`。 */
function comboOf(event: React.KeyboardEvent): string | null {
  if (MODIFIER_CODES.has(event.code)) {
    return null;
  }
  const key = mainKeyOf(event.code);
  if (key === null) {
    return null;
  }
  const parts: string[] = [];
  if (event.ctrlKey) parts.push("Ctrl");
  if (event.altKey) parts.push("Alt");
  if (event.shiftKey) parts.push("Shift");
  if (event.metaKey) parts.push("Meta");
  if (parts.length === 0) {
    return null;
  }
  parts.push(key);
  return parts.join("+");
}

/**
 * 设置弹窗（T5 + 阶段 02 接线）。
 *
 * 两个 pane 的所有写控件都接真实命令：快捷键/悬浮球走 `update_settings`，
 * 容量/保留走库内 `settings` 表，清空与孤儿清理是不可恢复操作，确认词判断都在 Rust 侧。
 * 写失败一律「提示 + 回滚显示值」，不留下「界面显示已保存、实际没落盘」的状态。
 */
export function SettingsDialog({ onClose }: Props) {
  const [pane, setPane] = useState<PaneId>("capture");
  const [opening, setOpening] = useState(false);
  const [capturing, setCapturing] = useState(false);
  const [hotkeyError, setHotkeyError] = useState<string | null>(null);
  const [capacityDraft, setCapacityDraft] = useState<string | null>(null);
  const [capacityError, setCapacityError] = useState<string | null>(null);
  const [retentionPick, setRetentionPick] = useState<
    { days: number | null; itemCount: number; bytes: number } | null
  >(null);
  const [customOpen, setCustomOpen] = useState(false);
  const [customDays, setCustomDays] = useState("90");
  const [purgeOpen, setPurgeOpen] = useState(false);
  const [purgeWord, setPurgeWord] = useState("");
  const [orphanConfirm, setOrphanConfirm] = useState(false);
  const [migration, setMigration] = useState<MigrationProgress | null>(null);
  const panelRef = useRef<HTMLDivElement>(null);
  const queryClient = useQueryClient();

  const settings = useQuery({
    queryKey: queryKeys.settings,
    queryFn: () => api.getSettings(),
  });
  const usage = useQuery({
    queryKey: queryKeys.usage,
    queryFn: () => api.getStorageUsage(),
    staleTime: 30_000,
  });
  const orphans = useQuery({
    queryKey: queryKeys.orphans,
    queryFn: () => api.getOrphans(),
    staleTime: 30_000,
  });
  // 确认词与条数由后端提供（前端不硬编码）：进入存储 pane 就取一次，
  // 这样折叠区展开时按钮的可用状态已经是正确的，不需要再等一轮请求。
  // 条数随列表变化（事件会失效该键），因此不设 staleTime，避免显示过期的条数。
  const purgePreview = useQuery({
    queryKey: queryKeys.purgePreview,
    queryFn: () => api.getPurgePreview(),
    enabled: pane === "storage",
    staleTime: 0,
  });

  useEffect(() => {
    panelRef.current?.focus();
  }, []);

  const loading = settings.isPending;
  const hotkey = settings.data?.captureHotkey ?? "";
  const ballSize = settings.data?.ballSize ?? null;
  const ballEnabled = settings.data?.ballEnabled ?? false;
  const edgeHide = settings.data?.ballEdgeHide ?? false;
  const hideInFullscreen = settings.data?.hideInFullscreen ?? false;
  const libraryPath = settings.data?.libraryPath ?? "—";
  const capacityGb = settings.data?.capacityGb ?? null;
  const retentionDays = settings.data?.retentionDays ?? null;

  const refreshUsage = () => {
    void queryClient.invalidateQueries({ queryKey: queryKeys.usage });
  };

  /** 统一的设置写入口：失败时提示并保持旧显示值（不做乐观更新）。 */
  const patchSettings = async (patch: SettingsPatch, what: string): Promise<boolean> => {
    try {
      const next = await api.updateSettings(patch);
      queryClient.setQueryData(queryKeys.settings, next);
      return true;
    } catch (error) {
      pushToast(`${what}失败：${errorMessage(error)}`, "error");
      void queryClient.invalidateQueries({ queryKey: queryKeys.settings });
      return false;
    }
  };

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

  const submitHotkey = async (combo: string) => {
    setHotkeyError(null);
    try {
      const next = await api.updateSettings({ captureHotkey: combo });
      queryClient.setQueryData(queryKeys.settings, next);
      setCapturing(false);
      pushToast("快捷键已更新");
    } catch (error) {
      // 解析失败或被占用：保留输入态，把后端的说明原样显示在行内。
      setHotkeyError(errorMessage(error));
    }
  };

  const submitCapacity = async () => {
    if (capacityDraft === null) {
      return;
    }
    const gigabytes = Number(capacityDraft.trim());
    if (!Number.isInteger(gigabytes) || gigabytes < CAPACITY_MIN || gigabytes > CAPACITY_MAX) {
      setCapacityError(`容量需为 ${CAPACITY_MIN}–${CAPACITY_MAX} GB 的整数`);
      return;
    }
    setCapacityError(null);
    try {
      await api.setCapacity(gigabytes);
      setCapacityDraft(null);
      await queryClient.invalidateQueries({ queryKey: queryKeys.settings });
      refreshUsage();
      pushToast(`容量已设为 ${gigabytes} GB`);
    } catch (error) {
      setCapacityError(errorMessage(error));
    }
  };

  const pickRetention = async (days: number | null) => {
    try {
      const preview = await api.getRetentionPreview(days);
      setRetentionPick({ days, ...preview });
    } catch (error) {
      pushToast(errorMessage(error), "error");
    }
  };

  const confirmRetention = async () => {
    if (retentionPick === null) {
      return;
    }
    const days = retentionPick.days;
    setRetentionPick(null);
    try {
      await api.setRetention(days);
      await queryClient.invalidateQueries({ queryKey: queryKeys.settings });
      pushToast(days === null ? "已设为永久保留" : `已设置保留 ${days} 天`);
    } catch (error) {
      pushToast(errorMessage(error), "error");
    }
  };

  const applyRetention = async () => {
    try {
      const report = await api.applyRetention();
      refreshUsage();
      void queryClient.invalidateQueries({ queryKey: queryKeys.items });
      void queryClient.invalidateQueries({ queryKey: ["items-filtered"] });
      pushToast(`清理完成：成功 ${report.deleted} 条，失败 ${report.failed} 条`);
    } catch (error) {
      pushToast(errorMessage(error), "error");
    }
  };

  const relocate = async () => {
    const picked = await api.pickDirectory();
    if (picked === null) {
      return;
    }
    setMigration({ phase: "prepare", done: 0, total: 0 });
    const unsubscribe = api.subscribeMigration(setMigration);
    try {
      const result = await api.relocateStorage(picked);
      pushToast(`已迁移到 ${result.newRoot}，重启后生效`);
      await queryClient.invalidateQueries({ queryKey: queryKeys.status });
    } catch (error) {
      pushToast(`迁移失败：${errorMessage(error)}`, "error");
    } finally {
      unsubscribe();
      setMigration(null);
    }
  };

  const purge = async () => {
    const word = purgePreview.data?.confirmationWord ?? "";
    if (purgeWord !== word) {
      return;
    }
    try {
      await api.purgeLibrary(purgeWord);
      setPurgeOpen(false);
      setPurgeWord("");
      refreshUsage();
      void queryClient.invalidateQueries({ queryKey: queryKeys.items });
      void queryClient.invalidateQueries({ queryKey: ["items-filtered"] });
      void queryClient.invalidateQueries({ queryKey: queryKeys.tags });
      pushToast("素材库已清空");
    } catch (error) {
      pushToast(errorMessage(error), "error");
    }
  };

  const cleanupOrphans = async () => {
    setOrphanConfirm(false);
    try {
      const stats = await api.cleanupOrphans();
      refreshUsage();
      void queryClient.invalidateQueries({ queryKey: queryKeys.orphans });
      pushToast(`已清理 ${stats.files} 个文件，释放 ${formatBytes(stats.bytes)}`);
    } catch (error) {
      pushToast(errorMessage(error), "error");
    }
  };

  const usedText =
    usage.data === undefined
      ? "—"
      : `${formatBytes(usage.data.usedBytes)} / ${formatBytes(usage.data.capacityBytes)}`;

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
            {pane === "models" ? (
              <ModelSettingsPane />
            ) : pane === "sources" ? (
              <ImportSourcesPane />
            ) : pane === "capture" ? (
              <div className="pane">
                <p className="pane-desc">
                  捕获只有一个动作：框选，或松手。所有分类与打标都由系统完成。
                </p>

                <div className="row">
                  <div>
                    <div className="row-t">区域截屏快捷键</div>
                    <div className="row-d">
                      与点击悬浮球完全等价。改完立即生效，被占用时会保留原来的键。
                    </div>
                  </div>
                  <div className="row-r">
                    <div className="kbd-set">
                      {loading ? (
                        <span className="mono row-d">读取中…</span>
                      ) : capturing ? (
                        <span className="mono row-d">按下新的组合键…</span>
                      ) : (
                        splitHotkey(hotkey).map((key) => <kbd key={key}>{key}</kbd>)
                      )}
                    </div>
                    <button
                      type="button"
                      className="mini-btn"
                      onClick={() => {
                        setHotkeyError(null);
                        setCapturing((value) => !value);
                      }}
                      onKeyDown={(event) => {
                        if (!capturing) {
                          return;
                        }
                        const combo = comboOf(event);
                        if (combo === null) {
                          return;
                        }
                        event.preventDefault();
                        void submitHotkey(combo);
                      }}
                      autoFocus={capturing}
                    >
                      {capturing ? "取消" : "修改"}
                    </button>
                  </div>
                </div>
                {hotkeyError !== null && (
                  <p className="row-d" role="alert" style={{ color: "var(--seal)" }}>
                    {hotkeyError}
                  </p>
                )}

                <div className="row">
                  <div>
                    <div className="row-t">悬浮球</div>
                    <div className="row-d">
                      始终置顶但不抢输入焦点。尺寸改动立即生效，球会留在工作区内。
                    </div>
                  </div>
                  <div className="row-r">
                    <div className="seg" role="group" aria-label="悬浮球直径">
                      {BALL_SIZES.map((size) => (
                        <button
                          key={size}
                          type="button"
                          className={ballSize === size ? "on" : undefined}
                          aria-pressed={ballSize === size}
                          onClick={() => void patchSettings({ ballSize: size }, "修改尺寸")}
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
                      onClick={() =>
                        void patchSettings({ ballEnabled: !ballEnabled }, "切换悬浮球")
                      }
                    >
                      <i />
                    </button>
                  </div>
                </div>

                <div className="row">
                  <div>
                    <div className="row-t">贴边收起</div>
                    <div className="row-d">
                      拖到屏幕边缘松手后收成细条，鼠标移入时展开。
                    </div>
                  </div>
                  <div className="row-r">
                    <button
                      type="button"
                      className={`switch${edgeHide ? " on" : ""}`}
                      role="switch"
                      aria-checked={edgeHide}
                      aria-label="贴边收起"
                      onClick={() =>
                        void patchSettings({ ballEdgeHide: !edgeHide }, "切换贴边收起")
                      }
                    >
                      <i />
                    </button>
                  </div>
                </div>

                <div className="row">
                  <div>
                    <div className="row-t">全屏时隐藏</div>
                    <div className="row-d">检测到全屏/演示/独占全屏时自动隐藏，退出后恢复。</div>
                  </div>
                  <div className="row-r">
                    <button
                      type="button"
                      className={`switch${hideInFullscreen ? " on" : ""}`}
                      role="switch"
                      aria-checked={hideInFullscreen}
                      aria-label="全屏时隐藏"
                      onClick={() =>
                        void patchSettings(
                          { hideInFullscreen: !hideInFullscreen },
                          "切换全屏隐藏",
                        )
                      }
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
                      disabled={migration !== null}
                      title="复制到新位置并校验；迁移后需要重启应用"
                      onClick={() => void relocate()}
                    >
                      {migration === null ? "更换" : "迁移中…"}
                    </button>
                  </div>
                </div>
                {migration !== null && (
                  <p className="row-d" role="status">
                    迁移中（{migration.phase}）：{migration.done} / {migration.total || "—"}
                  </p>
                )}

                <div className="row">
                  <div>
                    <div className="row-t">已用空间</div>
                    <div className="row-d">
                      含原图、原文、缩略图与数据库文件；{usage.data?.itemCount ?? "—"} 条素材。
                    </div>
                  </div>
                  <div className="row-r">
                    <span className="mono row-d" style={{ color: "var(--ink-3)" }}>
                      {usedText}
                    </span>
                  </div>
                </div>

                <div className="row">
                  <div>
                    <div className="row-t">空间容量</div>
                    <div className="row-d">
                      软配额：达到容量后拒绝新捕获，不会自动删除素材。范围 {CAPACITY_MIN} –{" "}
                      {CAPACITY_MAX} GB。
                    </div>
                  </div>
                  <div className="row-r">
                    <input
                      className="field"
                      style={{ width: 66, minWidth: 0, textAlign: "center" }}
                      type="number"
                      min={CAPACITY_MIN}
                      max={CAPACITY_MAX}
                      value={capacityDraft ?? (capacityGb === null ? "" : String(capacityGb))}
                      aria-label="空间容量（GB）"
                      onChange={(event) => setCapacityDraft(event.target.value)}
                      onBlur={() => void submitCapacity()}
                    />
                    <span className="mono row-d">GB</span>
                  </div>
                </div>
                {capacityError !== null && (
                  <p className="row-d" role="alert" style={{ color: "var(--seal)" }}>
                    {capacityError}
                  </p>
                )}

                <div className="row">
                  <div>
                    <div className="row-t">保留策略</div>
                    <div className="row-d">
                      默认永久保留。选择天数后先显示预估，确认才写入；写入不会立刻删除任何素材。
                    </div>
                  </div>
                  <div className="row-r">
                    <div className="seg" role="group" aria-label="保留策略">
                      <button
                        type="button"
                        className={retentionDays === null ? "on" : undefined}
                        aria-pressed={retentionDays === null}
                        onClick={() => void pickRetention(null)}
                      >
                        永久
                      </button>
                      {RETENTION_PRESETS.map((days) => (
                        <button
                          key={days}
                          type="button"
                          className={retentionDays === days ? "on" : undefined}
                          aria-pressed={retentionDays === days}
                          onClick={() => void pickRetention(days)}
                        >
                          {days} 天
                        </button>
                      ))}
                      <button
                        type="button"
                        className={customOpen ? "on" : undefined}
                        onClick={() => setCustomOpen((value) => !value)}
                      >
                        自定义
                      </button>
                    </div>
                    <button type="button" className="mini-btn" onClick={() => void applyRetention()}>
                      立即清理
                    </button>
                  </div>
                </div>
                {customOpen && (
                  <div className="row">
                    <div>
                      <div className="row-t">自定义天数</div>
                      <div className="row-d">1 – 3650 天。</div>
                    </div>
                    <div className="row-r">
                      <input
                        className="field"
                        style={{ width: 66, minWidth: 0, textAlign: "center" }}
                        type="number"
                        min={1}
                        max={3650}
                        value={customDays}
                        aria-label="自定义保留天数"
                        onChange={(event) => setCustomDays(event.target.value)}
                      />
                      <button
                        type="button"
                        className="mini-btn"
                        onClick={() => {
                          const days = Number(customDays.trim());
                          if (!Number.isInteger(days) || days < 1 || days > 3650) {
                            pushToast("保留天数需为 1–3650 的整数", "error");
                            return;
                          }
                          void pickRetention(days);
                        }}
                      >
                        应用
                      </button>
                    </div>
                  </div>
                )}

                <div className="row">
                  <div>
                    <div className="row-t">孤儿文件</div>
                    <div className="row-d">
                      未被任何素材引用的原图/原文；缩略图缺失{" "}
                      {orphans.data?.missingThumbs ?? "—"} 条（可重建，不提供删除）。
                    </div>
                  </div>
                  <div className="row-r">
                    <span className="mono row-d" style={{ color: "var(--ink-3)" }}>
                      {orphans.data === undefined
                        ? "—"
                        : `${orphans.data.files} 个 / ${formatBytes(orphans.data.bytes)}`}
                    </span>
                    <button
                      type="button"
                      className="mini-btn"
                      disabled={(orphans.data?.files ?? 0) === 0}
                      onClick={() => setOrphanConfirm(true)}
                    >
                      清理孤儿文件
                    </button>
                  </div>
                </div>

                <p className="row-d">
                  库目录下的 visrecall.db / visrecall.db-wal / visrecall.db-shm 属于同一个数据库，
                  请勿单独删除或移动；原图与文本位于 originals/ 与 texts/，可直接用系统工具查看。
                </p>

                <details
                  className="danger-zone"
                  open={purgeOpen}
                  onToggle={(event) => setPurgeOpen((event.target as HTMLDetailsElement).open)}
                >
                  <summary>清空素材库（不可恢复）</summary>
                  <p className="row-d">
                    将删除全部 {purgePreview.data?.itemCount ?? "—"} 条素材及其原图、缩略图、标签与任务；
                    容量与保留策略会保留。
                  </p>
                  <div className="row-r">
                    <input
                      className="field"
                      placeholder={purgePreview.data?.confirmationWord ?? "删除全部"}
                      aria-label="输入确认词"
                      value={purgeWord}
                      onChange={(event) => setPurgeWord(event.target.value)}
                    />
                    <button
                      type="button"
                      className="btn danger"
                      disabled={
                        purgePreview.data === undefined ||
                        purgeWord !== purgePreview.data.confirmationWord
                      }
                      onClick={() => void purge()}
                    >
                      清空素材库
                    </button>
                  </div>
                </details>
              </div>
            )}
          </div>
        </div>
      </div>

      {retentionPick !== null && (
        <ConfirmDialog
          title={retentionPick.days === null ? "改为永久保留？" : `保留最近 ${retentionPick.days} 天？`}
          description={
            retentionPick.days === null
              ? "之后不会自动清理任何素材。"
              : `将移除 ${retentionPick.itemCount} 条 / 释放 ${formatBytes(retentionPick.bytes)}；本次只保存策略，不会立即删除。`
          }
          confirmLabel="保存策略"
          onConfirm={() => void confirmRetention()}
          onCancel={() => setRetentionPick(null)}
        />
      )}

      {orphanConfirm && (
        <ConfirmDialog
          title="清理孤儿文件？"
          description={`将删除 ${orphans.data?.files ?? 0} 个未被引用的文件（${formatBytes(
            orphans.data?.bytes ?? 0,
          )}）。数据库与 library.json 不会被触碰。`}
          confirmLabel="清理"
          danger
          onConfirm={() => void cleanupOrphans()}
          onCancel={() => setOrphanConfirm(false)}
        />
      )}
    </div>
  );
}
