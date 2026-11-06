/*
 * 主题机制（D-UI-2）：默认跟随系统，手动切换后持久化。
 *
 * 解析优先级：本地保存的显式选择 > 系统 `prefers-color-scheme`。
 * 显式选择只在用户点击切换时写入（`system` 会删除该键），因此首次启动必然是跟随系统。
 * CSS 侧同样以 `prefers-color-scheme` 兜底首帧（见 tokens.css），JS 只负责写 `data-theme`。
 */
import { useCallback, useSyncExternalStore } from "react";

export type ThemeMode = "system" | "light" | "dark";
export type ResolvedTheme = "light" | "dark";

export const THEME_STORAGE_KEY = "visrecall.theme";

const DARK_QUERY = "(prefers-color-scheme: dark)";
/** 切换时短暂挂上的过渡类，让令牌替换有 340ms 的渐变而不是硬跳。 */
const TRANSITION_CLASS = "theming";
const TRANSITION_MS = 400;

/** 纯函数：模式 + 系统偏好 → 实际主题。便于单测，不读 DOM。 */
export function resolveTheme(mode: ThemeMode, systemPrefersDark: boolean): ResolvedTheme {
  if (mode === "system") {
    return systemPrefersDark ? "dark" : "light";
  }
  return mode;
}

/** 存储值 → 模式；任何非法值（含旧版本残留）都退回 `system`。 */
export function parseThemeMode(raw: string | null): ThemeMode {
  return raw === "light" || raw === "dark" || raw === "system" ? raw : "system";
}

function readStoredMode(): ThemeMode {
  try {
    return parseThemeMode(window.localStorage.getItem(THEME_STORAGE_KEY));
  } catch {
    // 存储不可用（策略禁用/隐私模式）时不影响本会话的切换。
    return "system";
  }
}

function writeStoredMode(mode: ThemeMode): void {
  try {
    if (mode === "system") {
      window.localStorage.removeItem(THEME_STORAGE_KEY);
    } else {
      window.localStorage.setItem(THEME_STORAGE_KEY, mode);
    }
  } catch {
    // 同上：持久化失败只是丢失偏好，不抛错。
  }
}

function systemPrefersDark(): boolean {
  return typeof window.matchMedia === "function" && window.matchMedia(DARK_QUERY).matches;
}

/** 把主题写到 `<html data-theme>`；`animate` 仅用于用户主动切换。 */
export function applyTheme(theme: ResolvedTheme, animate = false): void {
  const root = document.documentElement;
  if (animate) {
    root.classList.add(TRANSITION_CLASS);
    window.setTimeout(() => root.classList.remove(TRANSITION_CLASS), TRANSITION_MS);
  }
  root.dataset.theme = theme;
}

let mode: ThemeMode = "system";
let started = false;
const listeners = new Set<() => void>();

function currentTheme(): ResolvedTheme {
  return resolveTheme(mode, systemPrefersDark());
}

function notify(): void {
  for (const listener of listeners) {
    listener();
  }
}

/**
 * 启动主题：读偏好、写 DOM、在「跟随系统」时监听媒体查询变化。
 * 幂等，`main.tsx` 在首次渲染前调用一次即可。
 */
export function startTheme(): void {
  if (started) {
    return;
  }
  started = true;
  mode = readStoredMode();
  applyTheme(currentTheme());

  if (typeof window.matchMedia === "function") {
    window.matchMedia(DARK_QUERY).addEventListener("change", () => {
      if (mode === "system") {
        applyTheme(currentTheme());
        notify();
      }
    });
  }
}

export function getThemeMode(): ThemeMode {
  return mode;
}

export function getResolvedTheme(): ResolvedTheme {
  return currentTheme();
}

/** 设置模式；`system` 会清掉持久化键，回到跟随系统。 */
export function setThemeMode(next: ThemeMode): void {
  if (next === mode) {
    return;
  }
  mode = next;
  writeStoredMode(next);
  applyTheme(currentTheme(), true);
  notify();
}

/** 手动切换：以当前解析结果取反，并把结果固化为显式选择。 */
export function toggleTheme(): void {
  setThemeMode(currentTheme() === "dark" ? "light" : "dark");
}

export function subscribeTheme(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

/** 订阅当前解析出的主题；供标题栏的主题按钮与图标使用。 */
export function useTheme(): {
  mode: ThemeMode;
  theme: ResolvedTheme;
  toggle: () => void;
} {
  const theme = useSyncExternalStore(subscribeTheme, getResolvedTheme, getResolvedTheme);
  const toggle = useCallback(() => {
    toggleTheme();
  }, []);
  return { mode, theme, toggle };
}
