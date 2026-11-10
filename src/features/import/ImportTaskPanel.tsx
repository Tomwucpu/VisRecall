import { useEffect, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { api, type ImportTaskDto, type ImportTaskEntriesPageDto } from "../../lib/api";
import { describeError } from "../../lib/errors";
import { queryKeys } from "../../lib/queryClient";

const STATUS_LABEL: Record<string, string> = {
  pending: "等待处理",
  running: "正在导入",
  scanned: "扫描完成",
  completed: "导入完成",
  completed_with_errors: "部分完成",
  cancelled: "已取消",
  interrupted: "已中断",
  failed: "导入失败",
};

type Props = {
  task: ImportTaskDto;
  onUpdate: (task: ImportTaskDto) => void;
};

/**
 * 任务进度以权威查询为准，事件仅触发刷新。
 *
 * 终态例外：导入任务一结束就从库里删掉，结果（计数 + 失败明细）由
 * `import_task_finished` 事件快照写入缓存，这里不再回查任务。
 */
export function ImportTaskPanel({ task, onUpdate }: Props) {
  const queryClient = useQueryClient();
  const [operationError, setOperationError] = useState<string | null>(null);
  const [operating, setOperating] = useState(false);
  const taskQuery = useQuery({
    queryKey: queryKeys.importTask(task.id),
    queryFn: () => api.getImportTask(task.id),
    initialData: task,
    // 任务行在终态后即被删除：这一次 404 是正常现象，不必重试。
    retry: false,
    refetchInterval: (query) => {
      const status = query.state.data?.status;
      return status === "running" || status === "pending" ? 750 : false;
    },
  });
  const current = taskQuery.data ?? task;
  const running = current.status === "running" || current.status === "pending";
  const scanning = running && current.kind === "directory_scan";
  // 失败明细由 `import_task_finished` 事件写进缓存：任务行删除后无法再回查条目。
  const failures = useQuery<ImportTaskEntriesPageDto>({
    queryKey: [...queryKeys.importTask(task.id), "failures"],
    queryFn: () => api.listImportTaskEntries(task.id, null, 100),
    enabled: false,
  });

  useEffect(() => {
    if (taskQuery.data !== undefined) onUpdate(taskQuery.data);
  }, [taskQuery.data, onUpdate]);

  const apply = async (operation: () => Promise<ImportTaskDto>) => {
    setOperating(true);
    setOperationError(null);
    try {
      const next = await operation();
      queryClient.setQueryData(queryKeys.importTask(task.id), next);
      onUpdate(next);
    } catch (reason) {
      // 操作错误与后端任务错误分开，避免把本地失败伪造成权威任务状态。
      setOperationError(describeError(reason));
    } finally {
      setOperating(false);
    }
  };

  const percent = current.totalCount === 0 ? 100 : Math.min(100, Math.round((current.processedCount / current.totalCount) * 100));
  const statusLabel = scanning ? "正在扫描" : STATUS_LABEL[current.status] ?? current.status;
  return (
    <div className="import-task-panel">
      <div className="import-task-status" role="status">
        <strong>{statusLabel}</strong>
        <span>{scanning ? `已发现 ${current.totalCount.toLocaleString()} 项` : `${current.processedCount.toLocaleString()} / ${current.totalCount.toLocaleString()}`}</span>
      </div>
      <div className={`import-progress${scanning ? " is-indeterminate" : ""}`} role="progressbar" aria-label={scanning ? "目录扫描进度" : "导入进度"} aria-valuemin={0} aria-valuemax={100} aria-valuenow={scanning ? undefined : percent}>
        <i style={scanning ? undefined : { transform: `scaleX(${percent / 100})` }} />
      </div>
      <div className="import-task-counts">
        <span>成功 {current.succeededCount}</span>
        <span>跳过 {current.skippedCount}</span>
        <span>失败 {current.failedCount}</span>
      </div>
      {current.errorSummary !== null && current.errorSummary !== undefined && <p className="import-error" role="alert">{current.errorSummary}</p>}
      {operationError !== null && <p className="import-error" role="alert">{operationError}</p>}
      {failures.data !== undefined && failures.data.entries.some((entry) => entry.status === "failed") && (
        <ul className="import-failure-list">
          {failures.data.entries.filter((entry) => entry.status === "failed").map((entry) => (
            <li key={entry.entryKey}><strong>{entry.displayName}</strong><span>{entry.errorMessage ?? "导入失败"}</span></li>
          ))}
        </ul>
      )}
      <div className="import-task-actions">
        {running && <button type="button" className="btn" disabled={operating || current.cancelRequested} onClick={() => void apply(() => api.cancelImportTask(task.id))}>{current.cancelRequested ? "正在取消…" : scanning ? "取消扫描" : "取消，保留已完成项"}</button>}
      </div>
    </div>
  );
}
