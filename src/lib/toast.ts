/*
 * 全局 Toast（T7 全局件，T4 起被 stub 提示复用）。
 *
 * 采用模块级 store + `useSyncExternalStore`：任何组件（含非 React 的 catch 分支）
 * 都能 `pushToast()`，不需要 Context 层层传递。与原型一致，提示居中贴底、自动消失。
 */
export type ToastTone = "info" | "error";

export type ToastItem = {
  id: number;
  message: string;
  tone: ToastTone;
};

const LIFETIME_MS: Record<ToastTone, number> = {
  info: 2600,
  // 错误要留够阅读时间：文案里带「阶段 02 提供」这类信息，一闪而过等于没说。
  error: 4200,
};

let sequence = 0;
let items: ToastItem[] = [];
const listeners = new Set<() => void>();
const timers = new Map<number, ReturnType<typeof setTimeout>>();

function notify(): void {
  for (const listener of listeners) {
    listener();
  }
}

function remove(id: number): void {
  const timer = timers.get(id);
  if (timer !== undefined) {
    clearTimeout(timer);
    timers.delete(id);
  }
  const next = items.filter((item) => item.id !== id);
  if (next.length !== items.length) {
    items = next;
    notify();
  }
}

/** 推一条提示；同样的文案正在显示时不叠加，只重置计时。 */
export function pushToast(message: string, tone: ToastTone = "info"): number {
  const existing = items.find((item) => item.message === message && item.tone === tone);
  if (existing !== undefined) {
    const timer = timers.get(existing.id);
    if (timer !== undefined) {
      clearTimeout(timer);
    }
    timers.set(
      existing.id,
      setTimeout(() => remove(existing.id), LIFETIME_MS[tone]),
    );
    return existing.id;
  }

  sequence += 1;
  const id = sequence;
  items = [...items, { id, message, tone }];
  timers.set(
    id,
    setTimeout(() => remove(id), LIFETIME_MS[tone]),
  );
  notify();
  return id;
}

export function dismissToast(id: number): void {
  remove(id);
}

/** 测试用：清空所有提示与计时器。 */
export function resetToasts(): void {
  for (const timer of timers.values()) {
    clearTimeout(timer);
  }
  timers.clear();
  items = [];
  notify();
}

/** `useSyncExternalStore` 的订阅入口，供视图层使用。 */
export function subscribeToasts(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

/** 快照必须是稳定引用：每次 `notify` 才换数组，未变化时返回同一引用。 */
export function getToastsSnapshot(): ToastItem[] {
  return items;
}
