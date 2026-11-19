import { useCallback, useEffect, useRef, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  api,
  type DirectoryScanOptionsDto,
  type DirectorySelectionDto,
  type FileSelectionDto,
  type ImportTaskDto,
} from "../../lib/api";
import { describeError } from "../../lib/errors";
import { queryKeys } from "../../lib/queryClient";
import { pushToast } from "../../lib/toast";
import { ConfirmDialog } from "../common/ConfirmDialog";
import { DirectoryImportPanel } from "./DirectoryImportPanel";
import { ImportDropZone } from "./ImportDropZone";
import { ImportFileList } from "./ImportFileList";
import { ImportTaskPanel } from "./ImportTaskPanel";
import { TextImportForm, type TextDraft } from "./TextImportForm";

type View = "entry" | "files" | "text" | "directory" | "task";

type Props = {
  searching: boolean;
  onShowAll: () => void;
  onClose: () => void;
};

const EMPTY_TEXT: TextDraft = { title: "", body: "", tags: "" };
const DEFAULT_DIRECTORY_OPTIONS: DirectoryScanOptionsDto = {
  includeSubdirectories: true,
  includeImages: true,
  includeText: true,
};

const VIEW_TITLE: Record<View, string> = {
  entry: "添加素材",
  files: "确认导入文件",
  text: "新建文字",
  directory: "导入目录",
  task: "导入结果",
};

/** 所有主动导入共用同一弹窗，文件复制与目录引用始终分批提交。 */
export function ImportDialog({ searching, onShowAll, onClose }: Props) {
  const [view, setView] = useState<View>("entry");
  const [fileSelection, setFileSelection] = useState<FileSelectionDto | null>(null);
  const [removedIds, setRemovedIds] = useState<ReadonlySet<string>>(new Set());
  const [textDraft, setTextDraft] = useState<TextDraft>(EMPTY_TEXT);
  const [directorySelection, setDirectorySelection] = useState<DirectorySelectionDto | null>(null);
  const [directoryOptions, setDirectoryOptions] = useState(DEFAULT_DIRECTORY_OPTIONS);
  const [scanTask, setScanTask] = useState<ImportTaskDto | null>(null);
  const [activeTask, setActiveTask] = useState<ImportTaskDto | null>(null);
  const [textSaved, setTextSaved] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [confirmClose, setConfirmClose] = useState(false);
  const panelRef = useRef<HTMLDivElement>(null);
  const queryClient = useQueryClient();
  // 只服务于「继续未完成的目录扫描」：导入任务完成即删除，没有历史可列。
  const pendingScans = useQuery({
    queryKey: queryKeys.importTasks,
    queryFn: () => api.listImportTasks(10),
    refetchInterval: 2_000,
  });
  const recoverableScan = pendingScans.data?.find((task) => task.kind === "directory_scan"
    && ["pending", "running", "scanned"].includes(task.status));
  const importSources = useQuery({
    queryKey: queryKeys.importSources,
    queryFn: api.listImportSources,
    enabled: recoverableScan !== undefined,
  });

  useEffect(() => {
    const previous = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    panelRef.current?.focus();
    return () => previous?.focus();
  }, []);

  const textDirty = textDraft.title.length > 0 || textDraft.body.length > 0 || textDraft.tags.length > 0;
  const dirty = fileSelection !== null || scanTask?.status === "scanned" || textDirty;
  const requestClose = useCallback(() => {
    if (dirty) setConfirmClose(true);
    else onClose();
  }, [dirty, onClose]);

  const finishClose = () => {
    setConfirmClose(false);
    void api.discardImportSelection().catch(() => undefined);
    onClose();
  };

  const acceptFiles = useCallback((selection: FileSelectionDto) => {
    if (fileSelection !== null) {
      setError("请先处理当前待导入文件，再选择新批次");
      setView("files");
      return;
    }
    setFileSelection(selection);
    setRemovedIds(new Set());
    setError(null);
    setView("files");
  }, [fileSelection]);

  const resumeDirectoryScan = (task: ImportTaskDto) => {
    const source = importSources.data?.find((item) => item.id === task.sourceId);
    if (source === undefined) {
      setError("暂时无法读取该扫描的来源目录，请稍后重试");
      return;
    }
    // 恢复视图不需要原选择令牌；只有首次提交扫描才消费令牌。
    setDirectorySelection({ token: "", path: source.rootPath });
    setDirectoryOptions({
      includeSubdirectories: source.includeSubdirectories,
      includeImages: source.includeImages,
      includeText: source.includeText,
    });
    setScanTask(task);
    setTextSaved(false);
    setError(null);
    setView("directory");
  };

  const pickFiles = async () => {
    if (fileSelection !== null) {
      setView("files");
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const selection = await api.pickImportFiles();
      if (selection !== null) acceptFiles(selection);
    } catch (reason) {
      setError(describeError(reason));
    } finally {
      setBusy(false);
    }
  };

  const pickDirectory = async () => {
    if (directorySelection !== null) {
      setView("directory");
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const selection = await api.pickImportDirectory();
      if (selection !== null) {
        setDirectorySelection(selection);
        setScanTask(null);
        setView("directory");
      }
    } catch (reason) {
      setError(describeError(reason));
    } finally {
      setBusy(false);
    }
  };

  const supportedFiles = fileSelection?.files.filter((file) => file.supported && !removedIds.has(file.entryId)) ?? [];

  const startFiles = async () => {
    if (fileSelection === null || supportedFiles.length === 0) return;
    setBusy(true);
    setError(null);
    try {
      const task = await api.startFileImport(fileSelection.token, supportedFiles.map((file) => file.entryId));
      setFileSelection(null);
      setRemovedIds(new Set());
      setActiveTask(task);
      setTextSaved(false);
      setView("task");
    } catch (reason) {
      setError(describeError(reason));
    } finally {
      setBusy(false);
    }
  };

  const saveText = async () => {
    const body = textDraft.body.trim();
    if (body.length === 0) {
      setError("请输入正文后再保存");
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await api.createTextItem(
        textDraft.title.trim() || null,
        body,
        textDraft.tags.split(/[,，]/).map((tag) => tag.trim()).filter(Boolean),
      );
      setTextDraft(EMPTY_TEXT);
      setActiveTask(null);
      setTextSaved(true);
      setView("task");
      void queryClient.resetQueries({ queryKey: queryKeys.items });
      pushToast("文字已添加到素材库");
    } catch (reason) {
      setError(describeError(reason));
    } finally {
      setBusy(false);
    }
  };

  const taskRunning = activeTask !== null && (activeTask.status === "running" || activeTask.status === "pending");

  return (
    <div className="modal import-modal" onMouseDown={requestClose}>
      <div
        ref={panelRef}
        className="import-panel"
        role="dialog"
        aria-modal="true"
        aria-labelledby="import-title"
        aria-describedby="import-description"
        tabIndex={-1}
        onMouseDown={(event) => event.stopPropagation()}
        onKeyDown={(event) => {
          if (event.key === "Escape") {
            event.preventDefault();
            event.stopPropagation();
            requestClose();
          }
          if (event.key === "Tab") {
            const focusable = Array.from(panelRef.current?.querySelectorAll<HTMLElement>("button:not(:disabled), input:not(:disabled), textarea:not(:disabled), [tabindex='0']") ?? []);
            const first = focusable[0];
            const last = focusable.at(-1);
            if (first === undefined || last === undefined) return;
            if (event.shiftKey && (document.activeElement === first || document.activeElement === panelRef.current)) {
              event.preventDefault();
              last.focus();
            } else if (!event.shiftKey && document.activeElement === last) {
              event.preventDefault();
              first.focus();
            }
          }
        }}
      >
        <header className="import-head">
          <div>
            <h2 id="import-title">{VIEW_TITLE[view]}</h2>
            <p id="import-description">将图片、文本文件或本地目录添加到素材库</p>
          </div>
          <button type="button" className="icon-btn" aria-label="关闭添加素材" onClick={requestClose}>
            <svg width="13" height="13" viewBox="0 0 14 14" stroke="currentColor" strokeWidth="1.4" aria-hidden="true"><path d="M2 2l10 10M12 2L2 12" /></svg>
          </button>
        </header>

        <div className="import-body">
          {view !== "entry" && view !== "task" && (
            <button type="button" className="import-back" onClick={() => { setError(null); setView("entry"); }}>← 返回添加方式</button>
          )}

          {view === "entry" && (
            <>
              <ImportDropZone onSelection={acceptFiles} />
              <div className="import-entry-actions">
                <div>
                  <button type="button" className="btn primary" disabled={busy} onClick={() => void pickFiles()}>选择文件</button>
                  <span>保存到素材库</span>
                </div>
                <div>
                  <button type="button" className="btn" disabled={busy} onClick={() => void pickDirectory()}>导入目录</button>
                  <span>引用原文件，不复制</span>
                </div>
              </div>
              <button type="button" className="import-text-link" onClick={() => { setError(null); setView("text"); }}>也可以：新建文字</button>
              {recoverableScan !== undefined && (
                <div className="import-resume">
                  <button type="button" disabled={importSources.isPending} onClick={() => resumeDirectoryScan(recoverableScan)}>
                    <span>{recoverableScan.status === "scanned" ? "查看目录扫描结果" : "继续查看目录扫描"}</span>
                    <small>已发现 {recoverableScan.totalCount} 项</small>
                  </button>
                </div>
              )}
            </>
          )}

          {view === "files" && fileSelection !== null && (
            <>
              <ImportFileList selection={fileSelection} removedIds={removedIds} onRemove={(id) => setRemovedIds((current) => new Set(current).add(id))} />
              {fileSelection.files.some((file) => /\.(txt|md)$/i.test(file.fileName)) && (
                <p className="import-muted">文本文件仅前 10,000 字符参与检索。</p>
              )}
              <div className="import-footer">
                <span>将导入 {supportedFiles.length} 个文件</span>
                <button type="button" className="btn primary" disabled={busy || supportedFiles.length === 0} onClick={() => void startFiles()}>{busy ? "正在导入…" : "开始导入"}</button>
              </div>
            </>
          )}

          {view === "text" && (
            <>
              <TextImportForm draft={textDraft} disabled={busy} onChange={setTextDraft} onSubmit={() => void saveText()} />
              <div className="import-footer">
                <span>正文最多 10,000 字符</span>
                <button type="button" className="btn primary" disabled={busy || textDraft.body.trim().length === 0} onClick={() => void saveText()}>{busy ? "正在保存…" : "保存文字"}</button>
              </div>
            </>
          )}

          {view === "directory" && directorySelection !== null && (
            <DirectoryImportPanel
              selection={directorySelection}
              options={directoryOptions}
              scanTask={scanTask}
              onOptionsChange={setDirectoryOptions}
              onScanned={setScanTask}
              onImport={(task) => { setScanTask(null); setActiveTask(task); setTextSaved(false); setView("task"); }}
              onReset={() => { setDirectorySelection(null); setScanTask(null); setError(null); setView("entry"); }}
            />
          )}

          {view === "task" && activeTask !== null && <ImportTaskPanel task={activeTask} onUpdate={setActiveTask} />}
          {view === "task" && textSaved && <div className="import-text-success" role="status"><strong>文字已保存</strong><span>现在可以通过标题、正文和标签检索。</span></div>}
          {error !== null && <p className="import-error" role="alert">{error}</p>}
        </div>

        {view === "task" && !taskRunning && (
          <footer className="import-result-footer">
            <button type="button" className="btn" onClick={() => { setActiveTask(null); setTextSaved(false); setDirectorySelection(null); setScanTask(null); setView("entry"); }}>继续添加</button>
            {searching && <button type="button" className="btn" onClick={onShowAll}>查看全部素材</button>}
            <button type="button" className="btn primary" onClick={onClose}>完成</button>
          </footer>
        )}
      </div>

      {confirmClose && (
        <ConfirmDialog
          title="放弃未提交的内容？"
          description="待导入文件、目录扫描结果或文字草稿尚未提交，关闭后将丢失这些内容。"
          confirmLabel="放弃并关闭"
          danger
          onConfirm={finishClose}
          onCancel={() => setConfirmClose(false)}
        />
      )}
    </div>
  );
}
