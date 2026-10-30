//! Operations and integrations: notifications (webhooks, ntfy, Discord, e-mail),
//! Trakt, backups, history import from Plex/Jellyfin, the server dashboard with
//! bandwidth and logs, and single sign-on via OpenID Connect.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use flixnet_core::events::ServerEvent;
use flixnet_core::scheduler::TaskDef;
use flixnet_media::transcode::TranscodeError;
use utoipa_axum::router::OpenApiRouter;

use crate::AppState;

pub mod backup;
pub mod bandwidth;
pub mod dashboard;
pub mod import;
mod jwt;
pub mod logs;
pub mod matching;
pub mod notify;
pub mod oidc;
pub mod time;
pub mod trakt;

/// Shared state of the operations features (part of [`AppState`]).
#[derive(Clone)]
pub struct Ops {
    /// HTTP client for webhooks, Trakt, Plex/Jellyfin and the OIDC provider.
    pub http: reqwest::Client,
    pub bandwidth: bandwidth::BandwidthMeter,
    pub system: Arc<dashboard::SystemProbe>,
    pub oidc: oidc::OidcState,
    pub trakt: trakt::TraktState,
    /// Transcode sessions whose failure was already reported (session → time).
    transcode_failures: Arc<Mutex<HashMap<String, i64>>>,
}

impl Default for Ops {
    fn default() -> Self {
        Self {
            http: flixnet_library::http::client(),
            bandwidth: Default::default(),
            system: Default::default(),
            oidc: Default::default(),
            trakt: Default::default(),
            transcode_failures: Default::default(),
        }
    }
}

pub(crate) fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .merge(notify::router())
        .merge(trakt::router())
        .merge(backup::router())
        .merge(import::router())
        .merge(dashboard::router())
        .merge(logs::router())
        .merge(oidc::router())
}

/// Starts the event listeners (notifications, Trakt scrobbling); without a Tokio
/// runtime nothing is started.
pub(crate) fn spawn_listeners(state: &AppState) {
    notify::spawn_listener(state);
    trakt::spawn_scrobbler(state);
}

/// Recurring tasks of this module (backup, Trakt sync).
pub fn tasks() -> Vec<TaskDef> {
    backup::tasks().into_iter().chain(trakt::tasks()).collect()
}

/// Publishes a transcode failure once per session (players retry segments).
pub(crate) fn report_transcode_failure(state: &AppState, session_id: &str, err: &TranscodeError) {
    if !matches!(err, TranscodeError::Failed(_) | TranscodeError::Spawn(_)) {
        return;
    }
    let now = flixnet_db::now_ms();
    {
        let mut seen = state.ops.transcode_failures.lock().expect("failures lock");
        seen.retain(|_, at| now - *at < 6 * 60 * 60 * 1000);
        if seen.insert(session_id.to_owned(), now).is_some() {
            return;
        }
    }
    let (user_id, item_id) = state
        .playback
        .list(now)
        .into_iter()
        .find(|s| s.transcode_session_id.as_deref() == Some(session_id))
        .map_or((state.transcoder.owner(session_id), None), |s| {
            (Some(s.user_id), Some(s.item_id))
        });
    state.events.publish(ServerEvent::TranscodeFailed {
        session_id: session_id.to_owned(),
        user_id,
        item_id,
        error: err.to_string(),
    });
}
