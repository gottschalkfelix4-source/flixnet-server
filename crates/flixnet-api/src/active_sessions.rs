//! Active playback sessions, kept in memory: who watches what, on which device, how
//! (Direct Play/Remux/Transcode) and where they are.
//!
//! Players report every ~10 s. A session that stops reporting expires and is
//! finished like a regular stop (resume point / watched, history entry), so a crashed
//! client or a closed laptop still leaves the right state behind.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use flixnet_core::events::{EventBus, PlaybackEvent, ServerEvent};
use flixnet_db::Db;
use flixnet_media::decision::PlayMethod;

/// A session without a report for this long is over.
pub const SESSION_TIMEOUT_MS: i64 = 90_000;

#[derive(Debug, Clone)]
pub struct ActiveSession {
    pub id: String,
    pub user_id: i64,
    pub user_name: String,
    pub device_id: i64,
    pub device_name: Option<String>,
    pub client: Option<String>,
    pub item_id: i64,
    pub position_ms: i64,
    pub duration_ms: Option<i64>,
    pub play_method: PlayMethod,
    pub transcode_session_id: Option<String>,
    pub is_paused: bool,
    pub started_at: i64,
    pub last_report_at: i64,
    /// Highest quartile (25/50/75 % = 1/2/3) already written to the history.
    pub milestone: u8,
}

/// Registry of active sessions. Cheap to clone.
#[derive(Clone, Default)]
pub struct ActiveSessions {
    inner: Arc<Mutex<HashMap<String, ActiveSession>>>,
}

impl ActiveSessions {
    pub fn insert(&self, session: ActiveSession) {
        self.inner
            .lock()
            .unwrap()
            .insert(session.id.clone(), session);
    }

    pub fn get(&self, id: &str) -> Option<ActiveSession> {
        self.inner.lock().unwrap().get(id).cloned()
    }

    /// Applies a change and returns the updated session.
    pub fn update(
        &self,
        id: &str,
        change: impl FnOnce(&mut ActiveSession),
    ) -> Option<ActiveSession> {
        let mut sessions = self.inner.lock().unwrap();
        let session = sessions.get_mut(id)?;
        change(session);
        Some(session.clone())
    }

    pub fn remove(&self, id: &str) -> Option<ActiveSession> {
        self.inner.lock().unwrap().remove(id)
    }

    /// Sessions that reported within the timeout, oldest first.
    pub fn list(&self, now_ms: i64) -> Vec<ActiveSession> {
        let mut out: Vec<_> = self
            .inner
            .lock()
            .unwrap()
            .values()
            .filter(|s| now_ms - s.last_report_at <= SESSION_TIMEOUT_MS)
            .cloned()
            .collect();
        out.sort_by(|a, b| (a.started_at, &a.id).cmp(&(b.started_at, &b.id)));
        out
    }

    /// Removes and returns the sessions whose last report is older than the timeout.
    pub fn take_expired(&self, now_ms: i64) -> Vec<ActiveSession> {
        let mut sessions = self.inner.lock().unwrap();
        let expired: Vec<String> = sessions
            .values()
            .filter(|s| now_ms - s.last_report_at > SESSION_TIMEOUT_MS)
            .map(|s| s.id.clone())
            .collect();
        expired
            .iter()
            .filter_map(|id| sessions.remove(id))
            .collect()
    }

    /// Finishes expired sessions in the background (needs a Tokio runtime; without
    /// one, e.g. when only the OpenAPI document is generated, nothing is started).
    pub fn spawn_reaper(&self, db: Db, events: EventBus) {
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(reaper(Arc::downgrade(&self.inner), db, events));
        }
    }
}

impl ActiveSession {
    /// What playback events carry about this session.
    pub fn event(&self, completed: bool) -> PlaybackEvent {
        PlaybackEvent {
            session_id: self.id.clone(),
            user_id: self.user_id,
            user_name: self.user_name.clone(),
            item_id: self.item_id,
            device_name: self.device_name.clone(),
            client: self.client.clone(),
            play_method: match self.play_method {
                PlayMethod::DirectPlay => "direct_play",
                PlayMethod::Remux => "remux",
                PlayMethod::Transcode => "transcode",
            }
            .to_owned(),
            position_ms: self.position_ms,
            duration_ms: self.duration_ms,
            completed,
        }
    }
}

async fn reaper(inner: Weak<Mutex<HashMap<String, ActiveSession>>>, db: Db, events: EventBus) {
    let mut tick = tokio::time::interval(Duration::from_secs(10));
    loop {
        tick.tick().await;
        let Some(inner) = inner.upgrade() else {
            return;
        };
        let expired = ActiveSessions { inner }.take_expired(flixnet_db::now_ms());
        for session in expired {
            tracing::info!(session = %session.id, user = session.user_id, "playback session expired");
            match crate::playstate::finish(&db, &session).await {
                Ok(progress) => events.publish(ServerEvent::PlaybackStopped(
                    session.event(progress == flixnet_db::playstate::Progress::Played),
                )),
                Err(err) => {
                    tracing::warn!(session = %session.id, %err, "finishing an expired playback session failed");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: &str, last_report_at: i64) -> ActiveSession {
        ActiveSession {
            id: id.to_owned(),
            user_id: 1,
            user_name: "Felix".into(),
            device_id: 1,
            device_name: None,
            client: None,
            item_id: 1,
            position_ms: 0,
            duration_ms: None,
            play_method: PlayMethod::DirectPlay,
            transcode_session_id: None,
            is_paused: false,
            started_at: last_report_at,
            last_report_at,
            milestone: 0,
        }
    }

    #[test]
    fn sessions_expire_after_missing_reports() {
        let sessions = ActiveSessions::default();
        sessions.insert(session("old", 1_000));
        sessions.insert(session("fresh", 50_000));
        let now = 1_000 + SESSION_TIMEOUT_MS + 1;
        assert_eq!(
            sessions.list(now).len(),
            1,
            "expired sessions are not listed"
        );
        let expired = sessions.take_expired(now);
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].id, "old");
        assert!(sessions.get("old").is_none());
        assert!(sessions.get("fresh").is_some());
        assert!(sessions.take_expired(now).is_empty());
    }

    #[test]
    fn updates_apply_to_the_stored_session() {
        let sessions = ActiveSessions::default();
        sessions.insert(session("a", 0));
        let updated = sessions.update("a", |s| s.position_ms = 42).unwrap();
        assert_eq!(updated.position_ms, 42);
        assert_eq!(sessions.get("a").unwrap().position_ms, 42);
        assert!(sessions.update("missing", |_| {}).is_none());
    }
}
