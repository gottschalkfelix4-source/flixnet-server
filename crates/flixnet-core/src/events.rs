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
    /// A scan imported new movies or episodes (`items` holds up to 20 of them).
    ItemsAdded {
        library_id: i64,
        library_name: String,
        count: usize,
        items: Vec<AddedItem>,
    },
    /// A library scan finished.
    ScanFinished {
        library_id: i64,
        library_name: String,
        added: usize,
        changed: usize,
        missing: usize,
    },
    /// A library scan failed.
    ScanFailed {
        library_id: i64,
        library_name: String,
        error: String,
    },
    /// Someone started playing a movie or episode.
    PlaybackStarted(PlaybackEvent),
    /// A running playback was paused.
    PlaybackPaused(PlaybackEvent),
    /// A paused playback continues.
    PlaybackResumed(PlaybackEvent),
    /// A playback ended (stopped, finished or timed out).
    PlaybackStopped(PlaybackEvent),
    /// ffmpeg failed for a transcode session.
    TranscodeFailed {
        session_id: String,
        user_id: Option<i64>,
        item_id: Option<i64>,
        error: String,
    },
    /// A DVR recording finished.
    RecordingFinished {
        recording_id: i64,
        title: String,
        channel: Option<String>,
        item_id: Option<i64>,
    },
    /// A user account was created.
    UserCreated { user_id: i64, name: String },
    /// A user signed in from a device that never signed in before.
    NewDeviceLogin {
        user_id: i64,
        user_name: String,
        device_name: String,
        client: String,
    },
    /// A newer server version is available.
    UpdateAvailable { current: String, latest: String },
}

/// An item mentioned by [`ServerEvent::ItemsAdded`].
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AddedItem {
    pub item_id: i64,
    pub kind: String,
    pub title: String,
}

/// What playback events carry.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PlaybackEvent {
    pub session_id: String,
    pub user_id: i64,
    pub user_name: String,
    pub item_id: i64,
    pub device_name: Option<String>,
    pub client: Option<String>,
    /// `direct_play`, `remux` or `transcode`.
    pub play_method: String,
    pub position_ms: i64,
    pub duration_ms: Option<i64>,
    /// Stop events: the item now counts as watched.
    pub completed: bool,
}

impl PlaybackEvent {
    /// Progress in percent (0–100), 0 when the duration is unknown.
    pub fn progress_percent(&self) -> f64 {
        match self.duration_ms.filter(|d| *d > 0) {
            Some(d) => (self.position_ms.max(0) as f64 * 100.0 / d as f64).clamp(0.0, 100.0),
            None => 0.0,
        }
    }
}

impl ServerEvent {
    /// Stable name used by notification targets (`playback.started`, …); `None` for
    /// purely internal events.
    pub fn notification_kind(&self) -> Option<&'static str> {
        Some(match self {
            ServerEvent::JobUpdated { .. } | ServerEvent::LibraryChanged { .. } => return None,
            ServerEvent::ItemsAdded { .. } => "library.items_added",
            ServerEvent::ScanFinished { .. } => "library.scan_finished",
            ServerEvent::ScanFailed { .. } => "library.scan_failed",
            ServerEvent::PlaybackStarted(_) => "playback.started",
            ServerEvent::PlaybackPaused(_) => "playback.paused",
            ServerEvent::PlaybackResumed(_) => "playback.resumed",
            ServerEvent::PlaybackStopped(_) => "playback.stopped",
            ServerEvent::TranscodeFailed { .. } => "transcode.failed",
            ServerEvent::RecordingFinished { .. } => "dvr.recording_finished",
            ServerEvent::UserCreated { .. } => "user.created",
            ServerEvent::NewDeviceLogin { .. } => "user.new_device",
            ServerEvent::UpdateAvailable { .. } => "server.update_available",
        })
    }
}

/// Every event kind notification targets can subscribe to, with a German label.
pub const NOTIFICATION_KINDS: &[(&str, &str)] = &[
    ("library.items_added", "Neue Einträge in der Bibliothek"),
    ("library.scan_finished", "Scan abgeschlossen"),
    ("library.scan_failed", "Scan fehlgeschlagen"),
    ("playback.started", "Wiedergabe gestartet"),
    ("playback.paused", "Wiedergabe pausiert"),
    ("playback.resumed", "Wiedergabe fortgesetzt"),
    ("playback.stopped", "Wiedergabe beendet"),
    ("transcode.failed", "Transcoding fehlgeschlagen"),
    ("dvr.recording_finished", "Aufnahme fertig"),
    ("user.created", "Neues Nutzerkonto"),
    ("user.new_device", "Anmeldung von neuem Gerät"),
    ("server.update_available", "Server-Update verfügbar"),
];

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
