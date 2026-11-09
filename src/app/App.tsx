import { useCallback, useEffect, useMemo, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { commands, type AppStatus } from "../lib/bindings";
import { api, type MigrationProgress } from "../lib/api";
import { queryKeys } from "../lib/queryClient";
import { describeError } from "../lib/errors";
import { pushToast } from "../lib/toast";
import { toggleTheme } from "../lib/theme";
import { subscribeAppEvents } from "../lib/events";
import { subscribeNotifications } from "../lib/notifications";
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
import { usePipelineState } from "../features/shell/usePipelineState";
import { SearchBar } from "../features/search/SearchBar";
import { QueryChips } from "../features/search/QueryChips";
import { stripChip } from "../lib/searchTypes";
import { ResultGrid } from "../features/search/ResultGrid";
import { useSearch } from "../features/search/useSearch";

type LoadState =
  | { status: "loading" }
  | { status: "ready"; app: AppStatus }
  | { status: "error"; message: string };

/** 迁移阶段 → 横幅文案（后端 phase 取值固定为这四个）。 */
const MIGRATION_PHASE: Record<string, string> = {
  prepare: "准备",
  copy: "复制文件",
  verify: "校验",
  done: "完成",
};

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
  const [selectedId, setSelectedId] = useState<string | null>(null);
  // 默认不显示详情列，选中素材后滑入，关闭后整列滑出（原型 `.detail.is-hidden`）。
  const [detailHidden, setDetailHidden] = useState(true);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [sidebarCollapsed, setSidebarCollapsed] = useState(false);
  const [restarting, setRestarting] = useState(false);
  const [migration, setMigration] = useState<MigrationProgress | null>(null);
  // 阶段 03：搜索模式。空查询 = 普通列表，非空 = 检索结果（两套数据源不混用）。
  const [query, setQuery] = useState("");
  const items = useItems();
  const search = useSearch(query);
  const pipeline = usePipelineState();
  const searching = query.trim().length > 0;

  // 库总量只在用量查询里（`itemCount` 来自 count_items），列表只统计已加载页。
  const usageQuery = useQuery({
    queryKey: queryKeys.usage,
    queryFn: () => api.getStorageUsage(),
    staleTime: 30_000,
  });

  const closeDetail = useCallback(() => {
    setSelectedId(null);
    setDetailHidden(true);
  }, []);

  const openDetail = useCallback((next: ItemCardData) => {
    setSelectedId(next.id);
    setDetailHidden(false);
  }, []);

  // 应用状态走查询缓存：迁移后「待重启」横幅要能在刷新界面后仍然出现，
  // 因此它必须来自可重取的查询，而不是一次性的组件内 state。
  // 状态没有专属事件（库可用性是启动期快照），回到窗口时重取一次兜底。
  const statusQuery = useQuery({
    queryKey: queryKeys.status,
    queryFn: api.getStatus,
    refetchOnWindowFocus: true,
  });

  const state: LoadState = useMemo(() => {
    if (statusQuery.isPending) {
      return { status: "loading" };
    }
    if (statusQuery.isError) {
      return { status: "error", message: describeError(statusQuery.error) };
    }
    return { status: "ready", app: statusQuery.data };
  }, [statusQuery.isPending, statusQuery.isError, statusQuery.error, statusQuery.data]);

  const load = useCallback(async () => {
    await statusQuery.refetch();
  }, [statusQuery]);

  // 数据变化与后台通知各走一条订阅：前者失效查询，后者只弹提示（StrictMode 安全）。
  useEffect(() => subscribeAppEvents(), []);
  useEffect(() => subscribeNotifications(), []);

  // 迁移进度常驻在标题栏下方：关掉设置弹窗也看得见当前阶段。
  useEffect(() => {
    return api.subscribeMigration((progress) => {
      setMigration(progress.phase === "done" ? null : progress);
    });
  }, []);

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
        closeDetail();
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
  }, [settingsOpen, closeDetail]);

  // 库已迁移、待重启：横幅常驻，直到用户重启进程（闸门不持久化，重启后自动消失）。
  const restartRequired = statusQuery.data?.restartRequired === true;
  const restart = useCallback(async () => {
    if (restarting) {
      return;
    }
    setRestarting(true);
    try {
      await commands.restartApp();
    } catch (error) {
      setRestarting(false);
      pushToast(describeError(error), "error");
    }
  }, [restarting]);

  const status = statusText(state);
  // 总数优先用后端 count（覆盖全部页），用量还没回来时退回已加载条数。
  const totalCount = usageQuery.data?.itemCount ?? null;
  const shownCount = totalCount ?? (items.isPending ? null : list.length);
  const countText = shownCount === null ? "读取中…" : `找到 ${shownCount} 项`;
  const selected = list.find((item) => item.id === selectedId) ?? null;
  const migrationPercent =
    migration !== null && migration.total > 0
      ? Math.min(100, Math.round((migration.done / migration.total) * 100))
      : null;

  return (
    <div className="app">
      <TitleBar
        status={status}
        itemCount={shownCount}
        onOpenSettings={() => setSettingsOpen(true)}
      />

      {migration !== null && (
        <div className="migration-banner" role="status">
          <span>
            正在迁移库位置：{MIGRATION_PHASE[migration.phase] ?? migration.phase}
            {migrationPercent === null ? "…" : ` · ${migrationPercent}%`}
          </span>
          <div className="migration-bar" aria-hidden="true">
            <i style={{ width: `${migrationPercent ?? 8}%` }} />
          </div>
        </div>
      )}

      {restartRequired && (
        <div className="migration-banner" role="status">
          <span>
            库已迁移到 {statusQuery.data?.libraryPath}，重启后生效；重启前捕获已暂停。
          </span>
          <button
            type="button"
            className="mini-btn"
            onClick={() => void restart()}
            disabled={restarting}
          >
            {restarting ? "正在重启…" : "立即重启"}
          </button>
        </div>
      )}

      <div className="workspace">
        <Sidebar
          collapsed={sidebarCollapsed}
          onToggle={() => setSidebarCollapsed((value) => !value)}
          itemCount={shownCount}
        />

        <main className="main">
          <div className="main-head">
            <div className="crumbs">
              <span>素材库</span>
              <span className="sep">/</span>
              <span className="here">{searching ? "检索" : "全部素材"}</span>
            </div>
            <SearchBar
              value={query}
              onChange={setQuery}
              busy={search.busy}
              semanticReady={pipeline.semanticReady}
            />
            <div className="result-bar">
              <div className="result-title">
                <h1>{searching ? "检索结果" : "全部素材"}</h1>
                <span className="n">
                  {searching
                    ? search.status === "ready"
                      ? `命中 ${search.response?.total ?? 0} 项`
                      : "检索中…"
                    : countText}
                </span>
              </div>
            </div>
            {searching && search.response !== null && (
              <QueryChips
                parsed={search.response.parsed}
                onRemove={(chip) => setQuery(stripChip(query, chip))}
              />
            )}
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

            {searching ? (
              <ResultGrid
                hits={search.hits}
                status={search.status}
                errorMessage={search.error}
                semanticReady={pipeline.semanticReady}
                total={search.response?.total ?? 0}
                onOpen={openDetail}
                onNotThis={(hit) => void search.feedback(hit)}
                selectedId={selectedId}
              />
            ) : !items.isPending && !items.isError && list.length === 0 ? (
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
                onOpen={openDetail}
                selectedId={selectedId}
              />
            )}
          </div>
        </main>

        <aside
          className={`detail${detailHidden ? " is-hidden" : ""}`}
          aria-hidden={detailHidden}
          inert={detailHidden}
        >
          {selected !== null ? (
            <DetailPanel item={selected} onClose={closeDetail} />
          ) : (
            <DetailEmpty />
          )}
        </aside>
      </div>

      <StatusBar
        status={status}
        version={state.status === "ready" ? state.app.version : null}
        pipeline={pipeline}
      />
      {settingsOpen && <SettingsDialog onClose={() => setSettingsOpen(false)} />}
      <ToastViewport />
    </div>
  );
}
