//! Jellyfin-compatible API, so existing Jellyfin clients (Infuse, Swiftfin, Findroid,
//! Streamyfin, Jellyfin for Android TV, …) can browse, play and report progress
//! against Flixnet before Flixnet's own apps exist.
//!
//! The layer runs on its own listener (`jellyfin.enabled`, `jellyfin.bind`, default
//! `0.0.0.0:8098`, off by default) and is **not** part of Flixnet's OpenAPI document:
//! it mimics Jellyfin's API (`https://api.jellyfin.org`) instead of defining one. It
//! owns no data and no logic of its own beyond the translation: users, tokens, library
//! access, age limits, the decision engine, transcode sessions, the image cache and
//! the playback state are Flixnet's (`flixnet-api`, `flixnet-db`, `flixnet-media`).
//!
//! # Conventions
//!
//! * Paths are matched case-insensitively (they are lower-cased before routing) and
//!   may carry the legacy `/emby` prefix; query parameter names are case-insensitive;
//!   JSON bodies are read in any key casing and written in PascalCase without `null`s.
//! * Ids are GUIDs derived from Flixnet ids (see [`ids`]). Times are ticks (100 ns),
//!   dates ISO-8601 in UTC.
//! * Tokens are Flixnet device tokens, issued by `Users/AuthenticateByName` and read
//!   from `Authorization: MediaBrowser …, Token="…"`, `X-Emby-Authorization`,
//!   `X-Emby-Token`, `X-MediaBrowser-Token` or `api_key`/`ApiKey`.
//!
//! # Without a token
//!
//! Only `System/Info/Public`, `System/Ping`, `Branding/*`, `QuickConnect/Enabled`,
//! `Users/Public` (always empty) and `Users/AuthenticateByName`. Media URLs that
//! players load without headers are protected otherwise: images by their signed
//! `tag` (or a token), direct streams and subtitles by a signed URL (or a token), HLS
//! playlists and segments by the 128-bit transcode session id in the path.
//!
//! # Endpoints
//!
//! * System: `System/Info/Public`, `System/Info`, `System/Ping`, `System/Endpoint`,
//!   `Branding/Configuration`, `Branding/Css`, `QuickConnect/Enabled` (false),
//!   `Plugins` (empty), `DisplayPreferences/{id}` (GET/POST, stored per user).
//! * Users: `Users/AuthenticateByName`, `Users/Public`, `Users/Me`, `Users`,
//!   `Users/{id}`, `Sessions`, `Sessions/Capabilities[/Full]`, `Sessions/Logout`,
//!   `Devices/Options` (accepted, ignored).
//! * Browsing: `UserViews` (+ `Users/{id}/Views`), `Items` (+ `Users/{id}/Items`) with
//!   ParentId, IncludeItemTypes, ExcludeItemTypes, Recursive, SortBy, SortOrder,
//!   StartIndex, Limit, Fields, Filters (IsPlayed, IsUnplayed, IsFavorite,
//!   IsResumable, IsFolder, IsNotFolder), IsPlayed, IsFavorite, SearchTerm,
//!   NameStartsWith, Genres, GenreIds, Years, PersonIds, Ids, ExcludeItemIds;
//!   `Items/{id}` (+ `Users/{id}/Items/{id}`), `Items/{id}/Ancestors`,
//!   `Shows/{id}/Seasons`, `Shows/{id}/Episodes`, `Items/Latest`
//!   (+ `Users/{id}/Items/Latest`), `Genres`, `Persons`, `Items/{id}/Similar`,
//!   `Search/Hints`, `Items/Filters[2]` (genres), and empty `Intros`, `LocalTrailers`,
//!   `SpecialFeatures`, `ThemeMedia`.
//! * Playback: `Items/{id}/PlaybackInfo` (GET/POST with the client's DeviceProfile),
//!   `Videos/{id}/stream[.ext]` and `Items/{id}/Download` (ranges), `Audio/{id}/stream`
//!   and `Audio/{id}/universal` (the original file), `Videos/{id}/master.m3u8` and
//!   `main.m3u8` (Flixnet transcode sessions), subtitles as
//!   `Videos/{id}/{mediaSourceId}/Subtitles/{index}[/{ticks}]/Stream.vtt|srt`,
//!   `DELETE Videos/ActiveEncodings`, `MediaSegments/{id}` (intro, credits, recap).
//! * Playstate: `Sessions/Playing`, `…/Progress`, `…/Stopped`, `…/Ping`, the legacy
//!   `[Users/{id}/]PlayingItems/{id}`, `UserPlayedItems/{id}`, `UserFavoriteItems/{id}`
//!   (+ `Users/{id}/PlayedItems|FavoriteItems/{id}`), `UserItems/{id}/UserData`,
//!   `UserItems/Resume` (+ `Users/{id}/Items/Resume`), `Shows/NextUp`.
//! * Images: `Items/{id}/Images/{type}[/{index}]` with maxWidth, width, fillWidth,
//!   maxHeight, height, fillHeight (resized through Flixnet's image cache).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::Request;
use axum::http::Uri;
use flixnet_api::AppState;
use tower::ServiceExt;
use tower_http::trace::TraceLayer;

mod auth;
mod convert;
mod dto;
pub mod ids;
mod images;
mod items;
mod playback;
mod playstate;
mod system;
mod users;
mod util;

/// Jellyfin version reported to clients (the API generation this layer follows).
pub const JELLYFIN_VERSION: &str = "10.11.0";

/// Shared state of the Jellyfin handlers: Flixnet's state plus the bits of
/// bookkeeping Jellyfin's protocol needs.
#[derive(Clone)]
pub struct JfState {
    pub app: AppState,
    server_id: Arc<str>,
    server_name: Arc<str>,
    plays: PlaySessions,
}

impl JfState {
    pub fn new(app: AppState) -> Self {
        // Stable across restarts (the signing secret is persisted), unique per server.
        let server_id = app.signer.tag("jellyfin-server", 1);
        let server_name = app
            .config
            .jellyfin
            .server_name
            .clone()
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| "Flixnet".to_owned());
        Self {
            app,
            server_id: server_id.into(),
            server_name: server_name.into(),
            plays: PlaySessions::default(),
        }
    }

    pub fn server_id(&self) -> &str {
        &self.server_id
    }
}

/// What `PlaybackInfo` started for a Jellyfin play session: transcode sessions per
/// media file, so progress reports and stops reach them.
#[derive(Debug, Clone)]
struct PlayEntry {
    user_id: i64,
    item_id: i64,
    transcodes: Vec<TranscodeRef>,
    created_at: i64,
}

/// A transcode session started for a play session.
#[derive(Debug, Clone)]
struct TranscodeRef {
    file_id: i64,
    session: String,
    method: flixnet_media::decision::PlayMethod,
    /// For the master playlist.
    bandwidth: i64,
    width: Option<i64>,
    height: Option<i64>,
}

#[derive(Clone, Default)]
struct PlaySessions(Arc<Mutex<HashMap<String, PlayEntry>>>);

/// Play sessions are forgotten after a day.
const PLAY_ENTRY_MAX_AGE_MS: i64 = 24 * 60 * 60 * 1000;

impl PlaySessions {
    fn insert(&self, id: String, entry: PlayEntry) {
        let mut map = self.0.lock().unwrap();
        let now = flixnet_db::now_ms();
        map.retain(|_, e| now - e.created_at < PLAY_ENTRY_MAX_AGE_MS);
        map.insert(id, entry);
    }

    fn get(&self, id: &str) -> Option<PlayEntry> {
        self.0.lock().unwrap().get(id).cloned()
    }

    fn remove(&self, id: &str) -> Option<PlayEntry> {
        self.0.lock().unwrap().remove(id)
    }

    fn find_transcode(&self, session: &str) -> Option<TranscodeRef> {
        self.0
            .lock()
            .unwrap()
            .values()
            .flat_map(|e| e.transcodes.iter())
            .find(|t| t.session == session)
            .cloned()
    }
}

/// The Jellyfin API router (serve it on its own listener).
pub fn router(app: AppState) -> Router {
    let state = JfState::new(app);
    let api = Router::new()
        .merge(system::router())
        .merge(users::router())
        .merge(items::router())
        .merge(playback::router())
        .merge(playstate::router())
        .merge(images::router())
        .layer(TraceLayer::new_for_http())
        .with_state(state);
    // Normalise the path before routing; a layer on `api` would run after it.
    Router::new().fallback_service(api.map_request(normalize))
}

/// Lower-cases the path, strips the legacy `/emby` / `/mediabrowser` prefix and a
/// trailing slash. The query string is kept as it is.
fn normalize(mut request: Request) -> Request {
    let uri = request.uri();
    let mut path = uri.path().to_ascii_lowercase();
    for prefix in ["/emby", "/mediabrowser"] {
        if path == prefix || path.starts_with(&format!("{prefix}/")) {
            path = path[prefix.len()..].to_owned();
        }
    }
    while path.len() > 1 && path.ends_with('/') {
        path.pop();
    }
    if path.is_empty() {
        path.push('/');
    }
    let path_and_query = match uri.query() {
        Some(query) => format!("{path}?{query}"),
        None => path,
    };
    let mut parts = uri.clone().into_parts();
    if let Ok(pq) = path_and_query.parse() {
        parts.path_and_query = Some(pq);
        if let Ok(new_uri) = Uri::from_parts(parts) {
            *request.uri_mut() = new_uri;
        }
    }
    request
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(uri: &str) -> String {
        let request = Request::builder()
            .uri(uri)
            .body(axum::body::Body::empty())
            .unwrap();
        normalize(request).uri().to_string()
    }

    #[test]
    fn paths_are_normalised() {
        assert_eq!(
            path("/Users/AuthenticateByName"),
            "/users/authenticatebyname"
        );
        assert_eq!(
            path("/emby/Items/ABC/?Fields=Genres"),
            "/items/abc?Fields=Genres"
        );
        assert_eq!(path("/mediabrowser/System/Info/"), "/system/info");
        assert_eq!(path("/embyx/Items"), "/embyx/items");
        assert_eq!(path("/emby"), "/");
    }
}
