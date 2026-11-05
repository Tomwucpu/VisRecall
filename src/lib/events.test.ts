import { QueryClient } from "@tanstack/react-query";
import { subscribeItemEvents } from "./events";

// 把 Tauri 的 `listen` 换成可观测的假实现：记录订阅名与处理器，退订可断言。
const mocks = vi.hoisted(() => ({
  listen: vi.fn(),
  unlisten: vi.fn(),
  handlers: [] as { name: string; handler: () => void }[],
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: (name: string, handler: () => void) => {
    mocks.handlers.push({ name, handler });
    return mocks.listen(name, handler);
  },
}));

beforeEach(() => {
  mocks.listen.mockReset();
  mocks.unlisten.mockReset();
  mocks.handlers.length = 0;
  mocks.listen.mockImplementation(async () => mocks.unlisten);
});

/** 等 `listen(...).then(...)` 这一轮微任务跑完。 */
function flush(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

function makeClient() {
  const client = new QueryClient();
  const reset = vi.spyOn(client, "resetQueries").mockResolvedValue(undefined);
  const invalidate = vi.spyOn(client, "invalidateQueries").mockResolvedValue(undefined);
  return { client, reset, invalidate };
}

test("resets the items query on item events instead of invalidating pages", async () => {
  const { client, reset, invalidate } = makeClient();
  const cleanup = subscribeItemEvents(client);
  await flush();

  expect(mocks.listen).toHaveBeenCalledTimes(2);
  expect(mocks.listen.mock.calls.map(([name]) => name)).toEqual([
    "item_created",
    "item_updated",
  ]);

  mocks.handlers.find((entry) => entry.name === "item_created")?.handler();
  expect(reset).toHaveBeenCalledTimes(1);
  expect(reset).toHaveBeenCalledWith({ queryKey: ["items"] });
  expect(invalidate).not.toHaveBeenCalled();

  cleanup();
  expect(mocks.unlisten).toHaveBeenCalledTimes(2);
});

test("strict-mode remount does not leave duplicate listeners", async () => {
  const { client, reset } = makeClient();

  // StrictMode：挂载 → 立刻卸载 → 再挂载。
  subscribeItemEvents(client)();
  const cleanup = subscribeItemEvents(client);
  await flush();

  // 第一次挂载的两个在途订阅在解析后立即退订；第二次的保持有效。
  expect(mocks.unlisten).toHaveBeenCalledTimes(2);
  expect(mocks.listen).toHaveBeenCalledTimes(4);

  // 已清理的处理器不再触发重置，只有第二次挂载的那一个生效。
  for (const { name, handler } of mocks.handlers) {
    if (name === "item_created") {
      handler();
    }
  }
  expect(reset).toHaveBeenCalledTimes(1);

  cleanup();
  expect(mocks.unlisten).toHaveBeenCalledTimes(4);
});
