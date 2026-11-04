import { useCallback, useEffect, useState } from "react";
import { commands, type AppStatus } from "../lib/bindings";

type LoadState =
  | { status: "loading" }
  | { status: "ready"; app: AppStatus }
  | { status: "error"; message: string };

/** 命令错误统一为 `{ code, message }`；未知错误兜底成字符串。 */
function describeError(error: unknown): string {
  if (typeof error === "object" && error !== null) {
    const { code, message } = error as { code?: unknown; message?: unknown };
    if (typeof message === "string") {
      return typeof code === "string" ? `${code}: ${message}` : message;
    }
  }
  return error instanceof Error ? error.message : String(error);
}

export default function App() {
  const [state, setState] = useState<LoadState>({ status: "loading" });
  const [opening, setOpening] = useState(false);

  const load = useCallback(async () => {
    try {
      const app = await commands.getAppStatus();
      setState({ status: "ready", app });
    } catch (error) {
      setState({ status: "error", message: describeError(error) });
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const openLibraryFolder = async () => {
    setOpening(true);
    try {
      await commands.openLibraryFolder();
    } catch (error) {
      setState({ status: "error", message: describeError(error) });
    } finally {
      setOpening(false);
    }
  };

  return (
    <main className="app-shell">
      <header className="app-header">
        <div>
          <h1>目记 VisRecall</h1>
          <p className="app-subtitle">捕获闭环 · 阶段 01</p>
        </div>
        <button type="button" onClick={() => void load()}>
          刷新状态
        </button>
      </header>

      {state.status === "loading" && <p className="app-hint">正在加载应用状态…</p>}

      {state.status === "error" && (
        <section className="app-card app-card-error" role="alert">
          <h2>无法读取应用状态</h2>
          <p>{state.message}</p>
          <button type="button" onClick={() => void load()}>
            重试
          </button>
        </section>
      )}

      {state.status === "ready" && (
        <section className="app-card">
          <h2>应用状态</h2>
          <dl className="app-meta">
            <dt>版本</dt>
            <dd>{state.app.version}</dd>
            <dt>库路径</dt>
            <dd title={state.app.libraryPath}>{state.app.libraryPath}</dd>
            <dt>库状态</dt>
            <dd className={state.app.libraryOk ? "app-ok" : "app-bad"}>
              {state.app.libraryOk ? "正常" : "无法打开"}
            </dd>
          </dl>
          <button type="button" disabled={opening} onClick={() => void openLibraryFolder()}>
            {opening ? "正在打开…" : "打开库目录"}
          </button>
        </section>
      )}

      <footer className="app-footer">关闭窗口后应用仍在托盘运行。</footer>
    </main>
  );
}
