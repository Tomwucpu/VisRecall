//! `rebuild` handler：对单条素材重算索引（修复通道）。
//!
//! 分工说明（见 `docs/architecture.md` §5.3）：
//! - **蓝绿全量重建**由 [`crate::lifecycle::rebuild_embeddings`] 同步完成（20 条规模下
//!   每批 100 条一次跑完；`jobs` 表没有 payload 列，把「目标表名/新模型」塞进任务反而更绕）。
//! - `rebuild` **任务**只承担「单条素材派生数据坏了 → 重算」：启动自检发现缺 FTS/向量行
//!   时按 `PRIORITY_REBUILD` 入队，执行逻辑与 `index` 完全一致（同一份代码，避免两条路径漂移）。

use visrecall_domain::AppError;

use crate::handlers::index;
use crate::worker::{JobCtx, JobHandler};

/// 单条素材的索引修复。
#[derive(Debug, Default, Clone, Copy)]
pub struct RebuildHandler;

impl JobHandler for RebuildHandler {
    fn stage(&self) -> &'static str {
        crate::STAGE_REBUILD
    }

    fn run(&self, ctx: &JobCtx<'_>) -> Result<(), AppError> {
        index::run_for(ctx)
    }
}
