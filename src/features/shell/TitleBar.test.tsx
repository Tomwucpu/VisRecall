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

test("shows only the VISRECALL wordmark in the top-left brand area", () => {
  render(<TitleBar onOpenSettings={() => {}} />);
  const brand = document.querySelector(".brand");
  expect(brand).toHaveTextContent("VISRECALL");
  expect(brand).not.toHaveTextContent("目记");
});

test("marks only non-interactive containers as drag regions", () => {
  render(<TitleBar onOpenSettings={() => {}} />);

  const header = document.querySelector(".titlebar");
  expect(header).toHaveAttribute("data-tauri-drag-region");

  // 字标本身是拖动区，否则从文字上按下拖不动窗口。
  const brandParts = document.querySelectorAll(".brand > span[data-tauri-drag-region]");
  expect(brandParts.length).toBe(1);

  // 按钮绝不能带拖拽属性，否则点击会被当成拖动。
  for (const name of ["切换主题", "设置", "最小化", "最大化", "关闭"]) {
    expect(screen.getByRole("button", { name })).not.toHaveAttribute("data-tauri-drag-region");
  }
});

test("opens settings through the title bar entry", () => {
  const onOpenSettings = vi.fn();
  render(<TitleBar onOpenSettings={onOpenSettings} />);

  fireEvent.click(screen.getByRole("button", { name: "设置" }));
  expect(onOpenSettings).toHaveBeenCalledTimes(1);
});

test("routes the window buttons to the current window API", async () => {
  render(<TitleBar onOpenSettings={() => {}} />);

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
  render(<TitleBar onOpenSettings={() => {}} />);

  const before = document.documentElement.dataset.theme;
  fireEvent.click(screen.getByRole("button", { name: "切换主题" }));

  const after = document.documentElement.dataset.theme;
  expect(after).not.toBe(before);
  expect(["light", "dark"]).toContain(after);
  expect(window.localStorage.getItem("visrecall.theme")).toBe(after);
});
