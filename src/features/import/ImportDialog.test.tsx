import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { ReactNode } from "react";
import { ImportDialog } from "./ImportDialog";

const task = {
  id: "task-1", sourceId: null, kind: "files", status: "completed",
  totalCount: 1, processedCount: 1, succeededCount: 1, skippedCount: 0,
  failedCount: 0, originalBytes: 12, cancelRequested: false,
  errorSummary: null, createdAt: 1, updatedAt: 2,
};

const scannedTask = {
  ...task,
  id: "scan-1",
  sourceId: "source-1",
  kind: "directory_scan",
  status: "scanned",
  totalCount: 3,
  processedCount: 0,
  succeededCount: 0,
  skippedCount: 1,
};

const commands = vi.hoisted(() => ({
  pickImportFiles: vi.fn(async () => ({
    token: "files-token",
    files: [
      { entryId: "a", fileName: "甲.png", size: 12, supported: true, reason: null },
      { entryId: "b", fileName: "乙.txt", size: 20, supported: true, reason: null },
      { entryId: "c", fileName: "丙.exe", size: 30, supported: false, reason: "不支持此类型" },
    ],
  })),
  startFileImport: vi.fn(async () => task),
  getImportTask: vi.fn(async (id: string) => id === "scan-1" ? scannedTask : task),
  createTextItem: vi.fn(async () => "text-1"),
  pickImportDirectory: vi.fn(async () => ({ token: "directory-token", path: "D:\\素材" })),
  scanImportDirectory: vi.fn(async () => ({ ...scannedTask, status: "pending", totalCount: 0, skippedCount: 0 })),
  listImportTaskEntries: vi.fn(async () => ({
    entries: [
      { rowid: 1, entryKey: "0", relativePath: "甲.png", displayName: "甲.png", size: 12, status: "ready", errorCode: null, errorMessage: null, itemId: null },
      { rowid: 2, entryKey: "1", relativePath: "文档/乙.md", displayName: "乙.md", size: 20, status: "ready", errorCode: null, errorMessage: null, itemId: null },
      { rowid: 3, entryKey: "2", relativePath: "说明.pdf", displayName: "说明.pdf", size: 30, status: "skipped", errorCode: "UNSUPPORTED", errorMessage: "不支持的文件类型", itemId: null },
    ],
    nextCursor: null,
  })),
  startDirectoryImport: vi.fn(async () => ({ ...task, id: "directory-1", kind: "directory_import", status: "pending", totalCount: 2, processedCount: 0, succeededCount: 0 })),
  cancelImportTask: vi.fn(async (id: string) => ({ ...scannedTask, id, status: "cancelled" })),
  setImportDropTarget: vi.fn(async () => undefined),
  discardImportSelection: vi.fn(async () => undefined),
  listImportTasks: vi.fn(async (): Promise<Array<typeof scannedTask>> => []),
  listImportSources: vi.fn(async () => [{ id: "source-1", rootPath: "D:\\素材" }]),
}));

vi.mock("../../lib/bindings", () => ({ commands }));
vi.mock("@tauri-apps/api/event", () => ({ listen: async () => () => {} }));

function renderDialog(node: ReactNode = <ImportDialog searching={false} onShowAll={() => {}} onClose={() => {}} />) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(<QueryClientProvider client={client}>{node}</QueryClientProvider>);
}

afterEach(() => vi.clearAllMocks());

test("shows unified file, directory and text entry points", () => {
  renderDialog();
  expect(screen.getByRole("dialog", { name: "添加素材" })).toBeInTheDocument();
  expect(screen.getByText("拖拽文件到这里")).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "选择文件" })).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "导入目录" })).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "也可以：新建文字" })).toBeInTheDocument();
});

test("removes mistaken files and only submits supported remaining entries", async () => {
  renderDialog();
  fireEvent.click(screen.getByRole("button", { name: "选择文件" }));
  expect(await screen.findByText("甲.png")).toBeInTheDocument();
  expect(screen.getByText(/不支持此类型/)).toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "移除 甲.png" }));
  fireEvent.click(screen.getByRole("button", { name: "开始导入" }));
  await waitFor(() => expect(commands.startFileImport).toHaveBeenCalledWith("files-token", ["b"]));
  expect(await screen.findByText("导入完成")).toBeInTheDocument();
});

test("keeps the text limit and submits trimmed content with Ctrl+Enter", async () => {
  renderDialog();
  fireEvent.click(screen.getByRole("button", { name: "也可以：新建文字" }));
  const body = screen.getByPlaceholderText("输入要保存和检索的文字");
  expect(body).toHaveAttribute("maxlength", "10000");
  fireEvent.change(screen.getByPlaceholderText("为这段文字命名"), { target: { value: "标题" } });
  fireEvent.change(body, { target: { value: "  正文内容  " } });
  fireEvent.change(screen.getByPlaceholderText("例如：灵感, 读书笔记"), { target: { value: "笔记，灵感" } });
  fireEvent.keyDown(body, { key: "Enter", ctrlKey: true });
  await waitFor(() => expect(commands.createTextItem).toHaveBeenCalledWith("标题", "正文内容", ["笔记", "灵感"]));
  expect(await screen.findByText("文字已保存")).toBeInTheDocument();
});

test("asks before closing an unsubmitted text draft", () => {
  const onClose = vi.fn();
  renderDialog(<ImportDialog searching={false} onShowAll={() => {}} onClose={onClose} />);
  fireEvent.click(screen.getByRole("button", { name: "也可以：新建文字" }));
  fireEvent.change(screen.getByPlaceholderText("输入要保存和检索的文字"), { target: { value: "草稿" } });
  fireEvent.click(screen.getByRole("button", { name: "关闭添加素材" }));
  expect(screen.getByText("放弃未提交的内容？")).toBeInTheDocument();
  expect(onClose).not.toHaveBeenCalled();
});

test("no longer lists previous imports on the entry view", async () => {
  // 导入任务完成即从库里删除，历史列表一并下线：只剩「继续目录扫描」入口。
  commands.listImportTasks.mockResolvedValueOnce([
    { ...scannedTask, id: "old-1", kind: "files", status: "completed", succeededCount: 3 },
  ]);
  renderDialog();
  expect(await screen.findByRole("button", { name: "选择文件" })).toBeInTheDocument();
  await waitFor(() => expect(commands.listImportTasks).toHaveBeenCalled());
  expect(screen.queryByText("最近导入")).not.toBeInTheDocument();
  expect(screen.queryByRole("button", { name: /文件导入/ })).not.toBeInTheDocument();
});

test("scans and paginates a directory before confirming reference import", async () => {
  renderDialog();
  fireEvent.click(screen.getByRole("button", { name: "导入目录" }));
  expect(await screen.findByText("D:\\素材")).toBeInTheDocument();
  expect(screen.getByText(/引用原文件，不复制。/)).toBeInTheDocument();
  expect(screen.getByText(/TXT \/ Markdown 仅前 10,000 字符参与检索/)).toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "扫描目录" }));
  expect(await screen.findByText("文档/乙.md")).toBeInTheDocument();
  expect(screen.getByText(/说明\.pdf/)).toBeInTheDocument();
  expect(screen.getByText(/不支持的文件类型/)).toBeInTheDocument();
  expect(commands.scanImportDirectory).toHaveBeenCalledWith("directory-token", {
    includeSubdirectories: true, includeImages: true, includeText: true,
  });
  expect(screen.queryByLabelText(/允许此来源/)).not.toBeInTheDocument();
  expect(screen.getByText(/云端解析由「设置 → 模型」中的开关统一决定/)).toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "确认引用导入" }));
  await waitFor(() => expect(commands.startDirectoryImport).toHaveBeenCalledWith("scan-1"));
});

test("updates scan counts and applies cancellation without waiting for another poll", async () => {
  const runningScan = {
    ...scannedTask,
    status: "running",
    totalCount: 64,
    skippedCount: 2,
    originalBytes: 620,
  };
  commands.getImportTask.mockResolvedValueOnce(runningScan);
  commands.cancelImportTask.mockResolvedValueOnce({ ...runningScan, status: "cancelled", cancelRequested: true });
  renderDialog();
  fireEvent.click(screen.getByRole("button", { name: "导入目录" }));
  fireEvent.click(await screen.findByRole("button", { name: "扫描目录" }));

  expect(await screen.findByText(/已发现 64 项，可导入 62 项，跳过 2 项/)).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "确认引用导入" })).not.toBeInTheDocument();
  expect(commands.listImportTaskEntries).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("button", { name: "取消扫描" }));

  expect(await screen.findByRole("button", { name: "重新选择目录" })).toBeInTheDocument();
  expect(commands.cancelImportTask).toHaveBeenCalledWith("scan-1");
  expect(screen.queryByRole("button", { name: "确认引用导入" })).not.toBeInTheDocument();
});

test("recovers a running directory scan when the dialog is reopened", async () => {
  const firstRender = renderDialog();
  firstRender.unmount();
  const runningScan = { ...scannedTask, status: "running", totalCount: 42, skippedCount: 0 };
  commands.listImportTasks.mockResolvedValueOnce([runningScan]);
  commands.getImportTask.mockResolvedValueOnce(runningScan);
  renderDialog();

  const resume = await screen.findByRole("button", { name: /继续查看目录扫描/ });
  await waitFor(() => expect(resume).toBeEnabled());
  fireEvent.click(resume);

  expect(await screen.findByText("D:\\素材")).toBeInTheDocument();
  expect(await screen.findByText(/已发现 42 项，可导入 42 项/)).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "确认引用导入" })).not.toBeInTheDocument();
});

test("disables duplicate file submission and immediately shows the accepted task", async () => {
  let resolveImport: ((value: typeof task) => void) | undefined;
  const pendingTask = { ...task, status: "pending", processedCount: 0, succeededCount: 0 };
  commands.startFileImport.mockImplementationOnce(() => new Promise((resolve) => { resolveImport = resolve; }));
  commands.getImportTask.mockResolvedValueOnce(pendingTask);
  renderDialog();
  fireEvent.click(screen.getByRole("button", { name: "选择文件" }));
  const submit = await screen.findByRole("button", { name: "开始导入" });
  fireEvent.click(submit);
  fireEvent.click(submit);

  expect(commands.startFileImport).toHaveBeenCalledTimes(1);
  expect(submit).toBeDisabled();
  resolveImport?.(pendingTask);
  expect(await screen.findByText("等待处理")).toBeInTheDocument();
  expect(screen.getByRole("progressbar", { name: "导入进度" })).toBeInTheDocument();
});
