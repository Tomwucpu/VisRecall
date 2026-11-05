import { useCallback, useEffect, useState } from "react";
import { commands, type AppStatus } from "../lib/bindings";
import { subscribeItemEvents } from "../lib/events";
import { EmptyState } from "../features/library/EmptyState";
import { ItemGrid } from "../features/library/ItemGrid";
import type { ItemCardData } from "../features/library/ItemCard";
import { useItems } from "../features/library/useItems";

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

export default function App() {
  const [state, setState] = useState<LoadState>({ status: "loading" });
  const [opening, setOpening] = useState(false);
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

  const openLibraryFolder = async () => {
    setOpening(true);
    try {
      await commands.openLibraryFolder();
    } catch (error) {
      setState({ status: "error", message: describeError(error) });
    } finally {
      setOpening(false);
    }
  };

  const refreshAll = () => {
    void load();
    void items.refetch();
  };

  // 事件失效会整段重取；按 id 去重，避免重复键。
  const loaded = new Map<string, ItemCardData>();
  for (const page of items.data?.pages ?? []) {
    for (const item of page) {
      loaded.set(item.id, item);
    }
  }
  const list = [...loaded.values()];

  const busy = items.isFetching && !items.isFetchingNextPage;
  const countText = list.length > 0 ? `已加载 ${list.length} 条` : "";

  return (
    <main className="app-shell">
      <header className="app-header">
        <div>
          <h1>目记 VisRecall</h1>
          <p className="app-subtitle">捕获闭环 · 阶段 01</p>
        </div>
        <div className="app-actions">
          <button type="button" disabled={opening} onClick={() => void openLibraryFolder()}>
            {opening ? "正在打开…" : "打开库目录"}
          </button>
          <button type="button" onClick={refreshAll}>
            刷新
          </button>
        </div>
      </header>

      {state.status === "loading" && <p className="app-hint">正在加载应用状态…</p>}

      {state.status === "error" && (
        <section className="app-card app-card-error" role="alert">
          <h2>无法读取应用状态</h2>
          <p>{state.message}</p>
          <button type="button" onClick={() => void load()}>
            重试
          </button>
        </section>
      )}

      {state.status === "ready" && (
        <dl className="app-meta app-meta-inline">
          <dt>版本</dt>
          <dd>{state.app.version}</dd>
          <dt>库路径</dt>
          <dd title={state.app.libraryPath}>{state.app.libraryPath}</dd>
          <dt>库状态</dt>
          <dd className={state.app.libraryOk ? "app-ok" : "app-bad"}>
            {state.app.libraryOk ? "正常" : "无法打开"}
          </dd>
        </dl>
      )}

      <section className="library">
        <div className="library-toolbar">
          <h2>素材</h2>
          {countText && <span className="library-count">{countText}</span>}
          {busy && <span className="library-refreshing">更新中…</span>}
        </div>

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
      </section>

      <footer className="app-footer">关闭窗口后应用仍在托盘运行。</footer>
    </main>
  );
}
