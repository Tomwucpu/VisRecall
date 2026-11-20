import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { api, type ImportSourceDto, type ImportTaskDto } from "../../lib/api";
import { describeError } from "../../lib/errors";
import { formatAbsoluteTime } from "../../lib/format";
import { queryKeys } from "../../lib/queryClient";
import { pushToast } from "../../lib/toast";
import { ConfirmDialog } from "../common/ConfirmDialog";
import { ImportTaskPanel } from "../import/ImportTaskPanel";

const SOURCE_STATUS: Record<string, string> = {
  available: "可用",
  offline: "离线",
  missing: "目录缺失",
  permission_denied: "无访问权限",
  removed: "已移除",
};

/** 设置中的引用来源管理；任何操作都不会修改外部原件。 */
export function ImportSourcesPane() {
  const queryClient = useQueryClient();
  const sources = useQuery({ queryKey: queryKeys.importSources, queryFn: api.listImportSources });
  const [busyId, setBusyId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [task, setTask] = useState<ImportTaskDto | null>(null);
  const [removeTarget, setRemoveTarget] = useState<ImportSourceDto | null>(null);
  const [removeItems, setRemoveItems] = useState(false);

  const refresh = async (source: ImportSourceDto) => {
    setBusyId(source.id);
    setError(null);
    try {
      setTask(await api.refreshImportSource(source.id));
      await queryClient.invalidateQueries({ queryKey: queryKeys.importSources });
    } catch (reason) {
      setError(describeError(reason));
    } finally {
      setBusyId(null);
    }
  };

  const relocate = async (source: ImportSourceDto) => {
    setBusyId(source.id);
    setError(null);
    try {
      const selection = await api.pickImportDirectory();
      if (selection !== null) {
        await api.relocateImportSource(source.id, selection.token);
        await queryClient.invalidateQueries({ queryKey: queryKeys.importSources });
        pushToast("目录已重新定位，可刷新同步内容");
      }
    } catch (reason) {
      setError(describeError(reason));
    } finally {
      setBusyId(null);
    }
  };

  const remove = async () => {
    if (removeTarget === null) return;
    setBusyId(removeTarget.id);
    setError(null);
    try {
      await api.removeImportSource(removeTarget.id, removeItems);
      setRemoveTarget(null);
      setRemoveItems(false);
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: queryKeys.importSources }),
        queryClient.invalidateQueries({ queryKey: queryKeys.items }),
        queryClient.invalidateQueries({ queryKey: ["items-filtered"] }),
      ]);
      pushToast(removeItems ? "来源及关联索引已移除，外部原件未改动" : "来源已停用，素材索引仍保留");
    } catch (reason) {
      setError(describeError(reason));
    } finally {
      setBusyId(null);
    }
  };

  return (
    <div className="pane import-sources-pane">
      <p className="pane-desc">目录以引用方式加入素材库。刷新、重新定位和移除来源都不会修改或删除外部原件。</p>
      {sources.isPending && <p className="row-d">正在读取导入目录…</p>}
      {sources.isError && <p className="import-error" role="alert">{describeError(sources.error)}</p>}
      {sources.data?.length === 0 && <div className="import-sources-empty"><strong>还没有导入目录</strong><span>从主窗口“添加”入口选择目录并扫描。</span></div>}
      <div className="import-source-list">
        {sources.data?.map((source) => (
          <section className="import-source" key={source.id}>
            <div className="import-source-heading">
              <strong title={source.rootPath}>{source.rootPath}</strong>
              <span>{SOURCE_STATUS[source.status] ?? source.status}</span>
            </div>
            <div className="import-source-meta">
              <span>{source.itemCount} 项素材</span>
              <span>最后同步：{source.lastScannedAt == null ? "—" : formatAbsoluteTime(source.lastScannedAt)}</span>
            </div>
            <div className="import-source-actions">
              <button type="button" className="mini-btn" disabled={busyId !== null} onClick={() => void refresh(source)}>{busyId === source.id ? "处理中…" : "刷新"}</button>
              <button type="button" className="mini-btn" disabled={busyId !== null} onClick={() => void relocate(source)}>重新定位</button>
              <button type="button" className="mini-btn danger-text" disabled={busyId !== null} onClick={() => { setRemoveItems(false); setRemoveTarget(source); }}>移除来源</button>
            </div>
          </section>
        ))}
      </div>
      {error !== null && <p className="import-error" role="alert">{error}</p>}
      {task !== null && <div className="import-source-task"><ImportTaskPanel task={task} onUpdate={setTask} /></div>}
      {removeTarget !== null && (
        <ConfirmDialog
          title="移除这个导入目录？"
          description={removeItems ? "关联素材与索引也会删除；外部原件始终保留，此操作不可恢复。" : "只停用目录同步，已建立的素材索引继续保留；外部原件不会改动。"}
          confirmLabel={removeItems ? "删除索引并移除" : "保留索引并移除"}
          danger={removeItems}
          onConfirm={() => void remove()}
          onCancel={() => setRemoveTarget(null)}
        >
          <label className="import-remove-choice"><input type="checkbox" checked={removeItems} onChange={(event) => setRemoveItems(event.target.checked)} />同时删除 {removeTarget.itemCount} 条关联素材与索引</label>
        </ConfirmDialog>
      )}
    </div>
  );
}
