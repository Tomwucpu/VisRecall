//! 容量与保留策略的编排（design §6.7）。
//!
//! 两件事放在这里而不是库层：
//! - **容量是软配额**：判定「已用 ≥ 容量」需要用量统计，而用量统计要遍历目录，
//!   属于编排成本；
//! - **保留清理是逐条删除**：每条都要走与单条删除相同的「先删库、后删文件、发事件」
//!   路径，单条失败不能中断其余条目。

use std::sync::Mutex;

use visrecall_domain::storage::{OrphanStats, PruneReport};
use visrecall_domain::{now_millis, AppError};
use visrecall_library::Library;

use crate::events::{AppEvent, EventSink, ItemsPrunedEvent};

/// 一次清理最多处理多少条：宁可多轮，也不要长时间持有库连接。
pub const PRUNE_BATCH: u32 = 500;

/// 保留清理的互斥锁：同一时刻只允许一个长任务（与迁移闸门同语义）。
///
/// 迁移与清理都动「源库」，并发执行会出现「清理删了一半时正在 copy」的竞态。
static PRUNE_LOCK: Mutex<()> = Mutex::new(());

/// 按已保存的保留策略清理过期素材。
///
/// - `days == None`（永久保留）→ 直接返回全 0：**默认值绝不能触发清库**；
/// - 逐条走 `Library::delete_item`（先删库、后删文件），单条失败只计数不中断；
/// - 只有成功删除 > 0 时才发 `items_pruned { count }`。
pub fn prune_expired(library: &Library, sink: &dyn EventSink) -> PruneReport {
    // 串行化：并发的两次清理没有意义（第二次看到的已经是被删过的库），
    // 但也不能悄悄跳过——启动清理与「立即清理」按钮撞车时，后者应当真的跑完。
    let _guard = PRUNE_LOCK
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());

    let Some(days) = library.retention().days else {
        return PruneReport::default();
    };

    let cutoff = now_millis() - i64::from(days) * 86_400_000;
    let expired = match library.list_expired(cutoff, PRUNE_BATCH) {
        Ok(items) => items,
        Err(err) => {
            tracing::warn!(%err, days, "failed to list expired items");
            return PruneReport::default();
        }
    };

    let mut report = PruneReport::default();
    for item in expired {
        // 自动保留策略首版只处理库内自有原件，目录引用由用户显式管理。
        match library.external_asset_for_item(&item.id) {
            Ok(Some(_)) => continue,
            Ok(None) => {}
            Err(err) => {
                report.failed += 1;
                tracing::warn!(id = %item.id, %err, "failed to check item ownership");
                continue;
            }
        }
        match library.delete_item(&item.id) {
            Ok(_) => report.deleted += 1,
            Err(err) => {
                report.failed += 1;
                tracing::warn!(id = %item.id, %err, "failed to prune expired item");
            }
        }
    }

    if report.deleted > 0 {
        sink.emit(AppEvent::ItemsPruned(ItemsPrunedEvent {
            count: report.deleted,
        }));
        tracing::info!(
            deleted = report.deleted,
            failed = report.failed,
            days,
            "retention prune finished"
        );
    }
    report
}

/// 容量门禁：已用 ≥ 容量时返回 `STORAGE_FULL`。
///
/// 抓帧流程里还有一份同样的判定（`capture_flow`），这份给「没有经过抓帧流程」的
/// 入口（例如后续的批量导入）复用。
pub fn check_quota(library: &Library) -> Result<(), AppError> {
    let usage = library.usage()?;
    if usage.used_bytes >= usage.capacity_bytes {
        return Err(AppError::StorageFull);
    }
    Ok(())
}

/// 孤儿统计：`originals/`、`texts/` 下未被引用的文件 + 缺失缩略图。
///
/// 统计口径只有一份（`Library::orphans`）；放在这里是为了让「预估」与「清理后的
/// 对账日志」挨在一起，命令层只做 DTO 映射。
pub fn orphan_stats(library: &Library) -> Result<OrphanStats, AppError> {
    library.orphans()
}

/// 清理孤儿文件，返回实际删除数（含「文件已被手动删除」，计为成功）。
///
/// **不发事件**：孤儿文件不属于任何素材，列表不需要刷新；清理会改变用量，由前端
/// 在命令返回后 `invalidateQueries(usage)` 拉取（P2-R10）。
pub fn cleanup_orphans(library: &Library) -> Result<OrphanStats, AppError> {
    let stats = library.cleanup_orphans()?;
    tracing::info!(
        removed = stats.files,
        bytes = stats.bytes,
        missing_thumbs = stats.missing_thumbs,
        "orphan cleanup finished"
    );
    Ok(stats)
}
