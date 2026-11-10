import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import App from "./App";

const getItem = vi.hoisted(() =>
  vi.fn(async (id: string) => ({
    id,
    kind: "image",
    status: "ready",
    capturedAt: 1_700_000_000_000,
    sourceApp: "chrome.exe",
    sourceTitle: "第一条素材",
    sourceUrl: null,
    width: 800,
    height: 500,
    charCount: null,
    note: "",
    thumbUrl: null,
  })),
);

const commands = vi.hoisted(() => ({
    getAppStatus: vi.fn(async () => ({
      version: "0.1.0",
      libraryPath: "D:\\VisRecall\\library",
      libraryOk: true,
      restartRequired: false,
    })),
    restartApp: vi.fn(async () => undefined),
    listItems: vi.fn(async () => [
      {
        id: "item-1",
        kind: "image",
        status: "ready",
        capturedAt: 1_700_000_000_000,
        sourceApp: "chrome.exe",
        sourceTitle: "第一条素材",
        sourceUrl: null,
        width: 800,
        height: 500,
        charCount: null,
        note: "",
        thumbUrl: null,
      },
    ]),
    getItem,
    getSettings: vi.fn(async () => ({
      libraryPath: "D:\\VisRecall\\library",
      captureHotkey: "Ctrl+Shift+S",
      ballEnabled: true,
      ballSize: 48,
    })),
    openLibraryFolder: vi.fn(async () => null),
    // 详情面板接入真实命令后需要的读接口（阶段 02 Task 12 之前保持空实现）。
    getItemExt: vi.fn(async (id: string) => ({
      item: {
        id,
        kind: "image",
        status: "ready",
        capturedAt: 1_700_000_000_000,
        sourceApp: "chrome.exe",
        sourceTitle: "第一条素材",
        sourceUrl: null,
        width: 800,
        height: 500,
        charCount: null,
        note: "",
        thumbUrl: null,
      },
      extras: { summary: null, bodyText: null, originalUrl: null, tags: [] },
    })),
    noteSet: vi.fn(async () => undefined),
    tagsList: vi.fn(async () => []),
    tagAdd: vi.fn(async () => ({ id: "t1", name: "work", auto: false, hue: null, itemCount: 1 })),
    tagDetach: vi.fn(async () => undefined),
    tagRename: vi.fn(async () => undefined),
    tagDelete: vi.fn(async () => 0),
    copyItemOriginal: vi.fn(async () => undefined),
    itemDelete: vi.fn(async () => undefined),
    storageUsageGet: vi.fn(async () => ({
      usedBytes: 0,
      capacityBytes: 1,
      itemCount: 1,
    })),
    storageCapacitySet: vi.fn(async () => undefined),
    retentionSet: vi.fn(async () => undefined),
    retentionPreview: vi.fn(async () => ({ itemCount: 0, bytes: 0 })),
    retentionApply: vi.fn(async () => ({ deleted: 0, failed: 0 })),
    libraryPurgePreview: vi.fn(async () => ({ itemCount: 1, confirmationWord: "删除全部" })),
    libraryPurge: vi.fn(async () => undefined),
    storageOrphansGet: vi.fn(async () => ({ files: 0, bytes: 0, missingThumbs: 0 })),
    storageOrphansCleanup: vi.fn(async () => ({ files: 0, bytes: 0, missingThumbs: 0 })),
    storageRelocate: vi.fn(async () => ({ restartRequired: true, newRoot: "E:/new" })),
    listImportTasks: vi.fn(async () => []),
  }));

vi.mock("../lib/bindings", () => ({ commands }));

// Tauri 事件订阅在 jsdom 里没有运行时，直接给一个空实现。
vi.mock("../lib/events", () => ({ subscribeAppEvents: () => () => {} }));
vi.mock("../lib/notifications", () => ({ subscribeNotifications: () => () => {} }));
vi.mock("@tauri-apps/api/event", () => ({ listen: async () => () => {} }));

function renderApp() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={client}>
      <App />
    </QueryClientProvider>,
  );
}

afterEach(() => {
  window.localStorage.clear();
  delete document.documentElement.dataset.theme;
  vi.clearAllMocks();
});

test("opens the detail panel from a card and closes it with Escape", async () => {
  renderApp();

  const card = await screen.findByRole("button", { name: /第一条素材/ });
  fireEvent.click(card);
  expect(await screen.findByText("待解析")).toBeInTheDocument();

  fireEvent.keyDown(window, { key: "Escape" });
  await waitFor(() => {
    expect(screen.queryByText("待解析")).not.toBeInTheDocument();
  });
  expect(screen.getByText("从中间选一条素材")).toBeInTheDocument();
});

test("shows the library total from usage instead of the loaded page", async () => {
  commands.storageUsageGet.mockResolvedValueOnce({
    usedBytes: 0,
    capacityBytes: 1,
    itemCount: 42,
  });

  renderApp();

  // 列表只加载了 1 条，但计数取后端的库总量。
  expect(await screen.findByText("找到 42 项")).toBeInTheDocument();
  expect(await screen.findByText("42 条素材")).toBeInTheDocument();
});

test("keeps the add entry beside search in the normal library", async () => {
  renderApp();
  expect(await screen.findByRole("button", { name: "添加素材" })).toBeInTheDocument();
  expect(screen.getByRole("searchbox", { name: "搜索素材" })).toBeInTheDocument();
});

test("toggles the sidebar with B and the theme with T", async () => {
  const { container } = renderApp();
  await screen.findAllByText("本地索引已就绪");

  const sidebar = container.querySelector(".sidebar");
  expect(sidebar).not.toHaveClass("collapsed");
  fireEvent.keyDown(window, { key: "b" });
  expect(sidebar).toHaveClass("collapsed");

  const before = document.documentElement.dataset.theme;
  fireEvent.keyDown(window, { key: "t" });
  expect(document.documentElement.dataset.theme).not.toBe(before);
});

test("does not steal T or B while typing in the note field", async () => {
  renderApp();

  const card = await screen.findByRole("button", { name: /第一条素材/ });
  fireEvent.click(card);
  const note = await screen.findByLabelText("备注");

  const before = document.documentElement.dataset.theme;
  fireEvent.keyDown(note, { key: "t" });
  expect(document.documentElement.dataset.theme).toBe(before);

  const sidebar = document.querySelector(".sidebar");
  expect(sidebar).not.toHaveClass("collapsed");
  fireEvent.keyDown(note, { key: "b" });
  expect(sidebar).not.toHaveClass("collapsed");
});

test("opens settings from the title bar", async () => {
  renderApp();

  fireEvent.click(screen.getByRole("button", { name: "设置" }));
  expect(await screen.findByRole("dialog", { name: "捕获" })).toBeInTheDocument();
});

test("shows a persistent banner when a library migration awaits restart", async () => {
  commands.getAppStatus.mockResolvedValueOnce({
    version: "0.1.0",
    libraryPath: "E:/VisRecall/library",
    libraryOk: true,
    restartRequired: true,
  });

  renderApp();

  const banner = await waitFor(() => {
    const node = document.querySelector(".migration-banner");
    expect(node).not.toBeNull();
    return node as HTMLElement;
  });
  expect(banner).toHaveTextContent("库已迁移到 E:/VisRecall/library，重启后生效");

  fireEvent.click(screen.getByRole("button", { name: "立即重启" }));
  await waitFor(() => {
    expect(commands.restartApp).toHaveBeenCalledTimes(1);
  });
});

test("hides the banner when no restart is required", async () => {
  renderApp();
  await screen.findAllByText("本地索引已就绪");
  expect(document.querySelector(".migration-banner")).toBeNull();
});
