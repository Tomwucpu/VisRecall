import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { resetToasts } from "../../lib/toast";
import { ToastViewport } from "../shell/ToastViewport";
import { SettingsDialog } from "./SettingsDialog";

const openLibraryFolder = vi.hoisted(() => vi.fn(async () => null));
const getSettings = vi.hoisted(() =>
  vi.fn(async () => ({
    libraryPath: "D:\\VisRecall\\library",
    captureHotkey: "Ctrl+Shift+S",
    ballEnabled: true,
    ballSize: 48,
  })),
);

vi.mock("../../lib/bindings", () => ({
  commands: {
    getAppStatus: vi.fn(),
    listItems: vi.fn(),
    getItem: vi.fn(),
    getSettings,
    openLibraryFolder,
  },
}));

function renderDialog(onClose = () => {}) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={client}>
      <SettingsDialog onClose={onClose} />
      <ToastViewport />
    </QueryClientProvider>,
  );
}

afterEach(() => {
  resetToasts();
  vi.clearAllMocks();
});

test("shows only the capture and storage panes", () => {
  renderDialog();

  expect(screen.getByRole("button", { name: "捕获" })).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "存储" })).toBeInTheDocument();
  // 模型/隐私/关于属于阶段 03/04 与 V0.5，不进 DOM。
  for (const hidden of ["模型", "隐私", "关于"]) {
    expect(screen.queryByRole("button", { name: hidden })).not.toBeInTheDocument();
  }
});

test("reflects the real hotkey and ball configuration read from settings", async () => {
  renderDialog();

  expect(await screen.findByText("Ctrl")).toBeInTheDocument();
  expect(screen.getByText("Shift")).toBeInTheDocument();
  expect(screen.getByText("S")).toBeInTheDocument();

  const switchButton = screen.getByRole("switch", { name: "悬浮球显示" });
  expect(switchButton).toHaveAttribute("aria-checked", "true");
  // 写入能力未接入：控件禁用，避免死按钮。
  expect(switchButton).toBeDisabled();
});

test("opens the library folder through the real command and confirms with a toast", async () => {
  renderDialog();

  fireEvent.click(screen.getByRole("button", { name: "存储" }));
  const open = await screen.findByRole("button", { name: "打开" });
  fireEvent.click(open);

  await waitFor(() => expect(openLibraryFolder).toHaveBeenCalledTimes(1));
  expect(await screen.findByText("已在资源管理器中打开")).toBeInTheDocument();
  expect(screen.getByText("D:\\VisRecall\\library")).toBeInTheDocument();
});

test("marks capacity and retention as unavailable instead of faking values", async () => {
  renderDialog();
  fireEvent.click(screen.getByRole("button", { name: "存储" }));

  expect(await screen.findByText("用量统计待阶段 02 提供。")).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "永久" })).toBeDisabled();
  expect(screen.getByRole("button", { name: "更换" })).toBeDisabled();
});

test("closes on Escape and on the backdrop", async () => {
  const onClose = vi.fn();
  const { container } = renderDialog(onClose);

  const dialog = screen.getByRole("dialog");
  fireEvent.keyDown(dialog, { key: "Escape" });
  expect(onClose).toHaveBeenCalledTimes(1);

  const backdrop = container.querySelector(".modal");
  expect(backdrop).not.toBeNull();
  fireEvent.mouseDown(backdrop!);
  expect(onClose).toHaveBeenCalledTimes(2);
});
