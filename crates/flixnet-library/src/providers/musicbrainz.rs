//! MusicBrainz (releases, recordings, artists) and the Cover Art Archive.
//!
//! MusicBrainz asks for at most one request per second per client and a meaningful
//! User-Agent with contact information; both are enforced here for every request
//! that leaves the process (cache hits are free). Responses are cached in the
//! provider cache: lookups by MBID for 30 days, searches for 7 days. 503 answers
//! (MusicBrainz' "slow down") are retried once after the advised wait.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use flixnet_db::{Db, cache};
use reqwest::StatusCode;
use serde::Deserialize;
use serde::de::DeserializeOwned;

pub const DEFAULT_BASE_URL: &str = "https://musicbrainz.org/ws/2";
pub const DEFAULT_COVER_ART_URL: &str = "https://coverartarchive.org";
/// Override the API roots (tests and stubs).
pub const BASE_URL_ENV: &str = "FLIXNET_MUSICBRAINZ_URL";
pub const COVER_ART_URL_ENV: &str = "FLIXNET_COVERART_URL";

pub const USER_AGENT: &str = concat!(
    "Flixnet/",
    env!("CARGO_PKG_VERSION"),
    " ( https://github.com/gottschalkfelix4-source/flixnet-server )"
);

/// MusicBrainz: one request per second.
pub const MIN_INTERVAL: Duration = Duration::from_secs(1);
/// The Cover Art Archive has no published limit; stay polite anyway.
const COVER_ART_INTERVAL: Duration = Duration::from_millis(250);

const LOOKUP_TTL_MS: i64 = 30 * 24 * 60 * 60 * 1000;
const SEARCH_TTL_MS: i64 = 7 * 24 * 60 * 60 * 1000;

#[derive(Debug, thiserror::Error)]
pub enum MusicBrainzError {
    #[error("MusicBrainz-Anfrage fehlgeschlagen: {0}")]
    Http(#[from] reqwest::Error),
    #[error("MusicBrainz antwortete mit Status {0}")]
    Status(u16),
    #[error("MusicBrainz drosselt die Anfragen")]
    RateLimited,
    #[error("unerwartete MusicBrainz-Antwort: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Db(#[from] flixnet_db::DbError),
}

// ---- Rate limiting --------------------------------------------------------------------

/// Spaces requests to one host: the next request may leave `interval` after the last.
#[derive(Debug)]
pub struct RateLimiter {
    interval: Duration,
    last: tokio::sync::Mutex<Option<tokio::time::Instant>>,
}

impl RateLimiter {
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            last: tokio::sync::Mutex::new(None),
        }
    }

    /// Waits for the next slot. Holding the lock while sleeping keeps callers in
    /// order, so concurrent jobs queue up instead of bursting.
    pub async fn wait(&self) {
        let mut last = self.last.lock().await;
        if let Some(at) = *last {
            let next = at + self.interval;
            if next > tokio::time::Instant::now() {
                tokio::time::sleep_until(next).await;
            }
        }
        *last = Some(tokio::time::Instant::now());
    }
}

/// One limiter per API root, shared by every client in the process (MusicBrainz
/// counts per client IP, not per object).
fn limiter(base_url: &str, interval: Duration) -> Arc<RateLimiter> {
    static LIMITERS: LazyLock<Mutex<HashMap<String, Arc<RateLimiter>>>> =
        LazyLock::new(Default::default);
    LIMITERS
        .lock()
        .expect("limiter map")
        .entry(base_url.to_owned())
        .or_insert_with(|| Arc::new(RateLimiter::new(interval)))
        .clone()
}

// ---- Response types (only the fields Flixnet uses) ------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct ArtistRef {
    pub id: String,
    pub name: String,
    #[serde(rename = "sort-name")]
    pub sort_name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ArtistCredit {
    pub name: String,
    #[serde(default)]
    pub joinphrase: String,
    pub artist: ArtistRef,
}

/// "A feat. B" from an artist credit.
pub fn credit_name(credits: &[ArtistCredit]) -> String {
    credits
        .iter()
        .map(|c| format!("{}{}", c.name, c.joinphrase))
        .collect::<String>()
        .trim()
        .to_owned()
}

#[derive(Debug, Clone, Deserialize)]
pub struct Genre {
    pub name: String,
    #[serde(default)]
    pub count: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReleaseGroup {
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(rename = "primary-type")]
    pub primary_type: Option<String>,
    #[serde(rename = "first-release-date")]
    pub first_release_date: Option<String>,
    #[serde(default)]
    pub genres: Vec<Genre>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RecordingRef {
    pub id: String,
    #[serde(default)]
    pub title: String,
    pub length: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Track {
    /// Release track id (`MUSICBRAINZ_RELEASETRACKID`).
    pub id: String,
    pub position: i64,
    #[serde(default)]
    pub title: String,
    pub length: Option<i64>,
    pub recording: Option<RecordingRef>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Medium {
    pub position: i64,
    pub format: Option<String>,
    #[serde(rename = "track-count", default)]
    pub track_count: i64,
    #[serde(default)]
    pub tracks: Vec<Track>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Release {
    pub id: String,
    pub title: String,
    pub date: Option<String>,
    pub country: Option<String>,
    #[serde(rename = "artist-credit", default)]
    pub artist_credit: Vec<ArtistCredit>,
    #[serde(rename = "release-group")]
    pub release_group: Option<ReleaseGroup>,
    #[serde(default)]
    pub genres: Vec<Genre>,
    #[serde(default)]
    pub media: Vec<Medium>,
    /// Search results only: 0–100.
    pub score: Option<i64>,
    #[serde(rename = "track-count")]
    pub track_count: Option<i64>,
}

impl Release {
    /// Tracks on all media (search results only list counts).
    pub fn total_tracks(&self) -> i64 {
        self.track_count.unwrap_or_else(|| {
            self.media
                .iter()
                .map(|m| m.track_count.max(m.tracks.len() as i64))
                .sum()
        })
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct LifeSpan {
    pub begin: Option<String>,
    pub end: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Artist {
    pub id: String,
    pub name: String,
    #[serde(rename = "sort-name")]
    pub sort_name: Option<String>,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub country: Option<String>,
    pub disambiguation: Option<String>,
    #[serde(rename = "life-span")]
    pub life_span: Option<LifeSpan>,
    #[serde(default)]
    pub genres: Vec<Genre>,
    /// Search results only: 0–100.
    pub score: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Recording {
    pub id: String,
    pub title: String,
    pub length: Option<i64>,
    #[serde(rename = "artist-credit", default)]
    pub artist_credit: Vec<ArtistCredit>,
    #[serde(default)]
    pub genres: Vec<Genre>,
}

#[derive(Debug, Clone, Deserialize)]
struct ReleaseSearch {
    #[serde(default)]
    releases: Vec<Release>,
}

#[derive(Debug, Clone, Deserialize)]
struct ArtistSearch {
    #[serde(default)]
    artists: Vec<Artist>,
}

#[derive(Debug, Clone, Deserialize)]
struct CoverArtImage {
    #[serde(default)]
    front: bool,
    image: String,
    #[serde(default)]
    thumbnails: HashMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
struct CoverArtListing {
    #[serde(default)]
    images: Vec<CoverArtImage>,
}

/// The top genres by vote count, title-cased ("punk rock" → "Punk Rock").
pub fn top_genres(genres: &[Genre], keep: usize) -> Vec<String> {
    let mut sorted: Vec<&Genre> = genres.iter().collect();
    sorted.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.name.cmp(&b.name)));
    sorted
        .into_iter()
        .take(keep)
        .map(|g| {
            g.name
                .split(' ')
                .map(|word| {
                    let mut chars = word.chars();
                    match chars.next() {
                        Some(first) => first.to_uppercase().chain(chars).collect(),
                        None => String::new(),
                    }
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

/// Lucene phrase for MusicBrainz search queries (quotes and backslashes escaped).
fn phrase(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

// ---- Client ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct MusicBrainz {
    http: reqwest::Client,
    base_url: String,
    cover_art_url: String,
    db: Db,
}

impl MusicBrainz {
    /// Client for the public services (or the roots from the environment overrides).
    pub fn new(db: Db) -> Self {
        let base = std::env::var(BASE_URL_ENV)
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| DEFAULT_BASE_URL.to_owned());
        let cover = std::env::var(COVER_ART_URL_ENV)
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| DEFAULT_COVER_ART_URL.to_owned());
        Self {
            http: crate::http::client(),
            base_url: base,
            cover_art_url: cover,
            db,
        }
    }

    pub fn with_base_urls(mut self, base_url: &str, cover_art_url: &str) -> Self {
        self.base_url = base_url.trim_end_matches('/').to_owned();
        self.cover_art_url = cover_art_url.trim_end_matches('/').to_owned();
        self
    }

    /// GET with cache, User-Agent, rate limit and one retry on 503. `Ok(None)` = 404.
    async fn get<T: DeserializeOwned>(
        &self,
        provider: &str,
        root: &str,
        interval: Duration,
        path: &str,
        query: &[(&str, String)],
        ttl_ms: i64,
    ) -> Result<Option<T>, MusicBrainzError> {
        let mut sorted: Vec<String> = query.iter().map(|(k, v)| format!("{k}={v}")).collect();
        sorted.sort();
        let cache_key = format!("{path}?{}", sorted.join("&"));
        if let Some(body) = cache::get(&self.db, provider, &cache_key).await? {
            return Ok(match body.as_str() {
                "" => None, // cached 404
                body => Some(serde_json::from_str(body)?),
            });
        }
        let url = format!("{}{path}", root.trim_end_matches('/'));
        let limiter = limiter(root, interval);
        for attempt in 0..2 {
            limiter.wait().await;
            let response = self
                .http
                .get(&url)
                .header(reqwest::header::USER_AGENT, USER_AGENT)
                .header(reqwest::header::ACCEPT, "application/json")
                .query(query)
                .send()
                .await?;
            match response.status() {
                StatusCode::OK => {
                    let body = response.text().await?;
                    let parsed = serde_json::from_str(&body)?;
                    cache::put(&self.db, provider, &cache_key, &body, ttl_ms).await?;
                    return Ok(Some(parsed));
                }
                StatusCode::NOT_FOUND => {
                    // Remember misses too (shorter), so rescans do not ask again.
                    cache::put(&self.db, provider, &cache_key, "", SEARCH_TTL_MS).await?;
                    return Ok(None);
                }
                StatusCode::SERVICE_UNAVAILABLE | StatusCode::TOO_MANY_REQUESTS => {
                    let wait = response
                        .headers()
                        .get(reqwest::header::RETRY_AFTER)
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.trim().parse::<u64>().ok())
                        .unwrap_or(1)
                        .clamp(1, 10);
                    if attempt == 0 {
                        tracing::debug!(wait, %url, "MusicBrainz asks to slow down");
                        tokio::time::sleep(Duration::from_secs(wait)).await;
                    }
                }
                status => return Err(MusicBrainzError::Status(status.as_u16())),
            }
        }
        Err(MusicBrainzError::RateLimited)
    }

    async fn mb<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
        ttl_ms: i64,
    ) -> Result<Option<T>, MusicBrainzError> {
        let mut query = query.to_vec();
        query.push(("fmt", "json".to_owned()));
        let base = self.base_url.clone();
        self.get("musicbrainz", &base, MIN_INTERVAL, path, &query, ttl_ms)
            .await
    }

    /// A release with its tracks, release group and genres.
    pub async fn release(&self, mbid: &str) -> Result<Option<Release>, MusicBrainzError> {
        self.mb(
            &format!("/release/{}", path_id(mbid)),
            &[(
                "inc",
                "artist-credits+release-groups+genres+recordings".to_owned(),
            )],
            LOOKUP_TTL_MS,
        )
        .await
    }

    /// Releases by title and artist, best first (MusicBrainz' own score).
    pub async fn search_releases(
        &self,
        title: &str,
        artist: Option<&str>,
    ) -> Result<Vec<Release>, MusicBrainzError> {
        let mut query = format!("release:{}", phrase(title));
        if let Some(artist) = artist {
            query.push_str(&format!(" AND artist:{}", phrase(artist)));
        }
        let found: Option<ReleaseSearch> = self
            .mb(
                "/release",
                &[("query", query), ("limit", "10".to_owned())],
                SEARCH_TTL_MS,
            )
            .await?;
        Ok(found.map(|f| f.releases).unwrap_or_default())
    }

    pub async fn artist(&self, mbid: &str) -> Result<Option<Artist>, MusicBrainzError> {
        self.mb(
            &format!("/artist/{}", path_id(mbid)),
            &[("inc", "genres".to_owned())],
            LOOKUP_TTL_MS,
        )
        .await
    }

    pub async fn search_artists(&self, name: &str) -> Result<Vec<Artist>, MusicBrainzError> {
        let found: Option<ArtistSearch> = self
            .mb(
                "/artist",
                &[
                    ("query", format!("artist:{}", phrase(name))),
                    ("limit", "5".to_owned()),
                ],
                SEARCH_TTL_MS,
            )
            .await?;
        Ok(found.map(|f| f.artists).unwrap_or_default())
    }

    pub async fn recording(&self, mbid: &str) -> Result<Option<Recording>, MusicBrainzError> {
        self.mb(
            &format!("/recording/{}", path_id(mbid)),
            &[("inc", "artist-credits+genres".to_owned())],
            LOOKUP_TTL_MS,
        )
        .await
    }

    /// Front cover from the Cover Art Archive: the release's own, else its release
    /// group's. Returns a ~500 px thumbnail URL when there is one.
    pub async fn front_cover(
        &self,
        release_mbid: &str,
        release_group_mbid: Option<&str>,
    ) -> Result<Option<String>, MusicBrainzError> {
        let root = self.cover_art_url.clone();
        let mut paths = vec![format!("/release/{}", path_id(release_mbid))];
        if let Some(group) = release_group_mbid {
            paths.push(format!("/release-group/{}", path_id(group)));
        }
        for path in paths {
            let listing: Option<CoverArtListing> = self
                .get(
                    "coverartarchive",
                    &root,
                    COVER_ART_INTERVAL,
                    &path,
                    &[],
                    LOOKUP_TTL_MS,
                )
                .await?;
            let front = listing.and_then(|l| {
                let mut images = l.images;
                images.sort_by_key(|i| !i.front);
                images.into_iter().next()
            });
            if let Some(image) = front {
                let url = ["500", "large", "1200"]
                    .iter()
                    .find_map(|k| image.thumbnails.get(*k).cloned())
                    .unwrap_or(image.image);
                // The archive answers with http:// links; https works the same.
                return Ok(Some(url.replacen("http://", "https://", 1)));
            }
        }
        Ok(None)
    }
}

/// MBIDs are UUIDs; anything else is reduced to safe characters for the path.
fn path_id(id: &str) -> String {
    id.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        assert_eq!(phrase(r#"Say "Hi" \o/"#), r#""Say \"Hi\" \\o/""#);
        assert_eq!(path_id("ab-12/../x?y"), "ab-12xy");
        let genres = vec![
            Genre {
                name: "punk rock".into(),
                count: 3,
            },
            Genre {
                name: "rock".into(),
                count: 5,
            },
        ];
        assert_eq!(top_genres(&genres, 5), ["Rock", "Punk Rock"]);
        let credits: Vec<ArtistCredit> = serde_json::from_str(
            r#"[{"name":"Anna","joinphrase":" feat. ","artist":{"id":"1","name":"Anna"}},
                {"name":"Bert","artist":{"id":"2","name":"Bert"}}]"#,
        )
        .unwrap();
        assert_eq!(credit_name(&credits), "Anna feat. Bert");
    }
}
