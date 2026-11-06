import { useSyncExternalStore } from "react";
import { getToastsSnapshot, subscribeToasts, type ToastItem } from "../../lib/toast";

function useToasts(): ToastItem[] {
  return useSyncExternalStore(subscribeToasts, getToastsSnapshot, getToastsSnapshot);
}

/** 固定容器：原型 `.toasts`，底部居中、不拦截点击。 */
export function ToastViewport() {
  const toasts = useToasts();

  return (
    <div className="toasts" role="status" aria-live="polite">
      {toasts.map((toast) => (
        <div key={toast.id} className={`toast${toast.tone === "error" ? " error" : ""}`}>
          {toast.tone === "error" ? (
            <svg
              width="11"
              height="11"
              viewBox="0 0 12 12"
              fill="none"
              stroke="currentColor"
              strokeWidth="1.6"
              strokeLinecap="round"
            >
              <path d="M6 2.6v4.2M6 9.4v.2" />
            </svg>
          ) : (
            <svg
              width="11"
              height="11"
              viewBox="0 0 12 12"
              fill="none"
              stroke="currentColor"
              strokeWidth="1.6"
            >
              <path d="M2 6.3l2.6 2.5L10 3.5" strokeLinecap="round" strokeLinejoin="round" />
            </svg>
          )}
          <span>{toast.message}</span>
        </div>
      ))}
    </div>
  );
}
