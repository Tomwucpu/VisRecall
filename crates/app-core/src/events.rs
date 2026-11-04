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

/// 阶段 01 的事件集合；后续任务只增字段/变体，不改已有名字。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppEvent {
    ItemCreated(ItemEvent),
    ItemUpdated(ItemUpdatedEvent),
    CaptureFailed(CaptureFailedEvent),
    AppRecovered(AppRecoveredEvent),
}

impl AppEvent {
    /// 前端订阅的事件名（snake_case，与 design §11.2 一致）。
    pub fn name(&self) -> &'static str {
        match self {
            AppEvent::ItemCreated(_) => "item_created",
            AppEvent::ItemUpdated(_) => "item_updated",
            AppEvent::CaptureFailed(_) => "capture_failed",
            AppEvent::AppRecovered(_) => "app_recovered",
        }
    }

    /// 事件的 JSON 载荷，交给 shell 层序列化发送。
    pub fn payload(&self) -> serde_json::Value {
        match self {
            AppEvent::ItemCreated(payload) => json(payload),
            AppEvent::ItemUpdated(payload) => json(payload),
            AppEvent::CaptureFailed(payload) => json(payload),
            AppEvent::AppRecovered(payload) => json(payload),
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
    }

    #[test]
    fn recording_sink_keeps_events() {
        let sink = RecordingSink::default();
        sink.emit(AppEvent::AppRecovered(AppRecoveredEvent { count: 1 }));
        assert_eq!(sink.events().len(), 1);
    }
}
