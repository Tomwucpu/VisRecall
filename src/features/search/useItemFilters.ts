import { useCallback, useMemo, useState } from "react";
import {
  customRange,
  EMPTY_FILTERS,
  filterChips,
  filtersKey,
  isFilterActive,
  presetRange,
  removeFilter,
  toApiFilters,
  toggleValue,
  type FilterState,
  type TimePreset,
} from "../../lib/filters";

/**
 * 筛选条状态：只存界面选择，派生值（IPC 契约 / 缓存键 / chip）每次都重算。
 *
 * 刻意不放 TanStack Query：筛选本身不是服务端状态，查询由 `useFilteredItems`
 * 或 `useSearch` 负责；这样「改条件」与「发请求」之间没有隐式的二次渲染。
 */
export function useItemFilters() {
  const [state, setState] = useState<FilterState>(EMPTY_FILTERS);

  const apiFilters = useMemo(() => toApiFilters(state), [state]);
  const key = useMemo(() => filtersKey(state), [state]);
  const chips = useMemo(() => filterChips(state), [state]);
  const active = isFilterActive(state);

  const toggleKind = useCallback((kind: string) => {
    setState((current) => ({ ...current, kinds: toggleValue(current.kinds, kind) }));
  }, []);

  const toggleStatus = useCallback((status: string) => {
    setState((current) => ({ ...current, statuses: toggleValue(current.statuses, status) }));
  }, []);

  const toggleTag = useCallback((tag: string) => {
    setState((current) => ({ ...current, tags: toggleValue(current.tags, tag) }));
  }, []);

  /** 选预设；`null` 表示清掉时间条件。 */
  const choosePreset = useCallback((preset: TimePreset | null) => {
    if (preset === null) {
      setState((current) => ({
        ...current,
        preset: null,
        timeFrom: null,
        timeTo: null,
        customFrom: "",
        customTo: "",
      }));
      return;
    }
    if (preset === "custom") {
      // 只切换面板，不外推区间：用户还没填日期前不应产生一个「今天」条件。
      setState((current) => ({ ...current, preset: "custom" }));
      return;
    }
    const range = presetRange(preset);
    setState((current) => ({
      ...current,
      preset,
      timeFrom: range.from,
      timeTo: range.to,
      customFrom: "",
      customTo: "",
    }));
  }, []);

  /** 自定义起止日期（任一端为空 = 该端不限）。 */
  const setCustomDates = useCallback((from: string, to: string) => {
    const range = customRange(from, to);
    setState((current) => ({
      ...current,
      preset: "custom",
      customFrom: from,
      customTo: to,
      timeFrom: range.timeFrom,
      timeTo: range.timeTo,
    }));
  }, []);

  const clear = useCallback(() => setState(EMPTY_FILTERS), []);

  const removeChip = useCallback((chipId: string) => {
    setState((current) => removeFilter(current, chipId));
  }, []);

  const counts = {
    kinds: state.kinds.length,
    statuses: state.statuses.length,
    tags: state.tags.length,
    time: state.timeFrom === null && state.timeTo === null ? 0 : 1,
  } as const;

  return {
    state,
    apiFilters,
    key,
    chips,
    active,
    counts,
    toggleKind,
    toggleStatus,
    toggleTag,
    choosePreset,
    setCustomDates,
    removeChip,
    clear,
  };
}

export type ItemFiltersController = ReturnType<typeof useItemFilters>;
