import { QueryClient } from "@tanstack/react-query";
import { subscribeAppEvents, subscribeItemEvents } from "./events";
import type { ItemCardData } from "../features/library/ItemCard";

function card(id: string, overrides: Partial<ItemCardData> = {}): ItemCardData {
  return {
    id,
    kind: "image",
    status: "ready",
    capturedAt: 1_700_000_000_000,
    sourceApp: "chrome.exe",
    sourceTitle: id,
    thumbUrl: null,
    ...overrides,
  };
}

type Pages = { pages: ItemCardData[][]; pageParams: (number | null)[] };

// 把 Tauri 的 `listen` 换成可观测的假实现：记录订阅名与处理器，退订可断言。
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

/** 触发所有同名处理器：StrictMode 场景下会同时存在「已退订」与「有效」两份。 */
function emit(name: string, payload: unknown = {}) {
  for (const entry of mocks.handlers.filter((item) => item.name === name)) {
    entry.handler({ payload });
  }
}

test("subscribes to every cache-affecting event", async () => {
  const { client } = makeClient();
  const cleanup = subscribeAppEvents(client);
  await flush();

  expect(mocks.listen.mock.calls.map(([name]) => name)).toEqual([
    "item_created",
    "item_updated",
    "item_deleted",
    "item_renamed",
    "items_deleted",
    "items_reset",
    "items_pruned",
    "item_note_updated",
    "item_content_updated",
    "item_tags_updated",
    "tags_changed",
    "hotkey_restored",
    "storage_migration_progress",
    "item_indexed",
    "item_enriched",
    "pipeline_status",
    "semantic_status",
    "import_task_progress",
    "import_task_finished",
  ]);

  cleanup();
  expect(mocks.unlisten).toHaveBeenCalledTimes(19);
});

test("import events refresh authoritative task and source queries", async () => {
  const { client, invalidate } = makeClient();
  const cleanup = subscribeAppEvents(client);
  await flush();

  emit("import_task_progress", { id: "task-1" });
  expect(invalidate).toHaveBeenCalledWith({ queryKey: ["import-task", "task-1"] });
  expect(invalidate).toHaveBeenCalledWith({ queryKey: ["import-tasks"] });
  expect(invalidate).not.toHaveBeenCalledWith({ queryKey: ["import-sources"] });

  emit("import_task_finished", { id: "task-1" });
  expect(invalidate).toHaveBeenCalledWith({ queryKey: ["import-sources"] });
  cleanup();
});

test("finished import tasks are cached from the event snapshot", async () => {
  const { client } = makeClient();
  const cleanup = subscribeAppEvents(client);
  await flush();

  const finished = {
    id: "task-1", sourceId: null, kind: "files", status: "completed_with_errors",
    totalCount: 1, processedCount: 1, succeededCount: 0, skippedCount: 0,
    failedCount: 1, originalBytes: 12, cancelRequested: false,
    errorSummary: null, createdAt: 1, updatedAt: 2,
  };
  const failure = {
    rowid: 1, entryKey: "0", relativePath: "a.txt", displayName: "a.txt", size: 12,
    status: "failed", errorCode: "IMPORT_FAILED", errorMessage: "读取失败", itemId: null,
  };
  emit("import_task_finished", { id: "task-1", task: finished, failures: [failure] });

  // 任务行在终态后已从库里删除，只能靠事件快照渲染结果。
  expect(client.getQueryData(["import-task", "task-1"])).toEqual(finished);
  expect(client.getQueryData(["import-task", "task-1", "failures"])).toEqual({
    entries: [failure],
    nextCursor: null,
  });
  cleanup();
});

test("list events invalidate lists without clearing cached pages", async () => {
  const { client, reset, invalidate } = makeClient();
  const cleanup = subscribeAppEvents(client);
  await flush();

  emit("item_created");
  expect(invalidate).toHaveBeenCalledWith({ queryKey: ["items"] });
  expect(invalidate).toHaveBeenCalledWith({ queryKey: ["items-filtered"] });
  expect(invalidate).toHaveBeenCalledWith({ queryKey: ["usage"] });
  // 清空确认词里的条数随库内容变化。
  expect(invalidate).toHaveBeenCalledWith({ queryKey: ["purge-preview"] });
  // 重置会丢分页并让列表闪白；列表事件只能失效，不能清空。
  expect(reset).not.toHaveBeenCalled();

  reset.mockClear();
  invalidate.mockClear();
  emit("items_pruned", { count: 3 });
  expect(invalidate).toHaveBeenCalledWith({ queryKey: ["items"] });
  expect(invalidate).toHaveBeenCalledWith({ queryKey: ["items-filtered"] });
  expect(invalidate).toHaveBeenCalledWith({ queryKey: ["usage"] });
  // 清理可能把某些标签的素材删光，标签云也要重取。
  expect(invalidate).toHaveBeenCalledWith({ queryKey: ["tags"] });
  expect(reset).not.toHaveBeenCalled();

  // 删素材可能留下孤儿文件，统计要重扫。
  invalidate.mockClear();
  emit("item_deleted", { id: "abc" });
  expect(invalidate).toHaveBeenCalledWith({ queryKey: ["orphans"] });
  expect(reset).not.toHaveBeenCalled();

  cleanup();
});

test("rename patches the card in place without refetching pages", async () => {
  const client = new QueryClient();
  const fetchCard = vi.fn(async (id: string) => card(id, { title: "新标题" }));
  const before: Pages = {
    pages: [[card("abc"), card("other")], [card("third")]],
    pageParams: [null, 1_700_000_000_000],
  };
  client.setQueryData(["items"], before);
  const untouched = before.pages[0][1];
  const reset = vi.spyOn(client, "resetQueries");

  const cleanup = subscribeAppEvents(client, fetchCard);
  await flush();
  emit("item_renamed", { id: "abc" });
  await flush();

  const data = client.getQueryData<Pages>(["items"])!;
  expect(data.pages[0][0]).toMatchObject({ id: "abc", title: "新标题" });
  expect(data.pages[0][1]).toBe(untouched);
  expect(data.pageParams).toEqual([null, 1_700_000_000_000]);
  expect(fetchCard).toHaveBeenCalledWith("abc");
  expect(reset).not.toHaveBeenCalled();

  cleanup();
});

test("item_updated patches only that card and keeps loaded pages", async () => {
  const client = new QueryClient();
  const fetchCard = vi.fn(async (id: string) =>
    card(id, { status: "parsing", thumbUrl: "http://visrecall.localhost/thumb/abc?rev=2" }),
  );
  const before: Pages = {
    pages: [[card("abc"), card("other")], [card("third")]],
    pageParams: [null, 1_700_000_000_000],
  };
  client.setQueryData(["items"], before);
  const untouched = before.pages[0][1];
  const reset = vi.spyOn(client, "resetQueries");

  const cleanup = subscribeAppEvents(client, fetchCard);
  await flush();
  emit("item_updated", { id: "abc", fields: ["status", "thumbUrl"] });
  await flush();

  const data = client.getQueryData<Pages>(["items"])!;
  expect(data.pages[0][0]).toMatchObject({
    id: "abc",
    status: "parsing",
    thumbUrl: "http://visrecall.localhost/thumb/abc?rev=2",
  });
  expect(data.pages[0][1]).toBe(untouched);
  expect(data.pages[1][0].id).toBe("third");
  expect(data.pageParams).toEqual([null, 1_700_000_000_000]);
  expect(reset).not.toHaveBeenCalled();

  cleanup();
});

test("item_deleted removes only that card from cached pages", async () => {
  const client = new QueryClient();
  const before: Pages = {
    pages: [[card("abc"), card("keep")], [card("third")]],
    pageParams: [null, 1_700_000_000_000],
  };
  client.setQueryData(["items"], before);
  const reset = vi.spyOn(client, "resetQueries");

  const cleanup = subscribeAppEvents(client, async () => null);
  await flush();
  emit("item_deleted", { id: "abc" });
  await flush();

  const data = client.getQueryData<Pages>(["items"])!;
  expect(data.pages.flat().map((item) => item.id)).toEqual(["keep", "third"]);
  expect(data.pageParams).toEqual([null, 1_700_000_000_000]);
  expect(reset).not.toHaveBeenCalled();

  cleanup();
});

test("batch delete invalidates lists once and refreshes counts", async () => {
  const { client, reset, invalidate } = makeClient();
  const cleanup = subscribeAppEvents(client);
  await flush();

  emit("items_deleted", { deleted: 2, failed: 1 });

  expect(invalidate).toHaveBeenCalledWith({ queryKey: ["items"] });
  expect(invalidate).toHaveBeenCalledWith({ queryKey: ["items-filtered"] });
  expect(invalidate).toHaveBeenCalledWith({ queryKey: ["usage"] });
  expect(invalidate).toHaveBeenCalledWith({ queryKey: ["orphans"] });
  expect(invalidate).toHaveBeenCalledWith({ queryKey: ["purge-preview"] });
  expect(invalidate).toHaveBeenCalledWith({ queryKey: ["tags"] });
  expect(reset).not.toHaveBeenCalled();

  cleanup();
});

test("note update only invalidates that detail", async () => {
  const { client, reset, invalidate } = makeClient();
  const cleanup = subscribeAppEvents(client);
  await flush();

  emit("item_note_updated", { id: "abc" });

  expect(invalidate).toHaveBeenCalledWith({ queryKey: ["detail", "abc"] });
  // 备注变化不得重置列表：否则用户正在浏览的列表会跳动。
  expect(reset).not.toHaveBeenCalled();

  cleanup();
});

test("content update invalidates the detail and refreshes the list only for body text", async () => {
  const { client, reset, invalidate } = makeClient();
  const cleanup = subscribeAppEvents(client);
  await flush();

  emit("item_content_updated", { id: "abc", field: "summary" });
  expect(invalidate).toHaveBeenCalledWith({ queryKey: ["detail", "abc"] });
  expect(invalidate).not.toHaveBeenCalledWith({ queryKey: ["items"] });
  expect(reset).not.toHaveBeenCalled();

  invalidate.mockClear();
  emit("item_content_updated", { id: "abc", field: "bodyText" });
  // 正文改变卡片预览：按 id 局部刷新，不能整表失效。
  expect(invalidate).toHaveBeenCalledWith({ queryKey: ["detail", "abc"] });
  expect(invalidate).not.toHaveBeenCalledWith({ queryKey: ["items"] });
  expect(reset).not.toHaveBeenCalled();

  cleanup();
});

test("body edit patches the preview from a single item fetch", async () => {
  const client = new QueryClient();
  const fetchCard = vi.fn(async (id: string) => card(id, { preview: "新正文" }));
  client.setQueryData<Pages>(["items"], {
    pages: [[card("abc"), card("other")]],
    pageParams: [null],
  });
  const untouched = client.getQueryData<Pages>(["items"])!.pages[0][1];

  const cleanup = subscribeAppEvents(client, fetchCard);
  await flush();
  emit("item_content_updated", { id: "abc", field: "bodyText" });
  await flush();

  const data = client.getQueryData<Pages>(["items"])!;
  expect(data.pages[0][0]).toMatchObject({ id: "abc", preview: "新正文" });
  expect(data.pages[0][1]).toBe(untouched);

  cleanup();
});

test("tag events invalidate the detail and the tag cloud", async () => {
  const { client, invalidate } = makeClient();
  const cleanup = subscribeAppEvents(client);
  await flush();

  emit("item_tags_updated", { id: "abc" });
  expect(invalidate).toHaveBeenCalledWith({ queryKey: ["detail", "abc"] });
  expect(invalidate).toHaveBeenCalledWith({ queryKey: ["tags"] });
  // 标签变化会改变「按标签筛选」的成员，筛选视图需要重取。
  expect(invalidate).toHaveBeenCalledWith({ queryKey: ["items-filtered"] });

  invalidate.mockClear();
  emit("tags_changed");
  expect(invalidate).toHaveBeenCalledWith({ queryKey: ["tags"] });
  expect(invalidate).toHaveBeenCalledTimes(1);

  cleanup();
});

test("note event without an id is ignored", async () => {
  const { client, invalidate } = makeClient();
  const cleanup = subscribeAppEvents(client);
  await flush();

  emit("item_note_updated", {});
  expect(invalidate).not.toHaveBeenCalled();

  cleanup();
});

test("settings and library status follow backend-side changes", async () => {
  const { client, invalidate } = makeClient();
  const cleanup = subscribeAppEvents(client);
  await flush();

  emit("hotkey_restored");
  expect(invalidate).toHaveBeenCalledWith({ queryKey: ["settings"] });

  invalidate.mockClear();
  emit("storage_migration_progress", { phase: "copy", done: 1, total: 2 });
  expect(invalidate).not.toHaveBeenCalled();

  emit("storage_migration_progress", { phase: "done", done: 0, total: 0 });
  expect(invalidate).toHaveBeenCalledWith({ queryKey: ["status"] });
  expect(invalidate).toHaveBeenCalledWith({ queryKey: ["usage"] });

  cleanup();
});

test("subscribeItemEvents keeps the phase 01 list-only behaviour", async () => {
  const { client, reset, invalidate } = makeClient();
  const cleanup = subscribeItemEvents(client);
  await flush();

  expect(mocks.listen).toHaveBeenCalledTimes(5);
  emit("item_created");
  expect(invalidate).toHaveBeenCalledWith({ queryKey: ["items"] });
  expect(reset).not.toHaveBeenCalled();
  // 新素材会改变用量，但不应动标签云。
  expect(invalidate).toHaveBeenCalledWith({ queryKey: ["usage"] });
  expect(invalidate).not.toHaveBeenCalledWith({ queryKey: ["tags"] });

  cleanup();
  expect(mocks.unlisten).toHaveBeenCalledTimes(5);
});

test("indexing events refresh the list without resetting it", async () => {
  const { client, reset, invalidate } = makeClient();
  const cleanup = subscribeAppEvents(client);
  await flush();

  emit("item_indexed", { id: "abc" });
  expect(reset).not.toHaveBeenCalled();
  // 编辑保存后的重索引回执：只按 id 更新该条，不整表失效。
  expect(invalidate).not.toHaveBeenCalledWith({ queryKey: ["items"] });
  expect(invalidate).toHaveBeenCalledWith({ queryKey: ["detail", "abc"] });

  invalidate.mockClear();
  emit("item_enriched", { id: "abc" });
  expect(reset).not.toHaveBeenCalled();
  expect(invalidate).not.toHaveBeenCalledWith({ queryKey: ["items"] });
  expect(invalidate).toHaveBeenCalledWith({ queryKey: ["detail", "abc"] });
  expect(invalidate).toHaveBeenCalledWith({ queryKey: ["tags"] });

  // 状态栏事件不该碰任何 Query 缓存
  invalidate.mockClear();
  reset.mockClear();
  emit("pipeline_status", { queued: 2, running: 1 });
  emit("semantic_status", { ready: false });
  expect(invalidate).not.toHaveBeenCalled();
  expect(reset).not.toHaveBeenCalled();

  cleanup();
});

test("strict-mode remount does not leave duplicate listeners", async () => {
  const { client, reset, invalidate } = makeClient();

  // StrictMode：挂载 → 立刻卸载 → 再挂载。
  subscribeItemEvents(client)();
  const cleanup = subscribeItemEvents(client);
  await flush();

  // 第一次挂载的五个在途订阅在解析后立即退订；第二次的保持有效。
  expect(mocks.unlisten).toHaveBeenCalledTimes(5);
  expect(mocks.listen).toHaveBeenCalledTimes(10);

  emit("item_created");
  expect(invalidate).toHaveBeenCalledWith({ queryKey: ["items"] });
  expect(reset).not.toHaveBeenCalled();

  cleanup();
  expect(mocks.unlisten).toHaveBeenCalledTimes(10);
});
