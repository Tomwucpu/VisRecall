import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { QueryClientProvider } from "@tanstack/react-query";
import App from "./app/App";
import { ErrorBoundary } from "./app/ErrorBoundary";
import { queryClient } from "./lib/queryClient";
import { startTheme } from "./lib/theme";
import "./styles/globals.css";

// 首帧前写入 `data-theme`：CSS 自身已按系统偏好兜底，这里把解析结果固化，
// 保证「手动选择」在下次启动时优先于系统（D-UI-2）。
startTheme();

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <QueryClientProvider client={queryClient}>
      <ErrorBoundary>
        <App />
      </ErrorBoundary>
    </QueryClientProvider>
  </StrictMode>,
);
