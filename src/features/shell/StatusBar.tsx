import type { PipelineState } from "./usePipelineState";

type Props = {
  /** 与标题栏同一份库状态文案。 */
  status: string;
  /** 后端版本号；读取中为 `null`。 */
  version: string | null;
  /** 索引队列水位 + 语义通道状态（阶段 03）。 */
  pipeline?: PipelineState;
};

/** 索引中提示：`0/0` 时整块隐藏，不占地方也不闪。 */
function pipelineText(pipeline: PipelineState): string | null {
  if (pipeline.running > 0) {
    return `索引中 ${pipeline.running}`;
  }
  if (pipeline.queued > 0) {
    return `排队 ${pipeline.queued}`;
  }
  return null;
}

/**
 * 状态栏：左侧库状态，右侧只列**已实现**的快捷键与真实的后台状态。
 *
 * - `排队 N / 索引中 N` 来自 `pipeline_status`（0 时不显示）；
 * - `语义未就绪` 来自 `semantic_status`，此时检索降级为字面 + 条件（P3-R09）。
 */
export function StatusBar({ status, version, pipeline }: Props) {
  const indexing = pipeline === undefined ? null : pipelineText(pipeline);
  const degraded = pipeline !== undefined && !pipeline.semanticReady;

  return (
    <footer className="statusbar">
      <div className="status-left">
        <span className="live" />
        <span>{status}</span>
        {indexing !== null && (
          <span className="status-indexing" role="status">
            {indexing}
          </span>
        )}
      </div>
      <div className="status-right">
        {degraded && (
          <span className="status-degraded" role="status" title="语义检索未就绪：结果为字面 + 条件匹配">
            语义未就绪
          </span>
        )}
        {version !== null && <span>v{version}</span>}
        <span>
          <kbd>Ctrl K</kbd> 搜索
        </span>
        <span>
          <kbd>B</kbd> 侧栏
        </span>
        <span>
          <kbd>T</kbd> 主题
        </span>
      </div>
    </footer>
  );
}
