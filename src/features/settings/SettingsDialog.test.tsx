import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { SettingsDto } from "../../lib/api";
import { resetToasts } from "../../lib/toast";
import { ToastViewport } from "../shell/ToastViewport";
import { SettingsDialog } from "./SettingsDialog";

const commands = vi.hoisted(() => ({
  getSettings: vi.fn(async () => ({
    libraryPath: "D:\\VisRecall\\library",
    startupMode: "silent" as SettingsDto["startupMode"],
    captureHotkey: "Ctrl+Shift+S",
    ballEnabled: true,
    ballSize: 56,
    ballEdgeHide: true,
    hideInFullscreen: true,
    capacityGb: 50,
    retentionDays: null as number | null,
    cloudImageAi: false,
    cloudTextAi: false,
  })),
  updateSettings: vi.fn(async (patch: Record<string, unknown>) => ({
    libraryPath: "D:\\VisRecall\\library",
    startupMode: (patch.startupMode as SettingsDto["startupMode"] | undefined) ?? "silent",
    captureHotkey: (patch.captureHotkey as string | undefined) ?? "Ctrl+Shift+S",
    ballEnabled: true,
    ballSize: (patch.ballSize as number | undefined) ?? 56,
    ballEdgeHide: true,
    hideInFullscreen: true,
    capacityGb: 50,
    retentionDays: null as number | null,
    cloudImageAi: false,
    cloudTextAi: false,
  })),
  cloudImageAiSet: vi.fn(async () => undefined),
  cloudTextAiSet: vi.fn(async () => undefined),
  openLibraryFolder: vi.fn(async () => null),
  storageUsageGet: vi.fn(async () => ({
    usedBytes: 1024 * 1024,
    capacityBytes: 50 * 1024 * 1024 * 1024,
    itemCount: 6,
  })),
  storageOrphansGet: vi.fn(async () => ({ files: 2, bytes: 2048, missingThumbs: 1 })),
  storageOrphansCleanup: vi.fn(async () => ({ files: 2, bytes: 2048, missingThumbs: 1 })),
  storageCapacitySet: vi.fn(async () => undefined),
  retentionSet: vi.fn(async () => undefined),
  retentionPreview: vi.fn(async () => ({ itemCount: 3, bytes: 4096 })),
  retentionApply: vi.fn(async () => ({ deleted: 3, failed: 0 })),
  libraryPurgePreview: vi.fn(async () => ({ itemCount: 6, confirmationWord: "删除全部" })),
  libraryPurge: vi.fn(async () => undefined),
  pickDirectory: vi.fn(async () => null),
  storageRelocate: vi.fn(async () => ({ restartRequired: true, newRoot: "E:/new" })),
  getAppStatus: vi.fn(),
}));

vi.mock("../../lib/bindings", () => ({ commands }));

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

test("shows the general, capture, storage and model panes", () => {
  renderDialog();

  expect(screen.getByRole("button", { name: "通用" })).toBeInTheDocument();

  expect(screen.getByRole("button", { name: "捕获" })).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "存储" })).toBeInTheDocument();
  // 阶段 03 新增「模型」pane（自定义模型列表与协议）
  expect(screen.getByRole("button", { name: "模型" })).toBeInTheDocument();
  for (const hidden of ["隐私", "关于"]) {
    expect(screen.queryByRole("button", { name: hidden })).not.toBeInTheDocument();
  }
});

function openStartupSettings() {
  renderDialog();
  fireEvent.click(screen.getByRole("button", { name: "通用" }));
}

test("selects the saved startup mode and explains when it takes effect", async () => {
  const get = commands.getSettings.getMockImplementation()!;
  commands.getSettings.mockImplementationOnce(async () => ({
    ...(await get()),
    startupMode: "show_window",
  }));
  openStartupSettings();

  expect(await screen.findByRole("radio", { name: "启动时显示主窗口" })).toBeChecked();
  expect(screen.getByRole("radio", { name: "静默启动" })).not.toBeChecked();
  expect(screen.getByText(/下次启动生效/)).toBeInTheDocument();
});

test("saves only the startup mode and disables both options until it is confirmed", async () => {
  const update = commands.updateSettings.getMockImplementation()!;
  let finishSave = () => {};
  commands.updateSettings.mockImplementationOnce(async (patch) => {
    await new Promise<void>((resolve) => { finishSave = resolve; });
    return update(patch);
  });
  openStartupSettings();

  const show = await screen.findByRole("radio", { name: "启动时显示主窗口" });
  const silent = screen.getByRole("radio", { name: "静默启动" });
  expect(silent).toBeChecked();
  fireEvent.click(show);

  expect(commands.updateSettings).toHaveBeenCalledExactlyOnceWith({ startupMode: "show_window" });
  expect(show).toBeDisabled();
  expect(silent).toBeDisabled();
  expect(silent).toBeChecked();
  finishSave();
  await waitFor(() => expect(show).toBeChecked());
  expect(show).toBeEnabled();
});

test("keeps the saved startup mode and reports a write error when saving fails", async () => {
  commands.updateSettings.mockRejectedValueOnce({ code: "IO", message: "磁盘不可写" });
  openStartupSettings();

  const show = await screen.findByRole("radio", { name: "启动时显示主窗口" });
  fireEvent.click(show);

  expect(await screen.findByText(/修改启动方式失败：磁盘不可写/)).toBeInTheDocument();
  expect(screen.getByRole("radio", { name: "静默启动" })).toBeChecked();
  expect(show).not.toBeChecked();
  expect(show).toBeEnabled();
});

test("offers a retry instead of a fake default when startup settings cannot be read", async () => {
  commands.getSettings.mockRejectedValueOnce({ code: "IO", message: "读取失败" });
  openStartupSettings();

  expect(await screen.findByRole("alert")).toHaveTextContent("无法读取启动方式");
  expect(screen.queryByRole("radio")).not.toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "重试" }));

  expect(await screen.findByRole("radio", { name: "静默启动" })).toBeChecked();
  expect(commands.getSettings).toHaveBeenCalledTimes(2);
});

test("reflects the real hotkey and ball configuration read from settings", async () => {
  renderDialog();

  expect(await screen.findByText("Ctrl")).toBeInTheDocument();
  expect(screen.getByText("Shift")).toBeInTheDocument();
  expect(screen.getByText("S")).toBeInTheDocument();

  const ballSlider = screen.getByRole("slider", { name: "悬浮球直径" });
  expect(ballSlider).toHaveValue("56");
  expect(screen.getByRole("switch", { name: "悬浮球显示" })).toHaveAttribute(
    "aria-checked",
    "true",
  );
  expect(screen.getByRole("switch", { name: "贴边收起" })).toHaveAttribute(
    "aria-checked",
    "true",
  );
  expect(screen.getByRole("switch", { name: "全屏时隐藏" })).toHaveAttribute(
    "aria-checked",
    "true",
  );
});

test("changes the ball size through update_settings", async () => {
  renderDialog();

  const slider = await screen.findByRole("slider", { name: "悬浮球直径" });
  fireEvent.change(slider, { target: { value: "100" } });

  await waitFor(() => {
    expect(commands.updateSettings).toHaveBeenCalledWith({ ballSize: 100 });
  });
});

test("records a new hotkey and shows it after the command succeeds", async () => {
  renderDialog();

  const modify = await screen.findByRole("button", { name: "修改" });
  fireEvent.click(modify);

  const cancel = screen.getByRole("button", { name: "取消" });
  fireEvent.keyDown(cancel, { key: "S", code: "KeyS", ctrlKey: true, altKey: true });

  await waitFor(() => {
    expect(commands.updateSettings).toHaveBeenCalledWith({ captureHotkey: "Ctrl+Alt+S" });
  });
});

test("keeps the capture row in edit mode when the backend rejects the hotkey", async () => {
  commands.updateSettings.mockRejectedValueOnce({
    code: "INVALID_INPUT",
    message: "hotkey occupied: Ctrl+Shift+S",
  });
  renderDialog();

  fireEvent.click(await screen.findByRole("button", { name: "修改" }));
  const cancel = screen.getByRole("button", { name: "取消" });
  fireEvent.keyDown(cancel, { key: "S", code: "KeyS", ctrlKey: true });

  expect(await screen.findByRole("alert")).toHaveTextContent("hotkey occupied");
  // 仍在输入态：按钮还是「取消」。
  expect(screen.getByRole("button", { name: "取消" })).toBeInTheDocument();
});

test("rejects an out-of-range capacity without calling the backend", async () => {
  renderDialog();
  fireEvent.click(screen.getByRole("button", { name: "存储" }));

  const input = await screen.findByLabelText("空间容量（GB）");
  fireEvent.change(input, { target: { value: "0" } });
  fireEvent.blur(input);

  expect(await screen.findByText(/容量需为 1–4096 GB 的整数/)).toBeInTheDocument();
  expect(commands.storageCapacitySet).not.toHaveBeenCalled();
});

test("asks for a preview before saving the retention policy", async () => {
  renderDialog();
  fireEvent.click(screen.getByRole("button", { name: "存储" }));

  fireEvent.click(await screen.findByRole("button", { name: "90 天" }));

  const dialog = await screen.findByRole("dialog", { name: /保留最近 90 天/ });
  expect(dialog).toHaveTextContent("将移除 3 条");
  expect(commands.retentionSet).not.toHaveBeenCalled();

  fireEvent.click(screen.getByRole("button", { name: "保存策略" }));
  await waitFor(() => {
    expect(commands.retentionSet).toHaveBeenCalledWith(90);
  });
});

test("cancelling the retention preview does not write anything", async () => {
  renderDialog();
  fireEvent.click(screen.getByRole("button", { name: "存储" }));

  fireEvent.click(await screen.findByRole("button", { name: "30 天" }));
  fireEvent.click(await screen.findByRole("button", { name: "取消" }));

  expect(commands.retentionSet).not.toHaveBeenCalled();
});

test("keeps the purge button disabled until the confirmation word matches", async () => {
  renderDialog();
  fireEvent.click(screen.getByRole("button", { name: "存储" }));

  const input = await screen.findByLabelText("输入确认词");
  const submit = screen.getByRole("button", { name: "清空素材库" });
  expect(submit).toBeDisabled();

  fireEvent.change(input, { target: { value: "删除" } });
  expect(submit).toBeDisabled();

  fireEvent.change(input, { target: { value: "删除全部" } });
  await waitFor(() => expect(submit).toBeEnabled());

  fireEvent.click(submit);
  await waitFor(() => {
    expect(commands.libraryPurge).toHaveBeenCalledWith("删除全部");
  });
});

test("shows orphan stats and cleans them after confirmation", async () => {
  renderDialog();
  fireEvent.click(screen.getByRole("button", { name: "存储" }));

  expect(await screen.findByText(/2 个 \/ 2\.0 KB/)).toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "清理孤儿文件" }));
  fireEvent.click(await screen.findByRole("button", { name: "清理" }));

  await waitFor(() => {
    expect(commands.storageOrphansCleanup).toHaveBeenCalledTimes(1);
  });
});

test("mentions that database files must not be moved on their own", async () => {
  renderDialog();
  fireEvent.click(screen.getByRole("button", { name: "存储" }));

  expect(
    await screen.findByText(/visrecall\.db \/ visrecall\.db-wal \/ visrecall\.db-shm 属于同一个数据库/),
  ).toBeInTheDocument();
});

test("opens the library folder through the real command and confirms with a toast", async () => {
  renderDialog();
  fireEvent.click(screen.getByRole("button", { name: "存储" }));

  const open = await screen.findByRole("button", { name: "打开" });
  fireEvent.click(open);

  await waitFor(() => expect(commands.openLibraryFolder).toHaveBeenCalledTimes(1));
  expect(await screen.findByText("已在资源管理器中打开")).toBeInTheDocument();
  expect(screen.getByText("D:\\VisRecall\\library")).toBeInTheDocument();
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
