import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { commands, type AppStatus } from "../lib/bindings";
import { api, type MigrationProgress } from "../lib/api";
import { queryKeys } from "../lib/queryClient";
import { removeItemsEverywhere } from "../lib/itemCache";
import { describeError } from "../lib/errors";
import { pushToast } from "../lib/toast";
import { toggleTheme } from "../lib/theme";
import { subscribeAppEvents } from "../lib/events";
import { subscribeNotifications } from "../lib/notifications";
import { ToastViewport } from "../features/shell/ToastViewport";
import { DetailEmpty, DetailPanel } from "../features/detail/DetailPanel";
import { EmptyState } from "../features/library/EmptyState";
import { ItemGrid } from "../features/library/ItemGrid";
import { SelectionBar } from "../features/library/SelectionBar";
import { RenameItemDialog } from "../features/library/RenameItemDialog";
import { useSelection } from "../features/library/useSelection";
import { SettingsDialog } from "../features/settings/SettingsDialog";
import type { ItemCardData } from "../features/library/ItemCard";
import { flattenItemPages, useFilteredItems, useItems } from "../features/library/useItems";
import { Sidebar } from "../features/shell/Sidebar";
import { StatusBar } from "../features/shell/StatusBar";
import { TitleBar } from "../features/shell/TitleBar";
import { usePipelineState } from "../features/shell/usePipelineState";
import { SearchBar } from "../features/search/SearchBar";
import { QueryChips } from "../features/search/QueryChips";
import { FilterBar } from "../features/search/FilterBar";
import { ActiveFilterChips } from "../features/search/ActiveFilterChips";
import { useItemFilters } from "../features/search/useItemFilters";
import { stripChip } from "../lib/searchTypes";
import { ResultGrid } from "../features/search/ResultGrid";
import { hitToCardData } from "../features/search/hitToCard";
import { useSearch } from "../features/search/useSearch";
import { ImportDialog } from "../features/import/ImportDialog";
import { ConfirmDialog } from "../features/common/ConfirmDialog";

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

/** 单次批量删除上限：与后端建议的分批粒度一致，超过时循环分批。 */
const DELETE_BATCH_SIZE = 500;

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
  const queryClient = useQueryClient();
  const [selectedId, setSelectedId] = useState<string | null>(null);
  // 默认不显示详情列，选中素材后滑入，关闭后整列滑出（原型 `.detail.is-hidden`）。
  const [detailHidden, setDetailHidden] = useState(true);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [importOpen, setImportOpen] = useState(false);
  const [sidebarCollapsed, setSidebarCollapsed] = useState(false);
  const [restarting, setRestarting] = useState(false);
  const [migration, setMigration] = useState<MigrationProgress | null>(null);
  // 阶段 03：搜索模式。空查询 = 普通列表，非空 = 检索结果（两套数据源不混用）。
  const [query, setQuery] = useState("");
  const [renameTarget, setRenameTarget] = useState<ItemCardData | null>(null);
  const [confirmDeleteOpen, setConfirmDeleteOpen] = useState(false);
  const [deleting, setDeleting] = useState(false);

  const filters = useItemFilters();
  const items = useItems();
  const filtered = useFilteredItems(
    filters.apiFilters,
    filters.key,
    filters.active && query.trim().length === 0,
  );
  const search = useSearch(query, filters.apiFilters, filters.key);
  const pipeline = usePipelineState();
  const searching = query.trim().length > 0;

  const listQuery = filters.active && !searching ? filtered : items;
  const list = useMemo(() => flattenItemPages(listQuery.data?.pages), [listQuery.data]);

  // 检索结果也要能打开详情与多选：统一成卡片数据，避免两套查找逻辑。
  const searchCards = useMemo(() => search.hits.map(hitToCardData), [search.hits]);
  const visibleCards = searching ? searchCards : list;

  // 切换筛选/首次进入筛选时，新查询会短暂 pending：先把上一份可见结果留在网格里，
  // 避免整块加载占位把已有卡片卸载（与 React Query 的 keepPreviousData 同效）。
  const lastVisibleCards = useRef<ItemCardData[]>([]);
  useEffect(() => {
    // 只记普通列表/筛选列表；检索结果不参与，避免清空搜索时短暂显示旧命中。
    if (!searching && visibleCards.length > 0) {
      lastVisibleCards.current = visibleCards;
    }
  }, [visibleCards, searching]);
  const gridCards =
    !searching && listQuery.isPending && list.length === 0
      ? lastVisibleCards.current
      : visibleCards;
  const orderedIds = useMemo(() => visibleCards.map((item) => item.id), [visibleCards]);
  const selection = useSelection(orderedIds);
  const {
    selectionMode,
    selectedIds,
    selectMany,
    selectAllLoaded,
    clear: clearSelection,
    exit: exitSelection,
  } = selection;

  // 传给卡片的回调必须稳定，否则 memo 后的 ItemCard 每次列表渲染都会重新渲染。
  const { toggle: toggleSelection } = selection;
  const { fetchNextPage, refetch: refetchList } = listQuery;
  const toggleCardSelection = useCallback(
    (item: ItemCardData, shift: boolean) => toggleSelection(item.id, shift),
    [toggleSelection],
  );
  const loadMore = useCallback(() => void fetchNextPage(), [fetchNextPage]);
  const retryList = useCallback(() => void refetchList(), [refetchList]);

  const tagsQuery = useQuery({ queryKey: queryKeys.tags, queryFn: () => api.listTags() });
  const tagOptions = useMemo(
    () => (tagsQuery.data ?? []).map((tag) => tag.name),
    [tagsQuery.data],
  );

  // 筛选变化等同于「重新开始一轮选择」：已选 id 可能已不在结果集里。
  useEffect(() => {
    clearSelection();
  }, [filters.key, clearSelection]);

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

  const toggleSelectionMode = useCallback(() => {
    if (selection.selectionMode) {
      selection.exit();
      return;
    }
    selection.enter();
    // 多选与详情互斥：避免「点卡片以为在勾选，结果打开了详情」。
    closeDetail();
  }, [selection, closeDetail]);

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

  // 列表被事件重置后选中项可能已不存在：自动收起，避免详情停在幽灵数据上。
  useEffect(() => {
    if (
      selectedId !== null &&
      visibleCards.length > 0 &&
      !visibleCards.some((item) => item.id === selectedId)
    ) {
      setSelectedId(null);
      setDetailHidden(true);
    }
  }, [visibleCards, selectedId]);

  // 全局键盘：Esc 退出选择/收起详情、T 切主题、B 折叠侧栏；Ctrl+A 仅在多选模式下全选。
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      const target = event.target as HTMLElement | null;
      const typing =
        target !== null &&
        (target.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(target.tagName));

      if ((event.ctrlKey || event.metaKey) && (event.key === "a" || event.key === "A")) {
        // 只在多选模式下接管，避免抢走输入框/系统级的「全选」。
        if (typing || !selectionMode) {
          return;
        }
        event.preventDefault();
        selectAllLoaded();
        return;
      }

      if (event.defaultPrevented || event.ctrlKey || event.metaKey || event.altKey) {
        return;
      }

      if (event.key === "Escape") {
        // 输入态由组件自己处理 Esc（备注 flush、标签取消），这里不动。
        if (typing) {
          return;
        }
        if (renameTarget !== null) {
          setRenameTarget(null);
          return;
        }
        if (confirmDeleteOpen) {
          setConfirmDeleteOpen(false);
          return;
        }
        if (settingsOpen) {
          setSettingsOpen(false);
          return;
        }
        if (selectionMode) {
          exitSelection();
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
  }, [
    settingsOpen,
    renameTarget,
    confirmDeleteOpen,
    closeDetail,
    selectionMode,
    selectAllLoaded,
    exitSelection,
  ]);

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
  // 总数优先用后端 count（覆盖全部页）；筛选态没有 count 接口，如实标注「已加载」。
  const shownCount = searching
    ? search.status === "ready"
      ? (search.response?.total ?? 0)
      : null
    : filters.active
      ? listQuery.isPending
        ? null
        : list.length
      : (usageQuery.data?.itemCount ?? (listQuery.isPending ? null : list.length));
  const countText =
    shownCount === null
      ? "读取中…"
      : filters.active && !searching
        ? `已加载 ${shownCount} 项`
        : `找到 ${shownCount} 项`;
  const selected = visibleCards.find((item) => item.id === selectedId) ?? null;

  // 列表可能因检索防抖/筛选切换瞬时为空：保留最近一次选中的卡片，
  // 避免详情被卸载（也会连带丢掉面板里未保存的草稿）。
  const lastSelectedCard = useRef<ItemCardData | null>(null);
  useEffect(() => {
    if (selected !== null) {
      lastSelectedCard.current = selected;
    }
  }, [selected]);
  const shownSelected =
    selected ??
    (selectedId !== null &&
    visibleCards.length === 0 &&
    (listQuery.isPending || search.busy) &&
    lastSelectedCard.current?.id === selectedId
      ? lastSelectedCard.current
      : null);
  const migrationPercent =
    migration !== null && migration.total > 0
      ? Math.min(100, Math.round((migration.done / migration.total) * 100))
      : null;

  /** 「全选」优先按当前筛选取全部 id；纯检索态只能全选已加载的命中。 */
  const selectAll = useCallback(async () => {
    if (searching) {
      selectAllLoaded();
      return;
    }
    if (!filters.active) {
      selectAllLoaded();
      return;
    }
    try {
      const ids = await api.listItemIdsFiltered(filters.apiFilters);
      selectMany(ids);
    } catch (error) {
      pushToast(describeError(error), "error");
    }
  }, [searching, filters.active, filters.apiFilters, selectAllLoaded, selectMany]);

  const deleteSelected = useCallback(async () => {
    const ids = selection.ids;
    if (ids.length === 0 || deleting) {
      return;
    }
    setDeleting(true);
    let deleted = 0;
    let failed = 0;
    try {
      for (let offset = 0; offset < ids.length; offset += DELETE_BATCH_SIZE) {
        const report = await api.deleteItems(ids.slice(offset, offset + DELETE_BATCH_SIZE));
        deleted += report.deleted;
        failed += report.failed;
      }
      selection.remove(ids);
      // 事件载荷只有计数，批量删除在这里按已知 ids 立即移除，避免等一次整表重取。
      removeItemsEverywhere(queryClient, new Set(ids));
      if (selectedId !== null && ids.includes(selectedId)) {
        closeDetail();
      }
      setConfirmDeleteOpen(false);
      if (failed > 0) {
        pushToast(`已删除 ${deleted} 项，${failed} 项删除失败`, "error");
      } else {
        pushToast(`已删除 ${deleted} 项素材`);
      }
    } catch (error) {
      pushToast(describeError(error), "error");
    } finally {
      setDeleting(false);
    }
  }, [selection, selectedId, deleting, closeDetail, queryClient]);

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
          onTagFilter={filters.toggleTag}
          activeTags={filters.state.tags}
        />

        <main className="main">
          <div className="main-head">
            <div className="crumbs">
              <span>素材库</span>
              <span className="sep">/</span>
              <span className="here">
                {searching ? "检索" : filters.active ? "筛选" : "全部素材"}
              </span>
            </div>
            <div className="search-actions">
              <SearchBar
                value={query}
                onChange={setQuery}
                busy={search.busy}
                semanticReady={pipeline.semanticReady}
              />
              <button type="button" className="btn import-entry-button" aria-label="添加素材" onClick={() => setImportOpen(true)}>
                <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5" aria-hidden="true"><path d="M8 3v10M3 8h10" /></svg>
                <span>添加</span>
              </button>
            </div>

            <FilterBar controller={filters} tagOptions={tagOptions} />

            <div className="result-bar">
              <div className="result-title">
                <h1>{searching ? "检索结果" : filters.active ? "筛选结果" : "全部素材"}</h1>
                <span className="n">
                  {searching
                    ? search.status === "ready"
                      ? `命中 ${search.response?.total ?? 0} 项`
                      : "检索中…"
                    : countText}
                </span>
              </div>
              <button
                type="button"
                className={`mini-btn selection-toggle${selectionMode ? " is-active" : ""}`}
                aria-pressed={selectionMode}
                onClick={toggleSelectionMode}
              >
                {selectionMode ? "完成" : "选择"}
              </button>
            </div>

            {selectionMode && selection.count > 0 && (
              <SelectionBar
                count={selection.count}
                totalCount={searching ? shownCount : list.length}
                busy={deleting}
                onSelectAll={() => void selectAll()}
                onClear={selection.clear}
                onDelete={() => setConfirmDeleteOpen(true)}
              />
            )}

            <ActiveFilterChips
              chips={filters.chips}
              onRemove={filters.removeChip}
              onClear={filters.clear}
            />

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
                selectedId={selectedId}
                selectionMode={selectionMode}
                selectedIds={selectedIds}
                onToggleSelect={toggleCardSelection}
              />
            ) : !listQuery.isPending && !listQuery.isError && list.length === 0 && !filters.active ? (
              <EmptyState onAdd={() => setImportOpen(true)} />
            ) : (
              <ItemGrid
                items={gridCards}
                isPending={listQuery.isPending}
                isError={listQuery.isError}
                errorMessage={listQuery.error ? describeError(listQuery.error) : null}
                hasNextPage={listQuery.hasNextPage}
                isFetchingNextPage={listQuery.isFetchingNextPage}
                onLoadMore={loadMore}
                onRetry={retryList}
                onOpen={openDetail}
                selectedId={selectedId}
                selectionMode={selectionMode}
                selectedIds={selectedIds}
                onToggleSelect={toggleCardSelection}
                emptyText={
                  filters.active ? "没有符合当前筛选条件的素材。" : "还没有素材。"
                }
              />
            )}
          </div>
        </main>

        <aside
          className={`detail${detailHidden ? " is-hidden" : ""}`}
          aria-hidden={detailHidden}
          inert={detailHidden}
        >
          {shownSelected !== null ? (
            <DetailPanel item={shownSelected} onClose={closeDetail} onRename={setRenameTarget} />
          ) : (
            <DetailEmpty />
          )}
        </aside>
      </div>

      <StatusBar
        status={status}
        version={state.status === "ready" ? state.app.version : null}
        pipeline={pipeline}
        onOpenImports={() => setImportOpen(true)}
      />
      {settingsOpen && <SettingsDialog onClose={() => setSettingsOpen(false)} />}
      {importOpen && (
        <ImportDialog
          searching={searching}
          onShowAll={() => {
            filters.clear();
            setQuery("");
            setImportOpen(false);
          }}
          onClose={() => setImportOpen(false)}
        />
      )}
      {renameTarget !== null && (
        <RenameItemDialog
          item={{
            id: renameTarget.id,
            title: renameTarget.title,
            sourceTitle: renameTarget.sourceTitle,
          }}
          onClose={() => setRenameTarget(null)}
        />
      )}
      {confirmDeleteOpen && (
        <ConfirmDialog
          title={`删除 ${selection.count} 项素材？`}
          description="将删除这些素材及其原图、缩略图与标签关联，操作不可恢复。"
          confirmLabel="删除"
          danger
          onConfirm={() => void deleteSelected()}
          onCancel={() => setConfirmDeleteOpen(false)}
        />
      )}
      <ToastViewport />
    </div>
  );
}
