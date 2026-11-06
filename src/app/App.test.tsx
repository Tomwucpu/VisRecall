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

vi.mock("../lib/bindings", () => ({
  commands: {
    getAppStatus: vi.fn(async () => ({
      version: "0.1.0",
      libraryPath: "D:\\VisRecall\\library",
      libraryOk: true,
    })),
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
  },
}));

// Tauri 事件订阅在 jsdom 里没有运行时，直接给一个空实现。
vi.mock("../lib/events", () => ({ subscribeItemEvents: () => () => {} }));

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
  expect(await screen.findByText("待解析（阶段 03 提供）")).toBeInTheDocument();

  fireEvent.keyDown(window, { key: "Escape" });
  await waitFor(() => {
    expect(screen.queryByText("待解析（阶段 03 提供）")).not.toBeInTheDocument();
  });
  expect(screen.getByText("从中间选一条素材")).toBeInTheDocument();
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
