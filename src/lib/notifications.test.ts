import {
  describeCaptureFailure,
  describeJobFailure,
  resetJobFailureDedupe,
  subscribeNotifications,
} from "./notifications";
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
  resetJobFailureDedupe();
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

test("subscribes to the four notification channels", async () => {
  const cleanup = subscribeNotifications();
  await flush();

  expect(mocks.listen.mock.calls.map(([name]) => name)).toEqual([
    "capture_failed",
    "hotkey_conflict",
    "app_recovered",
    "job_failed",
  ]);

  cleanup();
  expect(mocks.unlisten).toHaveBeenCalledTimes(4);
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

test("describeJobFailure maps stage and code to readable text", () => {
  expect(
    describeJobFailure({
      itemId: "i1",
      stage: "vlm",
      code: "PROVIDER_CONFIG",
      message: "HTTP 401 (检查 API Key 与 base URL)",
      model: "my-vision-model",
    }),
  ).toBe("「my-vision-model」摘要与描述生成失败：密钥或地址配置错误，请在 设置 → 模型 中检查");

  expect(
    describeJobFailure({
      itemId: "i1",
      stage: "vlm",
      code: "PROVIDER",
      message: "timeout after 30s",
      model: "my-vision-model",
    }),
  ).toBe("「my-vision-model」摘要与描述生成失败：timeout after 30s");

  expect(
    describeJobFailure({
      itemId: "i1",
      stage: "vlm",
      code: "SCHEMA_VIOLATION",
      message: "missing summary",
      model: "my-vision-model",
    }),
  ).toBe("「my-vision-model」摘要与描述生成失败：模型返回格式异常");

  // 非云端阶段没有模型名；未知码兜底
  expect(
    describeJobFailure({
      itemId: "i1",
      stage: "index",
      code: "MODEL_NOT_READY",
      message: "bge",
      model: null,
    }),
  ).toBe("向量索引失败：本地模型未就绪");

  expect(describeJobFailure(null)).toBe("后台任务失败");
  expect(describeJobFailure({ stage: "vlm", code: "SOMETHING_NEW" })).toBe(
    "摘要与描述生成失败：失败（SOMETHING_NEW）",
  );
});

test("job failures toast once per identical text inside the dedupe window", async () => {
  const cleanup = subscribeNotifications();
  await flush();

  const payload = {
    itemId: "i1",
    stage: "vlm",
    code: "PROVIDER",
    message: "HTTP 502",
    model: "my-vision-model",
  };
  emit("job_failed", payload);
  emit("job_failed", { ...payload, itemId: "i2" });

  const messages = getToastsSnapshot().map((toast) => toast.message);
  expect(messages).toEqual(["「my-vision-model」摘要与描述生成失败：HTTP 502"]);

  // 不同文案不被吞掉
  emit("job_failed", { ...payload, message: "HTTP 503" });
  expect(getToastsSnapshot()).toHaveLength(2);

  cleanup();
});
