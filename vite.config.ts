import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  server: {
    port: 5173,
    strictPort: true,
    watch: {
      // 只监听前端源码。target/ 是 cargo 产物（含数百个 rustdoc HTML），
      // crates/ 与 docs/ 也不参与前端构建；它们进 watcher 会拖慢启动并
      // 在 cargo 编译时触发无用的事件。
      ignored: ["**/target/**", "**/crates/**", "**/docs/**"],
    },
  },
  optimizeDeps: {
    // 默认 entries 是 root 下 `**/*.html`：会把 docs/prototype 与
    // target/doc（rustdoc 生成物，数百个文件）一起当成依赖扫描入口。
    // 实测冷缓存下 scan 69.8s + bundle 144.7s，期间首个 HTML 请求被挂起，
    // 窗口表现为长时间白屏。限定为真正的应用入口后降回秒级。
    entries: ["index.html"],
  },
  test: {
    environment: "jsdom",
    setupFiles: ["./src/test/setup.ts"],
    globals: true,
  },
});
