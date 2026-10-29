//! In-process event bus. Producers (jobs, scanner, sessions) publish; the WebSocket
//! endpoint fans events out to clients, which use them to refresh their caches.

use serde::Serialize;
use tokio::sync::broadcast;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerEvent {
    /// A background job changed state or reported progress.
    JobUpdated {
        id: i64,
        kind: String,
        pool: String,
        state: String,
        progress: Option<f64>,
    },
    /// Items of a library were added, changed or removed.
    LibraryChanged { library_id: i64 },
}

/// Cheap to clone; slow subscribers miss old events (`Lagged`) instead of blocking producers.
#[derive(Debug, Clone)]
pub struct EventBus {
    sender: broadcast::Sender<ServerEvent>,
}

impl EventBus {
    pub fn new() -> Self {
        let (sender, _) = broadcast::channel(1024);
        Self { sender }
    }

    pub fn publish(&self, event: ServerEvent) {
        // No subscribers is fine.
        let _ = self.sender.send(event);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ServerEvent> {
        self.sender.subscribe()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}
