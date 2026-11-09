import { useEffect, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

/** 索引队列水位 + 语义通道就绪状态（P3-R09 的状态栏与降级标注）。 */
export type PipelineState = {
  queued: number;
  running: number;
  /** 语义检索是否可用；`false` = 检索降级为字面 + 条件。 */
  semanticReady: boolean;
};

const INITIAL: PipelineState = { queued: 0, running: 0, semanticReady: true };

type PipelineStatusPayload = { queued?: unknown; running?: unknown };
type SemanticStatusPayload = { ready?: unknown };

/**
 * 订阅 `pipeline_status` / `semantic_status`。
 *
 * 初始值取「就绪 + 空闲」而不是「未就绪」：首帧还不知道真实状态，
 * 先按可用展示（乐观），等事件到达再纠正——避免刚打开就闪一下降级提示。
 * `listen` 失败（非 Tauri 环境/测试）不影响渲染。
 */
export function usePipelineState(): PipelineState {
  const [state, setState] = useState<PipelineState>(INITIAL);

  useEffect(() => {
    let disposed = false;
    const unlisteners: UnlistenFn[] = [];

    const track = (promise: Promise<UnlistenFn>) => {
      promise
        .then((unlisten) => {
          if (disposed) {
            unlisten();
          } else {
            unlisteners.push(unlisten);
          }
        })
        .catch((error: unknown) => {
          console.error("pipeline state subscription failed", error);
        });
    };

    track(
      listen<PipelineStatusPayload>("pipeline_status", (event) => {
        const queued = Number(event.payload?.queued ?? 0);
        const running = Number(event.payload?.running ?? 0);
        setState((prev) => ({
          ...prev,
          queued: Number.isFinite(queued) ? queued : 0,
          running: Number.isFinite(running) ? running : 0,
        }));
      }),
    );

    track(
      listen<SemanticStatusPayload>("semantic_status", (event) => {
        const ready = event.payload?.ready === true;
        setState((prev) => ({ ...prev, semanticReady: ready }));
      }),
    );

    return () => {
      disposed = true;
      for (const unlisten of unlisteners.splice(0)) {
        unlisten();
      }
    };
  }, []);

  return state;
}
