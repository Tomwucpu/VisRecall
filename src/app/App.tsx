import { useCallback, useEffect, useState } from "react";
import { commands, type AppStatus } from "../lib/bindings";
import { subscribeItemEvents } from "../lib/events";
import { EmptyState } from "../features/library/EmptyState";
import { ItemGrid } from "../features/library/ItemGrid";
import type { ItemCardData } from "../features/library/ItemCard";
import { useItems } from "../features/library/useItems";
import { Sidebar } from "../features/shell/Sidebar";
import { StatusBar } from "../features/shell/StatusBar";
import { TitleBar } from "../features/shell/TitleBar";

type LoadState =
  | { status: "loading" }
  | { status: "ready"; app: AppStatus }
  | { status: "error"; message: string };

/** 命令错误统一为 `{ code, message }`；未知错误兜底成字符串。 */
function describeError(error: unknown): string {
  if (typeof error === "object" && error !== null) {
    const { code, message } = error as { code?: unknown; message?: unknown };
    if (typeof message === "string") {
      return typeof code === "string" ? `${code}: ${message}` : message;
    }
  }
  return error instanceof Error ? error.message : String(error);
}

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

  // 事件失效会整段重取；按 id 去重，避免重复键。
  const loaded = new Map<string, ItemCardData>();
  for (const page of items.data?.pages ?? []) {
    for (const item of page) {
      loaded.set(item.id, item);
    }
  }
  const list = [...loaded.values()];

  const status = statusText(state);
  const countText = items.isPending ? "读取中…" : `找到 ${list.length} 项`;

  return (
    <div className="app">
      <TitleBar status={status} itemCount={items.isPending ? null : list.length} />

      <div className="workspace">
        <Sidebar />

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
              />
            )}
          </div>
        </main>

        {/* 详情列：T4 接入真实内容，当前只保留列与空态。 */}
        <aside className="detail">
          <div className="detail-head">
            <span className="label">详情</span>
            <span className="label">—</span>
          </div>
          <div className="detail-empty">
            <div className="ring">
              <svg
                width="15"
                height="15"
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                strokeWidth="1.4"
              >
                <path d="M5 12h14" strokeLinecap="round" />
              </svg>
            </div>
            <span className="detail-empty-text">从中间选一条素材</span>
          </div>
        </aside>
      </div>

      <StatusBar status={status} version={state.status === "ready" ? state.app.version : null} />
    </div>
  );
}
