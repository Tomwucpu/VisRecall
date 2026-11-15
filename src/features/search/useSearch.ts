import { useEffect, useMemo, useRef, useState } from "react";
import { api, type ItemFilters } from "../../lib/api";
import { describeError } from "../../lib/errors";
import type { SearchResponse, SearchStatus } from "../../lib/searchTypes";

/** 搜索防抖（requirements P3-R09：250ms）。 */
export const SEARCH_DEBOUNCE_MS = 250;
/** 一次取多少条（10 条/屏 × 5，配合滚动加载）。 */
export const SEARCH_LIMIT = 50;

type State = {
  /** 当前展示的结果（`null` = 还没搜过）。 */
  response: SearchResponse | null;
  status: SearchStatus;
  error: string | null;
  /** 正在等待防抖或请求返回（用于结果区顶部细进度条）。 */
  busy: boolean;
};

const IDLE: State = { response: null, status: "idle", error: null, busy: false };

/**
 * 搜索状态机：防抖 + **过期响应丢弃**（P3-R09）。
 *
 * 「过期丢弃」用自增序号实现而不是 `AbortSignal`：IPC 无法真正取消，
 * 后端已经在算了，能做的只是别把旧结果盖到新结果上。序号在每次输入时 +1，
 * 响应回来时只有序号仍是最新的才允许写入 state。
 */
export function useSearch(raw: string, filters: ItemFilters | null = null, filterKey = "") {
  const [state, setState] = useState<State>(IDLE);
  const trimmed = raw.trim();
  const seq = useRef(0);
  // 筛选对象每次渲染都可能换引用；effect 依赖稳定的 `filterKey`，值走 ref 取最新。
  const filtersRef = useRef(filters);
  filtersRef.current = filters;

  useEffect(() => {
    if (trimmed.length === 0) {
      seq.current += 1; // 让在途响应过期
      setState(IDLE);
      return;
    }

    const current = (seq.current += 1);
    setState((prev) => ({ ...prev, busy: true, error: null }));

    const timer = window.setTimeout(() => {
      api
        .search(trimmed, SEARCH_LIMIT, filtersRef.current)
        .then((response) => {
          if (current !== seq.current) {
            return;
          }
          setState({ response, status: "ready", error: null, busy: false });
        })
        .catch((error: unknown) => {
          if (current !== seq.current) {
            return;
          }
          setState({ response: null, status: "error", error: describeError(error), busy: false });
        });
    }, SEARCH_DEBOUNCE_MS);

    return () => window.clearTimeout(timer);
  }, [trimmed, filterKey]);

  const hits = useMemo(() => state.response?.hits ?? [], [state.response]);

  return { ...state, hits };
}
