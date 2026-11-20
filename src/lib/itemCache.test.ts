import { QueryClient } from "@tanstack/react-query";
import { patchItemEverywhere, removeItemsEverywhere } from "./itemCache";
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

function seed(client: QueryClient, key: readonly unknown[], data: Pages) {
  client.setQueryData(key, data);
}

function read(client: QueryClient, key: readonly unknown[]): Pages {
  return client.getQueryData<Pages>(key)!;
}

test("patches one card across both list views and keeps page params", () => {
  const client = new QueryClient();
  const list = {
    pages: [[card("a"), card("b")], [card("c")]],
    pageParams: [null, 1_700_000_000_000],
  };
  const filtered = {
    pages: [[card("b"), card("c")]],
    pageParams: [null],
  };
  seed(client, ["items"], list);
  seed(client, ["items-filtered", "kind=image"], filtered);
  const untouched = list.pages[0][0];

  patchItemEverywhere(client, card("b", { status: "parsing", title: "新标题" }));

  const next = read(client, ["items"]);
  expect(next.pageParams).toEqual([null, 1_700_000_000_000]);
  expect(next.pages[0][1]).toMatchObject({ id: "b", status: "parsing", title: "新标题" });
  // 未命中的卡片保持原引用，配合结构共享不会触发重渲染。
  expect(next.pages[0][0]).toBe(untouched);

  expect(read(client, ["items-filtered", "kind=image"]).pages[0][0]).toMatchObject({
    id: "b",
    status: "parsing",
  });
});

test("patch of an unloaded id returns the same cache reference", () => {
  const client = new QueryClient();
  const data = { pages: [[card("a")]], pageParams: [null] };
  seed(client, ["items"], data);

  patchItemEverywhere(client, card("zzz"));

  expect(read(client, ["items"])).toBe(data);
});

test("removes deleted ids from every page and keeps page params", () => {
  const client = new QueryClient();
  seed(client, ["items"], {
    pages: [[card("a"), card("b")], [card("c"), card("d")]],
    pageParams: [null, 1_700_000_000_000],
  });
  seed(client, ["items-filtered", "kind=image"], {
    pages: [[card("b"), card("d")]],
    pageParams: [null],
  });

  removeItemsEverywhere(client, new Set(["b", "d"]));

  const next = read(client, ["items"]);
  expect(next.pageParams).toEqual([null, 1_700_000_000_000]);
  expect(next.pages.flat().map((item) => item.id)).toEqual(["a", "c"]);
  expect(read(client, ["items-filtered", "kind=image"]).pages.flat()).toEqual([]);
});

test("removing ids that are not loaded returns the same cache reference", () => {
  const client = new QueryClient();
  const data = { pages: [[card("a")]], pageParams: [null] };
  seed(client, ["items"], data);

  removeItemsEverywhere(client, new Set(["zzz"]));

  expect(read(client, ["items"])).toBe(data);
});
