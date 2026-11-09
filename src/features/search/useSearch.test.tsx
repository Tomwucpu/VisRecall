import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

import { api } from "../../lib/api";
import type { SearchResponse } from "../../lib/searchTypes";
import { SEARCH_DEBOUNCE_MS, useSearch } from "./useSearch";

function response(raw: string, ids: string[]): SearchResponse {
  return {
    queryId: `q-${raw}`,
    parsed: {
      time: null,
      app: null,
      color: [],
      chart: null,
      contentKind: null,
      hasFormula: null,
      tags: [],
      semanticText: raw,
      chips: [],
    },
    semanticUsed: true,
    tookMs: 3,
    total: ids.length,
    hits: ids.map((id) => ({
      item: {
        id,
        kind: "image",
        status: "ready",
        capturedAt: 1,
        sourceApp: null,
        sourceTitle: id,
        sourceUrl: null,
        width: null,
        height: null,
        charCount: null,
        note: "",
        thumbUrl: null,
        primaryColor: null,
      },
      score: 1,
      explanation: { filters: [], terms: [raw], semantic: false, highlightBoxes: [] },
    })),
  };
}

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
});

test("empty query never calls the backend", async () => {
  const spy = vi.spyOn(api, "search");
  const { result } = renderHook(() => useSearch("   "));
  await act(async () => {
    vi.advanceTimersByTime(SEARCH_DEBOUNCE_MS * 3);
  });
  expect(spy).not.toHaveBeenCalled();
  expect(result.current.status).toBe("idle");
});

test("debounces typing into a single request", async () => {
  const spy = vi.spyOn(api, "search").mockResolvedValue(response("发票", ["a"]));
  const { rerender, result } = renderHook(({ raw }) => useSearch(raw), {
    initialProps: { raw: "发" },
  });

  rerender({ raw: "发票" });
  await act(async () => {
    vi.advanceTimersByTime(SEARCH_DEBOUNCE_MS - 10);
  });
  expect(spy).not.toHaveBeenCalled();

  await act(async () => {
    vi.advanceTimersByTime(20);
  });
  expect(spy).toHaveBeenCalledTimes(1);
  expect(result.current.hits).toHaveLength(1);
});

test("drops a stale response that arrives after newer input", async () => {
  let resolveFirst: ((value: SearchResponse) => void) | null = null;
  const spy = vi
    .spyOn(api, "search")
    .mockImplementationOnce(
      () =>
        new Promise<SearchResponse>((resolve) => {
          resolveFirst = resolve;
        }),
    )
    .mockResolvedValueOnce(response("发票", ["new"]));

  const { rerender, result } = renderHook(({ raw }) => useSearch(raw), {
    initialProps: { raw: "旧词" },
  });

  await act(async () => {
    vi.advanceTimersByTime(SEARCH_DEBOUNCE_MS);
  });
  // 第一个请求还挂着，用户继续输入
  rerender({ raw: "发票" });
  await act(async () => {
    vi.advanceTimersByTime(SEARCH_DEBOUNCE_MS);
  });
  expect(result.current.hits[0]?.item.id).toBe("new");

  // 迟到的旧响应不得覆盖新结果
  await act(async () => {
    resolveFirst?.(response("旧词", ["stale"]));
  });
  expect(result.current.hits[0]?.item.id).toBe("new");
  expect(spy).toHaveBeenCalledTimes(2);
});

test("surfaces backend errors and clears them on the next successful search", async () => {
  vi.spyOn(api, "search")
    .mockRejectedValueOnce({ code: "PROVIDER", message: "boom" })
    .mockResolvedValueOnce(response("发票", ["a"]));

  const { rerender, result } = renderHook(({ raw }) => useSearch(raw), {
    initialProps: { raw: "坏查询" },
  });
  await act(async () => {
    vi.advanceTimersByTime(SEARCH_DEBOUNCE_MS);
  });
  expect(result.current.status).toBe("error");
  expect(result.current.error).not.toBeNull();

  rerender({ raw: "发票" });
  await act(async () => {
    vi.advanceTimersByTime(SEARCH_DEBOUNCE_MS);
  });
  // mock 立即 resolve：`act` 已经冲刷完微任务，无需再等 waitFor（假定时器下它会卡住）
  expect(result.current.status).toBe("ready");
  expect(result.current.error).toBeNull();
  expect(result.current.hits[0]?.item.id).toBe("a");
});
