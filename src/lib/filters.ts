/**
 * 结构化筛选的唯一换算层：界面状态 ↔ IPC 契约 ↔ chip 文案。
 *
 * 为什么单独成文件：`FilterBar`（编辑条件）、`ActiveFilterChips`（展示与移除）、
 * `useFilteredItems`（查询键）与 `useSearch`（检索参数）都要读同一份语义，
 * 分散到各组件里必然漂移。这里只放纯函数，不依赖 React。
 */
import type { ItemFilters } from "./api";
import { kindLabel, statusLabel } from "./format";

/** 时间预设；`custom` 由用户手填起止日期。 */
export type TimePreset = "today" | "7d" | "30d" | "year" | "custom";

/** 界面筛选状态：数组维度组内 OR、组间 AND（与后端 `ItemFilters` 一致）。 */
export type FilterState = {
  kinds: string[];
  statuses: string[];
  tags: string[];
  /** `null` = 不限时间。 */
  preset: TimePreset | null;
  /** 左闭右开（毫秒）；`preset` 非空时由 `presetRange` 算出。 */
  timeFrom: number | null;
  timeTo: number | null;
  /** 自定义起止日期（`YYYY-MM-DD`，本地时区），仅用于回填日期输入框。 */
  customFrom: string;
  customTo: string;
};

export type FilterChip = {
  /** 形如 `kind:image` / `status:ready` / `tag:工作` / `time`，移除时按它分支。 */
  id: string;
  label: string;
};

export type FilterPillKey = "kinds" | "time" | "tags" | "statuses";

export const EMPTY_FILTERS: FilterState = {
  kinds: [],
  statuses: [],
  tags: [],
  preset: null,
  timeFrom: null,
  timeTo: null,
  customFrom: "",
  customTo: "",
};

/** 类型选项（值必须与 `items.kind` 的 SQL 值一致）。 */
export const KIND_OPTIONS = ["image", "text", "link"] as const;

/** 状态选项（值必须与 `items.status` 的 SQL 值一致）。 */
export const STATUS_OPTIONS = ["parsing", "ready", "pending", "failed"] as const;

/** 时间预设按钮（自定义单独渲染日期输入）。 */
export const TIME_PRESET_OPTIONS: Array<{ key: TimePreset; label: string }> = [
  { key: "today", label: "今天" },
  { key: "7d", label: "近 7 天" },
  { key: "30d", label: "近 30 天" },
  { key: "year", label: "今年" },
];

const DAY_MS = 24 * 60 * 60 * 1000;

function startOfLocalDay(date: Date): number {
  return new Date(date.getFullYear(), date.getMonth(), date.getDate()).getTime();
}

/** `YYYY-MM-DD`（本地时区）→ 当天 0 点毫秒；非法/空返回 `null`。 */
export function dateInputToMs(value: string): number | null {
  const match = /^(\d{4})-(\d{2})-(\d{2})$/.exec(value.trim());
  if (match === null) {
    return null;
  }
  const year = Number(match[1]);
  const month = Number(match[2]);
  const day = Number(match[3]);
  const date = new Date(year, month - 1, day);
  if (Number.isNaN(date.getTime())) {
    return null;
  }
  return date.getTime();
}

/** 预设 → 左闭右开区间（本地时区语义，用户说「今天」就是本地的今天）。 */
export function presetRange(
  preset: TimePreset,
  now: Date = new Date(),
): { from: number; to: number } {
  const todayStart = startOfLocalDay(now);
  switch (preset) {
    case "today":
      return { from: todayStart, to: todayStart + DAY_MS };
    case "7d":
      return { from: todayStart - 6 * DAY_MS, to: todayStart + DAY_MS };
    case "30d":
      return { from: todayStart - 29 * DAY_MS, to: todayStart + DAY_MS };
    case "year":
      return {
        from: new Date(now.getFullYear(), 0, 1).getTime(),
        to: new Date(now.getFullYear() + 1, 0, 1).getTime(),
      };
    case "custom":
    default:
      return { from: todayStart, to: todayStart + DAY_MS };
  }
}

/** 自定义起止日期 → 左闭右开区间；只填一端也允许。 */
export function customRange(
  fromText: string,
  toText: string,
): { timeFrom: number | null; timeTo: number | null } {
  const from = dateInputToMs(fromText);
  const toDay = dateInputToMs(toText);
  return {
    timeFrom: from,
    // 「到 10 月 8 日」应包含当天，所以右边界取次日 0 点。
    timeTo: toDay === null ? null : toDay + DAY_MS,
  };
}

/** 切换数组里的一个值（多选语义）。 */
export function toggleValue(values: readonly string[], value: string): string[] {
  return values.includes(value)
    ? values.filter((existing) => existing !== value)
    : [...values, value];
}

/** 是否有任何生效条件（决定走普通列表还是筛选列表）。 */
export function isFilterActive(state: FilterState): boolean {
  return (
    state.kinds.length > 0 ||
    state.statuses.length > 0 ||
    state.tags.length > 0 ||
    state.timeFrom !== null ||
    state.timeTo !== null
  );
}

/** 当前生效条件数（pill 角标用；时间算一条）。 */
export function countForPill(state: FilterState, pill: FilterPillKey): number {
  switch (pill) {
    case "kinds":
      return state.kinds.length;
    case "statuses":
      return state.statuses.length;
    case "tags":
      return state.tags.length;
    case "time":
      return state.timeFrom === null && state.timeTo === null ? 0 : 1;
  }
}

/** 界面状态 → IPC 契约。 */
export function toApiFilters(state: FilterState): ItemFilters {
  return {
    kinds: state.kinds,
    statuses: state.statuses,
    tags: state.tags,
    timeFrom: state.timeFrom,
    timeTo: state.timeTo,
  };
}

/**
 * 筛选条件的稳定序列化键（查询缓存与检索依赖用）。
 *
 * 数组先排序：用户按不同顺序勾选同一组条件时应命中同一份缓存，
 * 否则 `useSearch` 会把「顺序变了」误判成「条件变了」而重新发请求。
 */
export function filtersKey(state: FilterState): string {
  return JSON.stringify({
    kinds: [...state.kinds].sort(),
    statuses: [...state.statuses].sort(),
    tags: [...state.tags].sort(),
    timeFrom: state.timeFrom,
    timeTo: state.timeTo,
  });
}

/** 时间条件的展示文案。 */
export function timeFilterLabel(state: FilterState): string | null {
  if (state.preset !== null && state.preset !== "custom") {
    return TIME_PRESET_OPTIONS.find((option) => option.key === state.preset)?.label ?? "时间";
  }
  if (state.preset === "custom") {
    const from = state.customFrom || "不限";
    const to = state.customTo || "不限";
    return `${from} 至 ${to}`;
  }
  if (state.timeFrom !== null || state.timeTo !== null) {
    return "自定义时间";
  }
  return null;
}

/** 生效条件 chip（每选中值一个，便于精确移除）。 */
export function filterChips(state: FilterState): FilterChip[] {
  const chips: FilterChip[] = [];
  for (const kind of state.kinds) {
    chips.push({ id: `kind:${kind}`, label: kindLabel(kind) });
  }
  const timeLabel = timeFilterLabel(state);
  if (timeLabel !== null) {
    chips.push({ id: "time", label: timeLabel });
  }
  for (const tag of state.tags) {
    chips.push({ id: `tag:${tag}`, label: `#${tag}` });
  }
  for (const status of state.statuses) {
    chips.push({ id: `status:${status}`, label: statusLabel(status) });
  }
  return chips;
}

/** 移除一个 chip；`time` 同时清掉预设与自定义日期。 */
export function removeFilter(state: FilterState, chipId: string): FilterState {
  if (chipId === "time") {
    return {
      ...state,
      preset: null,
      timeFrom: null,
      timeTo: null,
      customFrom: "",
      customTo: "",
    };
  }
  const [dimension, ...rest] = chipId.split(":");
  const value = rest.join(":");
  switch (dimension) {
    case "kind":
      return { ...state, kinds: state.kinds.filter((kind) => kind !== value) };
    case "status":
      return { ...state, statuses: state.statuses.filter((status) => status !== value) };
    case "tag":
      return { ...state, tags: state.tags.filter((tag) => tag !== value) };
    default:
      return state;
  }
}
