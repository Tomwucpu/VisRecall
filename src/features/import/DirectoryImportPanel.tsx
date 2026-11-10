import { useEffect, useState } from "react";
import { useInfiniteQuery, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  api,
  type DirectoryScanOptionsDto,
  type DirectorySelectionDto,
  type ImportTaskDto,
} from "../../lib/api";
import { describeError } from "../../lib/errors";
import { formatBytes } from "../../lib/format";
import { queryKeys } from "../../lib/queryClient";

type Props = {
  selection: DirectorySelectionDto;
  options: DirectoryScanOptionsDto;
  scanTask: ImportTaskDto | null;
  onOptionsChange: (options: DirectoryScanOptionsDto) => void;
  onScanned: (task: ImportTaskDto) => void;
  onImport: (task: ImportTaskDto) => void;
  onReset: () => void;
};

/** 目录任务提交后立即返回，扫描进度通过权威任务查询持续恢复。 */
export function DirectoryImportPanel({
  selection,
  options,
  scanTask,
  onOptionsChange,
  onScanned,
  onImport,
  onReset,
}: Props) {
  const queryClient = useQueryClient();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const taskQuery = useQuery({
    queryKey: queryKeys.importTask(scanTask?.id ?? "no-directory-scan"),
    queryFn: () => api.getImportTask(scanTask!.id),
    enabled: scanTask !== null,
    initialData: scanTask ?? undefined,
    // 扫描失败/取消后任务行会被删除，终态改为由事件快照提供，不再重试。
    retry: false,
    refetchInterval: (query) => {
      const status = query.state.data?.status;
      return status === "running" || status === "pending" ? 500 : false;
    },
  });
  const current = taskQuery.data ?? scanTask;
  const scanned = current?.status === "scanned";
  const scanning = current?.status === "running" || current?.status === "pending";
  const interrupted = current !== null && ["cancelled", "failed", "interrupted"].includes(current.status);
  const readyCount = current === null ? 0 : Math.max(0, current.totalCount - current.skippedCount);

  useEffect(() => {
    if (taskQuery.data !== undefined) onScanned(taskQuery.data);
  }, [taskQuery.data, onScanned]);

  const entries = useInfiniteQuery({
    queryKey: ["import-directory-entries", current?.id],
    queryFn: ({ pageParam }) => api.listImportTaskEntries(current!.id, pageParam, 100),
    initialPageParam: null as number | null,
    getNextPageParam: (page) => page.nextCursor ?? undefined,
    enabled: scanned,
  });

  const scan = async () => {
    setBusy(true);
    setError(null);
    try {
      onScanned(await api.scanImportDirectory(selection.token, options));
    } catch (reason) {
      setError(describeError(reason));
    } finally {
      setBusy(false);
    }
  };

  const cancel = async () => {
    if (current === null) return;
    setBusy(true);
    setError(null);
    try {
      const next = await api.cancelImportTask(current.id);
      // 取消响应本身就是权威终态，立即写入缓存，避免下一次轮询前仍显示运行中。
      queryClient.setQueryData(queryKeys.importTask(next.id), next);
      onScanned(next);
    } catch (reason) {
      setError(describeError(reason));
    } finally {
      setBusy(false);
    }
  };

  const startImport = async () => {
    if (current === null) return;
    setBusy(true);
    setError(null);
    try {
      onImport(await api.startDirectoryImport(current.id));
    } catch (reason) {
      setError(describeError(reason));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="directory-import">
      <div className="import-path" title={selection.path}>{selection.path}</div>
      <p className="import-reference-note">
        引用原文件，不复制。移动或断开原目录后，可在设置中重新定位。<br />
        TXT / Markdown 仅前 10,000 字符参与检索。
      </p>

      {current === null ? (
        <div className="directory-options">
          <label><input type="checkbox" checked={options.includeSubdirectories} disabled={busy} onChange={(event) => onOptionsChange({ ...options, includeSubdirectories: event.target.checked })} />包含子目录</label>
          <label><input type="checkbox" checked={options.includeImages} disabled={busy || !options.includeText} onChange={(event) => onOptionsChange({ ...options, includeImages: event.target.checked })} />图片</label>
          <label><input type="checkbox" checked={options.includeText} disabled={busy || !options.includeImages} onChange={(event) => onOptionsChange({ ...options, includeText: event.target.checked })} />TXT / Markdown</label>
          <label className="directory-ai-option"><input type="checkbox" checked={options.aiAllowed} disabled={busy} onChange={(event) => onOptionsChange({ ...options, aiAllowed: event.target.checked })} />允许此来源按全局隐私设置使用云端 AI</label>
        </div>
      ) : (
        <div className="directory-scan-summary" role="status" aria-live="polite">
          {scanning ? "正在扫描：" : "扫描结果："}
          已发现 {current.totalCount.toLocaleString()} 项，可导入 {readyCount.toLocaleString()} 项，跳过 {current.skippedCount.toLocaleString()} 项
          {!scanning && `，共 ${formatBytes(current.originalBytes)}`}
        </div>
      )}

      {scanned && (
        <>
          <div className="directory-entry-list">
            {entries.data?.pages.flatMap((page) => page.entries).map((entry) => (
              <div className={`directory-entry${entry.status === "skipped" ? " is-skipped" : ""}`} key={entry.entryKey}>
                <span title={entry.relativePath}>{entry.relativePath}</span>
                <small>
                  {formatBytes(entry.size)}
                  {entry.status === "skipped" && ` · ${entry.errorMessage ?? "已跳过"}`}
                </small>
              </div>
            ))}
            {entries.isPending && <p className="import-muted">正在读取预览…</p>}
            {entries.isError && <p className="import-error" role="alert">{describeError(entries.error)}</p>}
            {!entries.isPending && current.totalCount === 0 && <p className="import-muted">目录中没有可预览的文件</p>}
          </div>
          {entries.hasNextPage && (
            <button type="button" className="mini-btn" disabled={entries.isFetchingNextPage} onClick={() => void entries.fetchNextPage()}>
              {entries.isFetchingNextPage ? "正在加载…" : "加载更多预览"}
            </button>
          )}
        </>
      )}

      {error !== null && <p className="import-error" role="alert">{error}</p>}
      {current?.errorSummary && <p className="import-error" role="alert">{current.errorSummary}</p>}
      <div className="import-footer">
        <span>{scanned ? "确认后才会建立索引" : "最多扫描 50,000 个文件、32 层目录"}</span>
        {current === null && (
          <button type="button" className="btn primary" disabled={busy} onClick={() => void scan()}>
            {busy ? "正在提交…" : "扫描目录"}
          </button>
        )}
        {scanning && (
          <button type="button" className="btn" disabled={busy || current.cancelRequested} onClick={() => void cancel()}>
            {current.cancelRequested ? "正在取消…" : busy ? "正在提交…" : "取消扫描"}
          </button>
        )}
        {scanned && (
          <button type="button" className="btn primary" disabled={busy || readyCount === 0} onClick={() => void startImport()}>
            {busy ? "正在提交…" : "确认引用导入"}
          </button>
        )}
        {interrupted && <button type="button" className="btn primary" onClick={onReset}>重新选择目录</button>}
      </div>
    </div>
  );
}
