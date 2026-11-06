import { api, isUnsupported, SUMMARY_PLACEHOLDER } from "./api";

vi.mock("./bindings", () => ({
  commands: {
    getItem: vi.fn(async (id: string) => ({
      id,
      kind: "image",
      status: "ready",
      capturedAt: 1_700_000_000_000,
      sourceApp: "chrome.exe",
      sourceTitle: "Test",
      sourceUrl: null,
      width: 640,
      height: 400,
      charCount: null,
      note: "已存在的备注",
      thumbUrl: null,
    })),
  },
}));

test("merges the real item with explicit empty detail extras", async () => {
  const detail = await api.loadDetail("abc");

  expect(detail.item.note).toBe("已存在的备注");
  // stub 只返回「还没有」，绝不编造内容。
  expect(detail.extras).toEqual({
    summary: null,
    bodyText: null,
    originalUrl: null,
    tags: [],
  });
});

test("write stubs reject with a recognisable UNSUPPORTED code", async () => {
  await expect(api.saveNote("abc", "text")).rejects.toMatchObject({
    code: "UNSUPPORTED",
    message: "备注保存将在阶段 02 提供",
  });

  try {
    await api.deleteItem("abc");
    throw new Error("should have rejected");
  } catch (error) {
    expect(isUnsupported(error)).toBe(true);
  }
});

test("copy stub names the action that will exist later", async () => {
  await expect(api.copyOriginal("abc", "image")).rejects.toMatchObject({
    message: "复制原图将在阶段 02 提供",
  });
  await expect(api.copyOriginal("abc", "link")).rejects.toMatchObject({
    message: "复制链接将在阶段 02 提供",
  });
  await expect(api.copyOriginal("abc", "text")).rejects.toMatchObject({
    message: "复制原文将在阶段 02 提供",
  });
});

test("read stubs return empty values rather than invented data", async () => {
  await expect(api.listTags()).resolves.toEqual([]);
  await expect(api.getStorageUsage()).resolves.toBeNull();
});

test("exposes the single allowed AI summary placeholder", () => {
  expect(SUMMARY_PLACEHOLDER).toBe("待解析（阶段 03 提供）");
});
