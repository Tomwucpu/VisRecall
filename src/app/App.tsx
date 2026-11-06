import { useCallback, useEffect, useMemo, useState } from "react";
import { commands, type AppStatus } from "../lib/bindings";
import { describeError } from "../lib/errors";
import { toggleTheme } from "../lib/theme";
import { subscribeItemEvents } from "../lib/events";
import { ToastViewport } from "../features/shell/ToastViewport";
import { DetailEmpty, DetailPanel } from "../features/detail/DetailPanel";
import { EmptyState } from "../features/library/EmptyState";
import { ItemGrid } from "../features/library/ItemGrid";
import { SettingsDialog } from "../features/settings/SettingsDialog";
import type { ItemCardData } from "../features/library/ItemCard";
import { useItems } from "../features/library/useItems";
import { Sidebar } from "../features/shell/Sidebar";
import { StatusBar } from "../features/shell/StatusBar";
import { TitleBar } from "../features/shell/TitleBar";

type LoadState =
  | { status: "loading" }
  | { status: "ready"; app: AppStatus }
  | { status: "error"; message: string };

/** 库状态文案：标题栏与状态栏共用，保证两处不会说两套话。 */
function statusText(state: LoadState): string {
  if (state.status === "loading") {
    return "正在读取库状态";
  }
  if (state.status === "error") {
    return "库状态读取失败";
  }
  return state.app.libraryOk ? "本地索引已就绪" : "库路径不可用";
}

export default function App() {
  const [state, setState] = useState<LoadState>({ status: "loading" });
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [sidebarCollapsed, setSidebarCollapsed] = useState(false);
  const items = useItems();

  const load = useCallback(async () => {
    try {
      const app = await commands.getAppStatus();
      setState({ status: "ready", app });
    } catch (error) {
      setState({ status: "error", message: describeError(error) });
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  // 新素材/状态变化由后端事件触发缓存失效，列表自动刷新（StrictMode 安全）。
  useEffect(() => subscribeItemEvents(), []);

  // 事件失效会整段重取；按 id 去重，避免重复键。memo 化避免每次渲染都换数组引用。
  const list = useMemo(() => {
    const loaded = new Map<string, ItemCardData>();
    for (const page of items.data?.pages ?? []) {
      for (const item of page) {
        loaded.set(item.id, item);
      }
    }
    return [...loaded.values()];
  }, [items.data]);

  // 列表被事件重置后选中项可能已不存在：自动收起，避免详情停在幽灵数据上。
  useEffect(() => {
    if (selectedId !== null && list.length > 0 && !list.some((item) => item.id === selectedId)) {
      setSelectedId(null);
    }
  }, [list, selectedId]);


  // 全局键盘（T7）：Esc 收起详情、T 切主题、B 折叠侧栏。输入框内不抢按键，
  // Ctrl K 属阶段 03 的搜索，这里不绑定，也不在状态栏提示。
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.defaultPrevented || event.ctrlKey || event.metaKey || event.altKey) {
        return;
      }
      const target = event.target as HTMLElement | null;
      const typing =
        target !== null &&
        (target.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(target.tagName));

      if (event.key === "Escape") {
        // 输入态由组件自己处理 Esc（备注 flush、标签取消），这里不动。
        if (typing) {
          return;
        }
        if (settingsOpen) {
          setSettingsOpen(false);
          return;
        }
        setSelectedId(null);
        return;
      }

      if (typing) {
        return;
      }
      if (event.key === "t" || event.key === "T") {
        toggleTheme();
        return;
      }
      if (event.key === "b" || event.key === "B") {
        setSidebarCollapsed((value) => !value);
      }
    };

    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [settingsOpen]);

  const status = statusText(state);
  const countText = items.isPending ? "读取中…" : `找到 ${list.length} 项`;
  const selected = list.find((item) => item.id === selectedId) ?? null;

  return (
    <div className="app">
      <TitleBar
        status={status}
        itemCount={items.isPending ? null : list.length}
        onOpenSettings={() => setSettingsOpen(true)}
      />

      <div className="workspace">
        <Sidebar
          collapsed={sidebarCollapsed}
          onToggle={() => setSidebarCollapsed((value) => !value)}
          itemCount={items.isPending ? null : list.length}
        />

        <main className="main">
          <div className="main-head">
            <div className="crumbs">
              <span>素材库</span>
              <span className="sep">/</span>
              <span className="here">全部素材</span>
            </div>
            <div className="result-bar">
              <div className="result-title">
                <h1>全部素材</h1>
                <span className="n">{countText}</span>
              </div>
            </div>
          </div>

          <div className="grid-scroll">
            {state.status === "error" && (
              <div className="alert" role="alert">
                <span>无法读取应用状态：{state.message}</span>
                <button type="button" className="mini-btn" onClick={() => void load()}>
                  重试
                </button>
              </div>
            )}

            {!items.isPending && !items.isError && list.length === 0 ? (
              <EmptyState />
            ) : (
              <ItemGrid
                items={list}
                isPending={items.isPending}
                isError={items.isError}
                errorMessage={items.error ? describeError(items.error) : null}
                hasNextPage={items.hasNextPage}
                isFetchingNextPage={items.isFetchingNextPage}
                onLoadMore={() => void items.fetchNextPage()}
                onRetry={() => void items.refetch()}
                onOpen={(item) => setSelectedId(item.id)}
                selectedId={selectedId}
              />
            )}
          </div>
        </main>

        <aside className="detail">
          {selected !== null ? (
            <DetailPanel item={selected} onClose={() => setSelectedId(null)} />
          ) : (
            <DetailEmpty />
          )}
        </aside>
      </div>

      <StatusBar status={status} version={state.status === "ready" ? state.app.version : null} />
      {settingsOpen && <SettingsDialog onClose={() => setSettingsOpen(false)} />}
      <ToastViewport />
    </div>
  );
}
