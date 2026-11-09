//! 事件总线：`app-core` 只依赖 [`EventSink`] 抽象，由 `src-tauri` 用 `AppHandle`
//! 实现真正的发送；测试用 [`RecordingSink`] 断言。
//!
//! 事件名与载荷见 design §11.2；事件只携带 id / 小载荷，不放大图或大文本。

use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// `item_created` 载荷。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemEvent {
    pub id: String,
}

/// `item_updated` 载荷；`fields` 为本次变更字段名。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemUpdatedEvent {
    pub id: String,
    pub fields: Vec<String>,
}

/// `capture_failed` 载荷；`code` 与 IPC 错误码一致。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureFailedEvent {
    pub code: String,
    pub message: String,
}

/// `app_recovered` 载荷：启动时恢复的遗留任务数。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppRecoveredEvent {
    pub count: u64,
}

/// `hotkey_conflict` 载荷：注册失败原因（组合键被占用/配置非法）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HotkeyConflictEvent {
    pub message: String,
}

/// `items_pruned` 载荷：本次清理删除的条数。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemsPrunedEvent {
    pub count: u64,
}

/// `pipeline_status` 载荷：索引队列水位（0/0 时前端隐藏提示）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PipelineStatusEvent {
    pub queued: u64,
    pub running: u64,
}

/// `semantic_status` 载荷：语义检索通道是否可用（false = 降级为 FTS 模式）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SemanticStatusEvent {
    pub ready: bool,
}

/// `storage_migration_progress` 载荷；`phase ∈ { prepare, copy, verify, done }`。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrationProgressEvent {
    pub phase: String,
    pub done: u64,
    pub total: u64,
}

impl MigrationProgressEvent {
    /// 构造进度事件（`phase` 为小写字符串）。
    pub fn new(phase: &str, done: u64, total: u64) -> MigrationProgressEvent {
        MigrationProgressEvent {
            phase: phase.to_string(),
            done,
            total,
        }
    }
}

/// 阶段 01 + 阶段 02 的事件集合；后续任务只增字段/变体，不改已有名字。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppEvent {
    ItemCreated(ItemEvent),
    ItemUpdated(ItemUpdatedEvent),
    CaptureFailed(CaptureFailedEvent),
    HotkeyConflict(HotkeyConflictEvent),
    AppRecovered(AppRecoveredEvent),
    /// 备注保存成功；前端只失效该条详情，不动列表。
    ItemNoteUpdated(ItemEvent),
    /// 某条素材的标签关联变化；失效详情 + 标签云。
    ItemTagsUpdated(ItemEvent),
    /// 标签集合变化（新增/改名/删除）；失效标签云。
    TagsChanged,
    /// 单条彻底删除。
    ItemDeleted(ItemEvent),
    /// 全库清空。
    ItemsReset,
    /// 保留策略清理完成。
    ItemsPruned(ItemsPrunedEvent),
    /// 库迁移进度。
    StorageMigrationProgress(MigrationProgressEvent),
    /// 热键改键成功；`src-tauri` 据此把托盘 tooltip 复原。
    HotkeyRestored,
    /// 阶段一完成：素材已三路可检索（前端失效列表 + 搜索结果）。
    ItemIndexed(ItemEvent),
    /// 阶段二完成：摘要/标签/属性已增强（前端还要失效详情）。
    ItemEnriched(ItemEvent),
    /// 索引队列水位。
    PipelineStatus(PipelineStatusEvent),
    /// 语义检索通道就绪状态。
    SemanticStatus(SemanticStatusEvent),
}

impl AppEvent {
    /// 前端订阅的事件名（snake_case，与 design §11.2 一致）。
    pub fn name(&self) -> &'static str {
        match self {
            AppEvent::ItemCreated(_) => "item_created",
            AppEvent::ItemUpdated(_) => "item_updated",
            AppEvent::CaptureFailed(_) => "capture_failed",
            AppEvent::HotkeyConflict(_) => "hotkey_conflict",
            AppEvent::AppRecovered(_) => "app_recovered",
            AppEvent::ItemNoteUpdated(_) => "item_note_updated",
            AppEvent::ItemTagsUpdated(_) => "item_tags_updated",
            AppEvent::TagsChanged => "tags_changed",
            AppEvent::ItemDeleted(_) => "item_deleted",
            AppEvent::ItemsReset => "items_reset",
            AppEvent::ItemsPruned(_) => "items_pruned",
            AppEvent::StorageMigrationProgress(_) => "storage_migration_progress",
            AppEvent::HotkeyRestored => "hotkey_restored",
            AppEvent::ItemIndexed(_) => "item_indexed",
            AppEvent::ItemEnriched(_) => "item_enriched",
            AppEvent::PipelineStatus(_) => "pipeline_status",
            AppEvent::SemanticStatus(_) => "semantic_status",
        }
    }

    /// 事件的 JSON 载荷，交给 shell 层序列化发送。
    pub fn payload(&self) -> serde_json::Value {
        match self {
            AppEvent::ItemCreated(payload) => json(payload),
            AppEvent::ItemUpdated(payload) => json(payload),
            AppEvent::CaptureFailed(payload) => json(payload),
            AppEvent::HotkeyConflict(payload) => json(payload),
            AppEvent::AppRecovered(payload) => json(payload),
            AppEvent::ItemNoteUpdated(payload) => json(payload),
            AppEvent::ItemTagsUpdated(payload) => json(payload),
            AppEvent::TagsChanged => json(&serde_json::json!({})),
            AppEvent::ItemDeleted(payload) => json(payload),
            AppEvent::ItemsReset => json(&serde_json::json!({})),
            AppEvent::ItemsPruned(payload) => json(payload),
            AppEvent::StorageMigrationProgress(payload) => json(payload),
            AppEvent::HotkeyRestored => json(&serde_json::json!({})),
            AppEvent::ItemIndexed(payload) => json(payload),
            AppEvent::ItemEnriched(payload) => json(payload),
            AppEvent::PipelineStatus(payload) => json(payload),
            AppEvent::SemanticStatus(payload) => json(payload),
        }
    }
}

fn json<T: Serialize>(payload: &T) -> serde_json::Value {
    // 事件载荷都是本模块内的小结构，序列化失败只可能是编程错误；退化为 null 不 panic。
    serde_json::to_value(payload).unwrap_or(serde_json::Value::Null)
}

/// 事件发送端；实现必须是线程安全的（工作线程与 UI 线程都会发送）。
pub trait EventSink: Send + Sync {
    fn emit(&self, event: AppEvent);
}

/// 丢弃所有事件；用于不关心事件的一次性场景。
#[derive(Debug, Default)]
pub struct NullEventSink;

impl EventSink for NullEventSink {
    fn emit(&self, _event: AppEvent) {}
}

/// 把事件记录到内存，供测试断言。
#[derive(Debug, Default)]
pub struct RecordingSink {
    events: Mutex<Vec<AppEvent>>,
}

impl RecordingSink {
    pub fn events(&self) -> Vec<AppEvent> {
        self.events
            .lock()
            .map(|events| events.clone())
            .unwrap_or_default()
    }
}

impl EventSink for RecordingSink {
    fn emit(&self, event: AppEvent) {
        if let Ok(mut events) = self.events.lock() {
            events.push(event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_names_and_payloads_are_stable() {
        let event = AppEvent::AppRecovered(AppRecoveredEvent { count: 3 });
        assert_eq!(event.name(), "app_recovered");
        assert_eq!(event.payload()["count"], 3);

        let event = AppEvent::ItemCreated(ItemEvent { id: "abc".into() });
        assert_eq!(event.name(), "item_created");
        assert_eq!(event.payload()["id"], "abc");

        let event = AppEvent::HotkeyConflict(HotkeyConflictEvent {
            message: "hotkey occupied: Ctrl+Shift+S".into(),
        });
        assert_eq!(event.name(), "hotkey_conflict");
        assert_eq!(event.payload()["message"], "hotkey occupied: Ctrl+Shift+S");
    }

    #[test]
    fn phase_two_event_names_and_payloads_are_stable() {
        let id_event = ItemEvent { id: "abc".into() };

        let event = AppEvent::ItemNoteUpdated(id_event.clone());
        assert_eq!(event.name(), "item_note_updated");
        assert_eq!(event.payload()["id"], "abc");

        let event = AppEvent::ItemTagsUpdated(id_event.clone());
        assert_eq!(event.name(), "item_tags_updated");
        assert_eq!(event.payload()["id"], "abc");

        let event = AppEvent::TagsChanged;
        assert_eq!(event.name(), "tags_changed");
        assert_eq!(event.payload(), serde_json::json!({}));

        let event = AppEvent::ItemDeleted(id_event);
        assert_eq!(event.name(), "item_deleted");

        let event = AppEvent::ItemsReset;
        assert_eq!(event.name(), "items_reset");
        assert_eq!(event.payload(), serde_json::json!({}));

        let event = AppEvent::ItemsPruned(ItemsPrunedEvent { count: 3 });
        assert_eq!(event.name(), "items_pruned");
        assert_eq!(event.payload()["count"], 3);

        let event = AppEvent::StorageMigrationProgress(MigrationProgressEvent::new("copy", 5, 9));
        assert_eq!(event.name(), "storage_migration_progress");
        assert_eq!(event.payload()["phase"], "copy");
        assert_eq!(event.payload()["done"], 5);
        assert_eq!(event.payload()["total"], 9);

        let event = AppEvent::HotkeyRestored;
        assert_eq!(event.name(), "hotkey_restored");
        assert_eq!(event.payload(), serde_json::json!({}));
    }

    #[test]
    fn phase_three_event_names_and_payloads_are_stable() {
        let id_event = ItemEvent { id: "abc".into() };

        let event = AppEvent::ItemIndexed(id_event.clone());
        assert_eq!(event.name(), "item_indexed");
        assert_eq!(event.payload()["id"], "abc");

        let event = AppEvent::ItemEnriched(id_event);
        assert_eq!(event.name(), "item_enriched");
        assert_eq!(event.payload()["id"], "abc");

        let event = AppEvent::PipelineStatus(PipelineStatusEvent {
            queued: 3,
            running: 1,
        });
        assert_eq!(event.name(), "pipeline_status");
        assert_eq!(event.payload()["queued"], 3);
        assert_eq!(event.payload()["running"], 1);

        let event = AppEvent::SemanticStatus(SemanticStatusEvent { ready: false });
        assert_eq!(event.name(), "semantic_status");
        assert_eq!(event.payload()["ready"], false);
    }

    #[test]
    fn recording_sink_keeps_events() {
        let sink = RecordingSink::default();
        sink.emit(AppEvent::AppRecovered(AppRecoveredEvent { count: 1 }));
        assert_eq!(sink.events().len(), 1);
    }
}
