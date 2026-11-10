import { api, SUMMARY_PLACEHOLDER } from "./api";

/**
 * `api` 的职责只有一个：把参数原样传给对应命令。
 * 因此这里逐条断言「调了哪条命令、参数是什么」，不重复测试后端行为。
 */
const commands = vi.hoisted(() => ({
  getAppStatus: vi.fn(async () => ({
    version: "0.1.0",
    libraryPath: "D:\\VisRecall\\library",
    libraryOk: true,
    restartRequired: false,
  })),
  listItems: vi.fn(async () => []),
  openLibraryFolder: vi.fn(async () => null),
  getSettings: vi.fn(async () => ({
    libraryPath: "D:\\VisRecall\\library",
    captureHotkey: "Ctrl+Shift+S",
    ballEnabled: true,
    ballSize: 48,
  })),
  getItemExt: vi.fn(async (id: string) => ({
    item: { id },
    extras: { summary: null, bodyText: null, originalUrl: null, tags: [] },
  })),
  noteSet: vi.fn(async () => undefined),
  tagsList: vi.fn(async () => []),
  tagAdd: vi.fn(async () => ({ id: "t1", name: "work", auto: false, hue: "jade", itemCount: 1 })),
  tagDetach: vi.fn(async () => undefined),
  tagRename: vi.fn(async () => undefined),
  tagDelete: vi.fn(async () => 2),
  copyItemOriginal: vi.fn(async () => undefined),
  itemDelete: vi.fn(async () => undefined),
  storageUsageGet: vi.fn(async () => ({
    usedBytes: 1024,
    capacityBytes: 4096,
    itemCount: 3,
  })),
  storageCapacitySet: vi.fn(async () => undefined),
  retentionSet: vi.fn(async () => undefined),
  retentionPreview: vi.fn(async () => ({ itemCount: 2, bytes: 2048 })),
  retentionApply: vi.fn(async () => ({ deleted: 2, failed: 0 })),
  libraryPurgePreview: vi.fn(async () => ({ itemCount: 5, confirmationWord: "删除全部" })),
  libraryPurge: vi.fn(async () => undefined),
  storageOrphansGet: vi.fn(async () => ({ files: 1, bytes: 10, missingThumbs: 0 })),
  storageOrphansCleanup: vi.fn(async () => ({ files: 1, bytes: 10, missingThumbs: 0 })),
  storageRelocate: vi.fn(async () => ({ restartRequired: true, newRoot: "E:\\new" })),
}));

vi.mock("./bindings", () => ({ commands }));

beforeEach(() => {
  for (const spy of Object.values(commands)) {
    spy.mockClear();
  }
});

test("read commands forward their arguments", async () => {
  await api.listItems(123, 50);
  expect(commands.listItems).toHaveBeenCalledWith(123, 50);

  await api.loadDetail("abc");
  expect(commands.getItemExt).toHaveBeenCalledWith("abc");

  await api.getStatus();
  expect(commands.getAppStatus).toHaveBeenCalledTimes(1);

  await api.getSettings();
  expect(commands.getSettings).toHaveBeenCalledTimes(1);

  await api.openLibraryFolder();
  expect(commands.openLibraryFolder).toHaveBeenCalledTimes(1);

  await api.listTags();
  expect(commands.tagsList).toHaveBeenCalledTimes(1);

  await api.getStorageUsage();
  expect(commands.storageUsageGet).toHaveBeenCalledTimes(1);
});

test("note and tag writes use the documented commands", async () => {
  await api.saveNote("abc", "备注内容");
  expect(commands.noteSet).toHaveBeenCalledWith("abc", "备注内容");

  await api.addTag("abc", "work");
  expect(commands.tagAdd).toHaveBeenCalledWith("abc", "work");

  await api.removeTag("abc", "t1");
  expect(commands.tagDetach).toHaveBeenCalledWith("abc", "t1");

  await api.renameTag("t1", "job");
  expect(commands.tagRename).toHaveBeenCalledWith("t1", "job");

  await expect(api.deleteTag("t1")).resolves.toBe(2);
  expect(commands.tagDelete).toHaveBeenCalledWith("t1");
});

test("copy and delete take only the item id", async () => {
  // 后端按库里的 kind 自己分支；前端即使传了 kind 也不会透传。
  await api.copyOriginal("abc", "image");
  expect(commands.copyItemOriginal).toHaveBeenCalledWith("abc");

  await api.deleteItem("abc");
  expect(commands.itemDelete).toHaveBeenCalledWith("abc");
});

test("storage writes forward numbers and nulls", async () => {
  await api.setCapacity(50);
  expect(commands.storageCapacitySet).toHaveBeenCalledWith(50);

  await api.setRetention(90);
  expect(commands.retentionSet).toHaveBeenCalledWith(90);

  await api.setRetention(null);
  expect(commands.retentionSet).toHaveBeenLastCalledWith(null);

  await api.getRetentionPreview(null);
  expect(commands.retentionPreview).toHaveBeenCalledWith(null);

  await api.applyRetention();
  expect(commands.retentionApply).toHaveBeenCalledTimes(1);
});

test("destructive operations route through the server-side guard", async () => {
  const preview = await api.getPurgePreview();
  expect(preview.confirmationWord).toBe("删除全部");

  await api.purgeLibrary(preview.confirmationWord);
  expect(commands.libraryPurge).toHaveBeenCalledWith("删除全部");

  await api.getOrphans();
  expect(commands.storageOrphansGet).toHaveBeenCalledTimes(1);

  await api.cleanupOrphans();
  expect(commands.storageOrphansCleanup).toHaveBeenCalledTimes(1);
});

test("relocation reports that a restart is required", async () => {
  const result = await api.relocateStorage("E:\\new");
  expect(result.restartRequired).toBe(true);
  expect(commands.storageRelocate).toHaveBeenCalledWith("E:\\new");
});

test("exposes the single allowed AI summary placeholder", () => {
  expect(SUMMARY_PLACEHOLDER).toBe("待解析");
});
