import "@testing-library/jest-dom/vitest";
import { vi } from "vitest";

// jsdom 里没有 Tauri 运行时。按 Windows 上自定义协议的实际形态复刻 `convertFileSrc`，
// 让测试中的图片 URL 转换与真机一致；其余导出（如 `invoke`）保持真实实现。
vi.mock("@tauri-apps/api/core", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@tauri-apps/api/core")>();
  return {
    ...actual,
    convertFileSrc: (path: string, protocol: string = "asset") =>
      `http://${protocol}.localhost/${encodeURIComponent(path)}`,
  };
});
