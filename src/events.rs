//! Event broadcast hub for real-time WebSocket updates.

use serde::Serialize;
use std::sync::Arc;
use tokio::sync::broadcast;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HiveEvent {
    /// New message added to a conversation
    Message {
        workspace: String,
        bot: String,
        role: String,
        content: String,
    },
    /// Bot status changed (thinking, streaming, idle)
    BotStatus {
        workspace: String,
        bot: String,
        status: String,
        tool_name: Option<String>,
    },
    /// Worker state changed (reserved for future use)
    #[allow(dead_code)]
    WorkerUpdate {
        workspace: String,
        worker_id: String,
        status: String,
    },
}

#[derive(Clone)]
pub struct EventHub {
    tx: Arc<broadcast::Sender<HiveEvent>>,
}

impl Default for EventHub {
    fn default() -> Self {
        Self::new()
    }
}

impl EventHub {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(256);
        Self { tx: Arc::new(tx) }
    }

    pub fn send(&self, event: HiveEvent) {
        let _ = self.tx.send(event);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<HiveEvent> {
        self.tx.subscribe()
    }
}
