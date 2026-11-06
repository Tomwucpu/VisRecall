/**
 * 主题机制单测：解析规则（D-UI-2）、持久化、系统偏好联动。
 * 模块内有单例状态（mode/started），每个用例用 `vi.resetModules()` 重新导入。
 */

function stubMatchMedia(initialDark: boolean) {
  const listeners = new Set<(event: MediaQueryListEvent) => void>();
  let dark = initialDark;

  // `matches` 必须是会变的 getter：`startTheme` 只注册一次监听，
  // 之后每次读系统偏好都会重新调 `matchMedia`。
  window.matchMedia = ((query: string) => ({
    get matches() {
      return dark;
    },
    media: query,
    onchange: null,
    addEventListener: (_type: string, listener: (event: MediaQueryListEvent) => void) => {
      listeners.add(listener);
    },
    removeEventListener: (_type: string, listener: (event: MediaQueryListEvent) => void) => {
      listeners.delete(listener);
    },
    addListener: () => {},
    removeListener: () => {},
    dispatchEvent: () => false,
  })) as unknown as typeof window.matchMedia;

  return {
    emit(nextDark: boolean) {
      dark = nextDark;
      for (const listener of listeners) {
        listener({ matches: dark } as MediaQueryListEvent);
      }
    },
  };
}

async function loadTheme() {
  vi.resetModules();
  return import("./theme");
}

beforeEach(() => {
  window.localStorage.clear();
  delete document.documentElement.dataset.theme;
});

afterEach(() => {
  vi.restoreAllMocks();
});

test("resolves system mode against the media query", async () => {
  const { resolveTheme } = await loadTheme();
  expect(resolveTheme("system", true)).toBe("dark");
  expect(resolveTheme("system", false)).toBe("light");
  expect(resolveTheme("light", true)).toBe("light");
  expect(resolveTheme("dark", false)).toBe("dark");
});

test("falls back to system for missing or unknown stored values", async () => {
  const { parseThemeMode } = await loadTheme();
  expect(parseThemeMode(null)).toBe("system");
  expect(parseThemeMode("")).toBe("system");
  expect(parseThemeMode("sepia")).toBe("system");
  expect(parseThemeMode("dark")).toBe("dark");
});

test("applies the stored explicit choice over the system preference", async () => {
  stubMatchMedia(true);
  window.localStorage.setItem("visrecall.theme", "light");
  const { startTheme } = await loadTheme();

  startTheme();

  expect(document.documentElement.dataset.theme).toBe("light");
});

test("follows the system preference when nothing is stored", async () => {
  const media = stubMatchMedia(true);
  const { startTheme } = await loadTheme();

  startTheme();
  expect(document.documentElement.dataset.theme).toBe("dark");

  media.emit(false);
  expect(document.documentElement.dataset.theme).toBe("light");
});

test("stops following the system once the user picks a side", async () => {
  const media = stubMatchMedia(false);
  const { startTheme, setThemeMode } = await loadTheme();
  startTheme();

  setThemeMode("dark");
  expect(document.documentElement.dataset.theme).toBe("dark");
  expect(window.localStorage.getItem("visrecall.theme")).toBe("dark");

  media.emit(false);
  expect(document.documentElement.dataset.theme).toBe("dark");
});

test("toggling from system persists an explicit choice; system clears it", async () => {
  stubMatchMedia(false);
  const { startTheme, toggleTheme, setThemeMode, getThemeMode } = await loadTheme();
  startTheme();
  expect(getThemeMode()).toBe("system");

  toggleTheme();
  expect(getThemeMode()).toBe("dark");
  expect(window.localStorage.getItem("visrecall.theme")).toBe("dark");

  setThemeMode("system");
  expect(window.localStorage.getItem("visrecall.theme")).toBeNull();
  expect(document.documentElement.dataset.theme).toBe("light");
});

test("notifies subscribers on every change", async () => {
  stubMatchMedia(false);
  const { setThemeMode, subscribeTheme } = await loadTheme();
  const listener = vi.fn();
  const unsubscribe = subscribeTheme(listener);

  setThemeMode("dark");
  expect(listener).toHaveBeenCalledTimes(1);

  unsubscribe();
  setThemeMode("light");
  expect(listener).toHaveBeenCalledTimes(1);
});
