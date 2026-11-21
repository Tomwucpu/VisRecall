import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
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
      startupMode: "silent",
      captureHotkey: "Ctrl+Shift+S",
      ballEnabled: true,
      ballSize: 56,
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
    itemDeleteMany: vi.fn(async () => ({ deleted: 1, failed: 0 })),
    itemRename: vi.fn(async (id: string) => ({
      id,
      kind: "image",
      status: "ready",
      capturedAt: 1_700_000_000_000,
      sourceApp: "chrome.exe",
      sourceTitle: "第一条素材",
      title: "新标题",
      sourceUrl: null,
      width: 800,
      height: 500,
      charCount: null,
      note: "",
      thumbUrl: null,
    })),
    listItemsFiltered: vi.fn(async () => [
      {
        id: "item-1",
        kind: "image",
        status: "ready",
        capturedAt: 1_700_000_000_000,
        sourceApp: "chrome.exe",
        sourceTitle: "第一条素材",
        title: null,
        sourceUrl: null,
        width: 800,
        height: 500,
        charCount: null,
        note: "",
        thumbUrl: null,
      },
    ]),
    listItemIdsFiltered: vi.fn(async () => ["item-1"]),
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
  expect(banner).toHaveTextContent("若设置为静默启动，重启后可从托盘打开主窗口。");

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

test("selection mode turns cards into checkboxes and batch delete asks for confirmation", async () => {
  renderApp();

  const toggle = await screen.findByRole("button", { name: "选择" });
  fireEvent.click(toggle);
  expect(toggle).toHaveTextContent("完成");

  const card = await screen.findByRole("checkbox");
  fireEvent.click(card);

  expect(await screen.findByText("已选 1 项")).toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "删除" }));

  const dialog = await screen.findByRole("dialog");
  expect(dialog).toHaveTextContent("删除 1 项素材？");
  fireEvent.click(within(dialog).getByRole("button", { name: "删除" }));

  await waitFor(() => {
    expect(commands.itemDeleteMany).toHaveBeenCalledWith(["item-1"]);
  });
});

test("choosing a kind filter queries the filtered endpoint instead of the plain list", async () => {
  renderApp();
  await screen.findByRole("button", { name: /第一条素材/ });

  fireEvent.click(screen.getByRole("button", { name: /类型/ }));
  fireEvent.click(screen.getByLabelText("图片", { exact: false }));

  await waitFor(() => {
    expect(commands.listItemsFiltered).toHaveBeenCalledWith(
      { kinds: ["image"], statuses: [], tags: [], timeFrom: null, timeTo: null },
      null,
      50,
    );
  });
});

test("renaming from the detail panel saves through itemRename", async () => {
  renderApp();

  const card = await screen.findByRole("button", { name: /第一条素材/ });
  fireEvent.click(card);
  // 卡片本身不再提供重命名入口，只有详情标题旁的铅笔按钮。
  const rename = await screen.findByRole("button", { name: "重命名" });
  fireEvent.click(rename);

  // 详情里已有备注输入框，重命名输入框必须限定在弹窗内查找。
  const dialog = await screen.findByRole("dialog", { name: "重命名素材" });
  const input = within(dialog).getByRole("textbox");
  fireEvent.change(input, { target: { value: "会议记录" } });
  fireEvent.click(within(dialog).getByRole("button", { name: "保存" }));

  await waitFor(() => {
    expect(commands.itemRename).toHaveBeenCalledWith("item-1", "会议记录");
  });
});

test("removes batch-deleted cards without blanking the grid", async () => {
  renderApp();

  await screen.findByRole("button", { name: /第一条素材/ });
  fireEvent.click(screen.getByRole("button", { name: "选择" }));
  fireEvent.click(await screen.findByRole("checkbox", { name: /第一条素材/ }));
  fireEvent.click(screen.getByRole("button", { name: "删除" }));

  const dialog = await screen.findByRole("dialog");
  fireEvent.click(within(dialog).getByRole("button", { name: "删除" }));

  await waitFor(() => {
    expect(screen.queryByRole("checkbox", { name: /第一条素材/ })).not.toBeInTheDocument();
  });
  // 删除后不得出现整块加载占位，也不得等待下一次列表重取才移除。
  expect(screen.queryByText("正在加载素材…")).not.toBeInTheDocument();
  expect(commands.itemDeleteMany).toHaveBeenCalledWith(["item-1"]);
});

test("keeps the open detail while the list is transiently empty", async () => {
  renderApp();

  fireEvent.click(await screen.findByRole("button", { name: /第一条素材/ }));
  expect(await screen.findByLabelText("备注")).toBeInTheDocument();

  // 输入搜索词后进入防抖窗口：检索结果暂时为空，但已打开的详情不能跟着卸载。
  fireEvent.change(screen.getByRole("searchbox", { name: "搜索素材" }), {
    target: { value: "会议" },
  });

  expect(screen.getByLabelText("备注")).toBeInTheDocument();
});

test("keeps previous cards visible while a filter query is loading", async () => {
  let resolveFiltered: (() => void) | undefined;
  commands.listItemsFiltered.mockImplementationOnce(
    () => new Promise((resolve) => { resolveFiltered = () => resolve([]); }),
  );

  renderApp();
  await screen.findByRole("button", { name: /第一条素材/ });

  fireEvent.click(screen.getByRole("button", { name: /类型/ }));
  fireEvent.click(screen.getByLabelText("图片", { exact: false }));

  // 新筛选还没返回时，旧结果继续可见，不出现整块加载占位。
  expect(screen.getByRole("button", { name: /第一条素材/ })).toBeInTheDocument();
  expect(screen.queryByText("正在加载素材…")).not.toBeInTheDocument();

  resolveFiltered?.();
});
