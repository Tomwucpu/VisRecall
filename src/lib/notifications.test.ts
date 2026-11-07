import { describeCaptureFailure, subscribeNotifications } from "./notifications";
import { getToastsSnapshot, resetToasts } from "./toast";

const mocks = vi.hoisted(() => ({
  listen: vi.fn(),
  unlisten: vi.fn(),
  handlers: [] as { name: string; handler: (event: { payload: unknown }) => void }[],
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: (name: string, handler: (event: { payload: unknown }) => void) => {
    mocks.handlers.push({ name, handler });
    return mocks.listen(name, handler);
  },
}));

beforeEach(() => {
  mocks.listen.mockReset();
  mocks.unlisten.mockReset();
  mocks.handlers.length = 0;
  mocks.listen.mockImplementation(async () => mocks.unlisten);
  resetToasts();
});

afterEach(() => {
  resetToasts();
});

function flush(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

function emit(name: string, payload: unknown = {}): void {
  for (const entry of mocks.handlers.filter((item) => item.name === name)) {
    entry.handler({ payload });
  }
}

test("subscribes to the three notification channels", async () => {
  const cleanup = subscribeNotifications();
  await flush();

  expect(mocks.listen.mock.calls.map(([name]) => name)).toEqual([
    "capture_failed",
    "hotkey_conflict",
    "app_recovered",
  ]);

  cleanup();
  expect(mocks.unlisten).toHaveBeenCalledTimes(3);
});

test("turns a capture failure code into a readable toast", async () => {
  const cleanup = subscribeNotifications();
  await flush();

  emit("capture_failed", { code: "CLIPBOARD_BUSY", message: "clipboard busy" });

  expect(getToastsSnapshot().map((toast) => toast.message)).toEqual([
    "剪贴板被其他程序占用，请重试",
  ]);

  cleanup();
});

test("keeps silent codes and unknown payloads quiet", async () => {
  const cleanup = subscribeNotifications();
  await flush();

  emit("capture_failed", { code: "CAPTURE_CANCELLED", message: "cancelled" });
  emit("capture_failed", { code: "THUMBNAIL_FAILED", message: "thumb" });
  expect(getToastsSnapshot()).toHaveLength(0);

  // 未知码给出兜底文案，而不是把英文内部信息抛给用户。
  emit("capture_failed", { code: "SOMETHING_NEW", message: "internal detail" });
  expect(getToastsSnapshot()[0]?.message).toBe("捕获失败（SOMETHING_NEW）");

  cleanup();
});

test("reports hotkey conflicts and recovered jobs", async () => {
  const cleanup = subscribeNotifications();
  await flush();

  emit("hotkey_conflict", { message: "Ctrl+Shift+S occupied" });
  emit("app_recovered", { count: 2 });
  emit("app_recovered", { count: 0 });

  const messages = getToastsSnapshot().map((toast) => toast.message);
  expect(messages).toEqual(["快捷键不可用：Ctrl+Shift+S occupied", "已恢复 2 个未完成任务"]);

  cleanup();
});

test("describeCaptureFailure ignores non-object payloads", () => {
  expect(describeCaptureFailure(null)).toBe("捕获失败");
  expect(describeCaptureFailure({ code: "STORAGE_FULL" })).toBe(
    "存储已满，请清理或调整容量",
  );
});
