import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { ImportSourcesPane } from "./ImportSourcesPane";

const source = {
  id: "source-1", rootPath: "D:\\素材", includeSubdirectories: true,
  includeImages: true, includeText: true, status: "available",
  lastScannedAt: 1_700_000_000_000, itemCount: 3,
};
const commands = vi.hoisted(() => ({
  listImportSources: vi.fn(async () => [source]),
  refreshImportSource: vi.fn(async () => ({
    id: "refresh-1", sourceId: "source-1", kind: "refresh", status: "completed",
    totalCount: 3, processedCount: 3, succeededCount: 1, skippedCount: 2,
    failedCount: 0, originalBytes: 30, cancelRequested: false,
    errorSummary: null, createdAt: 1, updatedAt: 2,
  })),
  pickImportDirectory: vi.fn(async () => ({ token: "new-location", path: "E:\\素材" })),
  relocateImportSource: vi.fn(async () => ({ ...source, rootPath: "E:\\素材" })),
  removeImportSource: vi.fn(async () => 3),
}));

vi.mock("../../lib/bindings", () => ({ commands }));

function renderPane() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(<QueryClientProvider client={client}><ImportSourcesPane /></QueryClientProvider>);
}

afterEach(() => vi.clearAllMocks());

test("lists source status, refreshes and relocates a directory", async () => {
  renderPane();
  expect(await screen.findByText("D:\\素材")).toBeInTheDocument();
  expect(screen.getByText("3 项素材")).toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "刷新" }));
  await waitFor(() => expect(commands.refreshImportSource).toHaveBeenCalledWith("source-1"));
  fireEvent.click(screen.getByRole("button", { name: "重新定位" }));
  await waitFor(() => expect(commands.relocateImportSource).toHaveBeenCalledWith("source-1", "new-location"));
});

test("makes index deletion an explicit choice when removing a source", async () => {
  renderPane();
  fireEvent.click(await screen.findByRole("button", { name: "移除来源" }));
  const checkbox = screen.getByRole("checkbox", { name: "同时删除 3 条关联素材与索引" });
  expect(checkbox).not.toBeChecked();
  fireEvent.click(screen.getByRole("button", { name: "保留索引并移除" }));
  await waitFor(() => expect(commands.removeImportSource).toHaveBeenCalledWith("source-1", false));
});
