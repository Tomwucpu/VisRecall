import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { TitleBar } from "./TitleBar";

// jsdom 里没有 Tauri 运行时，窗口 API 换成可断言的桩。
const win = vi.hoisted(() => ({
  minimize: vi.fn(() => Promise.resolve()),
  toggleMaximize: vi.fn(() => Promise.resolve()),
  close: vi.fn(() => Promise.resolve()),
}));

vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => win,
}));

afterEach(() => {
  vi.clearAllMocks();
  window.localStorage.clear();
  delete document.documentElement.dataset.theme;
});

test("shows the library status and item count", () => {
  render(<TitleBar status="本地索引已就绪" itemCount={42} />);
  expect(screen.getByText("本地索引已就绪")).toBeInTheDocument();
  expect(screen.getByText("42 条素材")).toBeInTheDocument();
});

test("shows a loading hint instead of a count before the list is ready", () => {
  render(<TitleBar status="正在读取库状态" itemCount={null} />);
  expect(screen.getByText("素材读取中")).toBeInTheDocument();
});

test("marks only non-interactive containers as drag regions", () => {
  render(<TitleBar status="本地索引已就绪" itemCount={0} />);

  const header = document.querySelector(".titlebar");
  expect(header).toHaveAttribute("data-tauri-drag-region");

  // 中间那行文字整体是拖动区，否则从文字上按下拖不动窗口。
  const midSpans = document.querySelectorAll(".titlebar-mid > span[data-tauri-drag-region]");
  expect(midSpans.length).toBeGreaterThan(0);

  // 按钮绝不能带拖拽属性，否则点击会被当成拖动。
  for (const name of ["切换主题", "最小化", "最大化", "关闭"]) {
    expect(screen.getByRole("button", { name })).not.toHaveAttribute("data-tauri-drag-region");
  }
});

test("routes the window buttons to the current window API", async () => {
  render(<TitleBar status="本地索引已就绪" itemCount={0} />);

  fireEvent.click(screen.getByRole("button", { name: "最小化" }));
  fireEvent.click(screen.getByRole("button", { name: "最大化" }));
  fireEvent.click(screen.getByRole("button", { name: "关闭" }));

  await waitFor(() => {
    expect(win.minimize).toHaveBeenCalledTimes(1);
    expect(win.toggleMaximize).toHaveBeenCalledTimes(1);
    expect(win.close).toHaveBeenCalledTimes(1);
  });
});

test("toggles the theme and persists the choice", () => {
  render(<TitleBar status="本地索引已就绪" itemCount={0} />);

  const before = document.documentElement.dataset.theme;
  fireEvent.click(screen.getByRole("button", { name: "切换主题" }));

  const after = document.documentElement.dataset.theme;
  expect(after).not.toBe(before);
  expect(["light", "dark"]).toContain(after);
  expect(window.localStorage.getItem("visrecall.theme")).toBe(after);
});
