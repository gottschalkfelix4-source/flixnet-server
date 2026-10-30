//! OpenSubtitles.com REST API v1: subtitle search and download.
//!
//! - Every request carries the admin's `Api-Key` and a `User-Agent` naming the app
//!   with its version (OpenSubtitles rejects generic agents).
//! - Downloads count against a daily quota: anonymously (API key only) a small one
//!   per IP, more with a user login. With username/password configured, `/login`
//!   gives a token (valid about a day, cached here) and possibly another API host
//!   (`base_url`, e.g. for VIP users) that later requests go to.
//! - Rate limits: at most a few requests per second; requests are spaced out and a
//!   `429` is retried once after its `Retry-After`.
//! - Search parameters are sent sorted and lower-case, ids without `tt` or leading
//!   zeros, as the API asks (it redirects otherwise).

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use reqwest::{Method, StatusCode};
use serde::Deserialize;

pub const DEFAULT_BASE_URL: &str = "https://api.opensubtitles.com/api/v1";
const USER_AGENT: &str = concat!("Flixnet v", env!("CARGO_PKG_VERSION"));
/// Spacing between two requests (OpenSubtitles allows about 5 per second).
const MIN_INTERVAL: Duration = Duration::from_millis(250);
/// Login tokens are valid for 24 h; renew a little earlier.
const TOKEN_TTL_MS: i64 = 23 * 60 * 60 * 1000;

#[derive(Debug, thiserror::Error)]
pub enum OpenSubtitlesError {
    #[error("OpenSubtitles rejected the API key")]
    InvalidKey,
    #[error("OpenSubtitles rejected username or password")]
    InvalidLogin,
    #[error("download quota exhausted: {0}")]
    QuotaExhausted(String),
    #[error("OpenSubtitles rate limit reached")]
    RateLimited,
    #[error("OpenSubtitles answered {0}")]
    Status(StatusCode),
    #[error(transparent)]
    Http(#[from] reqwest::Error),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenSubtitlesConfig {
    pub api_key: String,
    pub username: Option<String>,
    pub password: Option<String>,
    pub base_url: String,
}

impl OpenSubtitlesConfig {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            username: None,
            password: None,
            base_url: DEFAULT_BASE_URL.to_owned(),
        }
    }
}

/// What to look for. Ids are the most reliable; `moviehash` finds subtitles made for
/// exactly this file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchQuery {
    /// ISO 639-1 codes (`de`, `en`), comma-separated in the request.
    pub languages: Vec<String>,
    pub moviehash: Option<String>,
    pub imdb_id: Option<String>,
    pub tmdb_id: Option<i64>,
    /// Episodes: ids of the series plus season/episode number.
    pub parent_imdb_id: Option<String>,
    pub parent_tmdb_id: Option<i64>,
    pub season_number: Option<i64>,
    pub episode_number: Option<i64>,
    /// Free text (title), used when no id is known.
    pub query: Option<String>,
    /// `movie` | `episode`
    pub kind: Option<String>,
}

impl SearchQuery {
    /// Query parameters, alphabetically sorted and lower-case.
    pub fn params(&self) -> Vec<(&'static str, String)> {
        let imdb = |id: &str| {
            id.trim()
                .trim_start_matches("tt")
                .parse::<u64>()
                .ok()
                .map(|n| n.to_string())
        };
        let mut params = Vec::new();
        if let Some(n) = self.episode_number {
            params.push(("episode_number", n.to_string()));
        }
        if let Some(id) = self.imdb_id.as_deref().and_then(imdb) {
            params.push(("imdb_id", id));
        }
        if !self.languages.is_empty() {
            let mut languages: Vec<String> =
                self.languages.iter().map(|l| l.to_lowercase()).collect();
            languages.sort();
            languages.dedup();
            params.push(("languages", languages.join(",")));
        }
        if let Some(hash) = &self.moviehash {
            params.push(("moviehash", hash.to_lowercase()));
        }
        if let Some(id) = self.parent_imdb_id.as_deref().and_then(imdb) {
            params.push(("parent_imdb_id", id));
        }
        if let Some(id) = self.parent_tmdb_id {
            params.push(("parent_tmdb_id", id.to_string()));
        }
        if let Some(query) = self.query.as_deref().filter(|q| !q.trim().is_empty()) {
            params.push(("query", query.trim().to_lowercase()));
        }
        if let Some(n) = self.season_number {
            params.push(("season_number", n.to_string()));
        }
        if let Some(id) = self.tmdb_id {
            params.push(("tmdb_id", id.to_string()));
        }
        if let Some(kind) = &self.kind {
            params.push(("type", kind.clone()));
        }
        params
    }
}

/// One downloadable subtitle file from a search.
#[derive(Debug, Clone, PartialEq)]
pub struct SubtitleResult {
    /// What `/download` takes.
    pub file_id: i64,
    pub file_name: Option<String>,
    pub subtitle_id: String,
    /// ISO 639-1 (`de`), as OpenSubtitles reports it.
    pub language: Option<String>,
    pub release: Option<String>,
    pub download_count: i64,
    pub ratings: f64,
    pub votes: i64,
    pub hearing_impaired: bool,
    pub foreign_parts_only: bool,
    pub fps: Option<f64>,
    pub hd: bool,
    pub from_trusted: bool,
    pub machine_translated: bool,
    /// Made for exactly this file (same OpenSubtitles hash).
    pub moviehash_match: bool,
    pub uploader: Option<String>,
    pub upload_date: Option<String>,
    /// Number of files (multi-CD subtitles have several).
    pub file_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadLink {
    pub link: String,
    pub file_name: Option<String>,
    /// Downloads left today (when OpenSubtitles says).
    pub remaining: Option<i64>,
}

// ---- Wire format ------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct SearchResponse {
    #[serde(default)]
    data: Vec<SearchEntry>,
}

#[derive(Debug, Deserialize)]
struct SearchEntry {
    id: String,
    attributes: Attributes,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Attributes {
    language: Option<String>,
    download_count: Option<i64>,
    hearing_impaired: Option<bool>,
    hd: Option<bool>,
    fps: Option<f64>,
    votes: Option<i64>,
    ratings: Option<f64>,
    from_trusted: Option<bool>,
    foreign_parts_only: Option<bool>,
    upload_date: Option<String>,
    ai_translated: Option<bool>,
    machine_translated: Option<bool>,
    release: Option<String>,
    moviehash_match: Option<bool>,
    uploader: Option<Uploader>,
    files: Vec<WireFile>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Uploader {
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WireFile {
    file_id: i64,
    file_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LoginResponse {
    token: String,
    base_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DownloadResponse {
    link: Option<String>,
    file_name: Option<String>,
    remaining: Option<i64>,
    message: Option<String>,
}

// ---- Client -----------------------------------------------------------------------

/// Login tokens by (base URL, user), shared by all clients of this process.
static TOKENS: LazyLock<Mutex<HashMap<(String, String), Token>>> = LazyLock::new(Default::default);
/// When the last request was sent (spacing for the rate limit).
static LAST_REQUEST: LazyLock<tokio::sync::Mutex<Option<tokio::time::Instant>>> =
    LazyLock::new(Default::default);

#[derive(Debug, Clone)]
struct Token {
    token: String,
    /// API root to use with this token.
    base_url: String,
    expires_ms: i64,
}

#[derive(Clone)]
pub struct OpenSubtitles {
    http: reqwest::Client,
    config: OpenSubtitlesConfig,
}

impl OpenSubtitles {
    pub fn new(config: OpenSubtitlesConfig) -> Self {
        Self {
            http: crate::http::client(),
            config,
        }
    }

    pub fn config(&self) -> &OpenSubtitlesConfig {
        &self.config
    }

    fn has_login(&self) -> bool {
        self.config
            .username
            .as_deref()
            .is_some_and(|u| !u.is_empty())
            && self
                .config
                .password
                .as_deref()
                .is_some_and(|p| !p.is_empty())
    }

    fn token_key(&self) -> (String, String) {
        (
            self.config.base_url.clone(),
            self.config.username.clone().unwrap_or_default(),
        )
    }

    /// Waits until the next request may go out.
    async fn throttle() {
        let mut last = LAST_REQUEST.lock().await;
        if let Some(at) = *last {
            let next = at + MIN_INTERVAL;
            if next > tokio::time::Instant::now() {
                tokio::time::sleep_until(next).await;
            }
        }
        *last = Some(tokio::time::Instant::now());
    }

    /// Sends a request (with one retry after a 429).
    async fn send(
        &self,
        method: Method,
        url: &str,
        query: &[(&'static str, String)],
        body: Option<serde_json::Value>,
        token: Option<&str>,
    ) -> Result<reqwest::Response, OpenSubtitlesError> {
        for attempt in 0..2 {
            Self::throttle().await;
            let mut request = self
                .http
                .request(method.clone(), url)
                .header("Api-Key", &self.config.api_key)
                .header(reqwest::header::USER_AGENT, USER_AGENT)
                .header(reqwest::header::ACCEPT, "application/json")
                .query(query);
            if let Some(token) = token {
                request = request.bearer_auth(token);
            }
            if let Some(body) = &body {
                request = request.json(body);
            }
            let response = request.send().await?;
            if response.status() != StatusCode::TOO_MANY_REQUESTS {
                return Ok(response);
            }
            let wait = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<u64>().ok())
                .unwrap_or(1)
                .min(10);
            if attempt == 0 {
                tracing::debug!(wait, "OpenSubtitles rate limit, retrying");
                tokio::time::sleep(Duration::from_secs(wait)).await;
            }
        }
        Err(OpenSubtitlesError::RateLimited)
    }

    fn check(status: StatusCode) -> Result<(), OpenSubtitlesError> {
        match status {
            s if s.is_success() => Ok(()),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Err(OpenSubtitlesError::InvalidKey),
            StatusCode::TOO_MANY_REQUESTS => Err(OpenSubtitlesError::RateLimited),
            s => Err(OpenSubtitlesError::Status(s)),
        }
    }

    /// A login token (cached), or `None` without configured credentials.
    async fn token(&self) -> Result<Option<Token>, OpenSubtitlesError> {
        if !self.has_login() {
            return Ok(None);
        }
        let key = self.token_key();
        let now = flixnet_db::now_ms();
        if let Some(token) = TOKENS
            .lock()
            .unwrap()
            .get(&key)
            .filter(|t| t.expires_ms > now)
        {
            return Ok(Some(token.clone()));
        }
        let url = format!("{}/login", self.config.base_url.trim_end_matches('/'));
        let body = serde_json::json!({
            "username": self.config.username,
            "password": self.config.password,
        });
        let response = self.send(Method::POST, &url, &[], Some(body), None).await?;
        match response.status() {
            StatusCode::UNAUTHORIZED | StatusCode::BAD_REQUEST => {
                return Err(OpenSubtitlesError::InvalidLogin);
            }
            status => Self::check(status)?,
        }
        let login: LoginResponse = response.json().await?;
        // A login may move the user to another host (VIP); only follow it from the
        // official API (never redirect a configured stub elsewhere).
        let base_url = match login.base_url.as_deref().map(str::trim) {
            Some(host)
                if !host.is_empty()
                    && self.config.base_url == DEFAULT_BASE_URL
                    && host.ends_with("opensubtitles.com")
                    && !host.contains('/') =>
            {
                format!("https://{host}/api/v1")
            }
            _ => self.config.base_url.clone(),
        };
        let token = Token {
            token: login.token,
            base_url,
            expires_ms: now + TOKEN_TTL_MS,
        };
        TOKENS.lock().unwrap().insert(key, token.clone());
        Ok(Some(token))
    }

    fn forget_token(&self) {
        TOKENS.lock().unwrap().remove(&self.token_key());
    }

    /// Checks the key (and the login, if configured).
    pub async fn verify(&self) -> Result<(), OpenSubtitlesError> {
        self.forget_token();
        if self.has_login() {
            self.token().await?;
            return Ok(());
        }
        let url = format!(
            "{}/infos/formats",
            self.config.base_url.trim_end_matches('/')
        );
        let response = self.send(Method::GET, &url, &[], None, None).await?;
        Self::check(response.status())
    }

    pub async fn search(
        &self,
        query: &SearchQuery,
    ) -> Result<Vec<SubtitleResult>, OpenSubtitlesError> {
        let token = self.token().await?;
        let base = token
            .as_ref()
            .map_or(self.config.base_url.as_str(), |t| t.base_url.as_str());
        let url = format!("{}/subtitles", base.trim_end_matches('/'));
        let response = self
            .send(
                Method::GET,
                &url,
                &query.params(),
                None,
                token.as_ref().map(|t| t.token.as_str()),
            )
            .await?;
        if response.status() == StatusCode::UNAUTHORIZED && token.is_some() {
            self.forget_token();
        }
        Self::check(response.status())?;
        let body: SearchResponse = response.json().await?;
        Ok(body
            .data
            .into_iter()
            .filter_map(|entry| {
                let a = entry.attributes;
                let first = a.files.first()?;
                Some(SubtitleResult {
                    file_id: first.file_id,
                    file_name: first.file_name.clone(),
                    subtitle_id: entry.id,
                    language: a.language.map(|l| l.to_lowercase()),
                    release: a.release.filter(|r| !r.trim().is_empty()),
                    download_count: a.download_count.unwrap_or(0),
                    ratings: a.ratings.unwrap_or(0.0),
                    votes: a.votes.unwrap_or(0),
                    hearing_impaired: a.hearing_impaired.unwrap_or(false),
                    foreign_parts_only: a.foreign_parts_only.unwrap_or(false),
                    fps: a.fps.filter(|f| *f > 0.0),
                    hd: a.hd.unwrap_or(false),
                    from_trusted: a.from_trusted.unwrap_or(false),
                    machine_translated: a.machine_translated.unwrap_or(false)
                        || a.ai_translated.unwrap_or(false),
                    moviehash_match: a.moviehash_match.unwrap_or(false),
                    uploader: a.uploader.and_then(|u| u.name),
                    upload_date: a.upload_date,
                    file_count: a.files.len(),
                })
            })
            .collect())
    }

    /// Requests a temporary download link for a subtitle file (counts against the quota).
    pub async fn download_link(&self, file_id: i64) -> Result<DownloadLink, OpenSubtitlesError> {
        let token = self.token().await?;
        let base = token
            .as_ref()
            .map_or(self.config.base_url.as_str(), |t| t.base_url.as_str());
        let url = format!("{}/download", base.trim_end_matches('/'));
        let response = self
            .send(
                Method::POST,
                &url,
                &[],
                Some(serde_json::json!({ "file_id": file_id, "sub_format": "srt" })),
                token.as_ref().map(|t| t.token.as_str()),
            )
            .await?;
        let status = response.status();
        if status == StatusCode::NOT_ACCEPTABLE {
            let message = response
                .json::<DownloadResponse>()
                .await
                .ok()
                .and_then(|r| r.message)
                .unwrap_or_default();
            return Err(OpenSubtitlesError::QuotaExhausted(message));
        }
        if status == StatusCode::UNAUTHORIZED && token.is_some() {
            self.forget_token();
        }
        Self::check(status)?;
        let body: DownloadResponse = response.json().await?;
        let link = body.link.filter(|l| l.starts_with("http")).ok_or_else(|| {
            OpenSubtitlesError::QuotaExhausted(body.message.clone().unwrap_or_default())
        })?;
        Ok(DownloadLink {
            link,
            file_name: body.file_name,
            remaining: body.remaining,
        })
    }

    /// The subtitle file behind a download link (at most 10 MiB).
    pub async fn fetch(&self, link: &str) -> Result<Vec<u8>, OpenSubtitlesError> {
        let response = self
            .http
            .get(link)
            .header(reqwest::header::USER_AGENT, USER_AGENT)
            .send()
            .await?;
        Self::check(response.status())?;
        if response
            .content_length()
            .is_some_and(|n| n > MAX_SUBTITLE_BYTES)
        {
            return Err(OpenSubtitlesError::Status(StatusCode::PAYLOAD_TOO_LARGE));
        }
        let bytes = response.bytes().await?;
        if bytes.len() as u64 > MAX_SUBTITLE_BYTES {
            return Err(OpenSubtitlesError::Status(StatusCode::PAYLOAD_TOO_LARGE));
        }
        Ok(bytes.to_vec())
    }
}

const MAX_SUBTITLE_BYTES: u64 = 10 * 1024 * 1024;

// ---- Movie hash -------------------------------------------------------------------

/// The OpenSubtitles hash of a video file: its size plus the sum of the 64-bit
/// little-endian words of the first and the last 64 KiB (wrapping), as 16 hex digits.
/// Blocking file I/O; run it with `spawn_blocking`.
pub fn moviehash(path: &Path) -> std::io::Result<String> {
    const CHUNK: u64 = 64 * 1024;
    let mut file = std::fs::File::open(path)?;
    let size = file.metadata()?.len();
    let mut hash = size;
    let mut add = |buf: &[u8]| {
        for word in buf.chunks(8) {
            let mut bytes = [0u8; 8];
            bytes[..word.len()].copy_from_slice(word);
            hash = hash.wrapping_add(u64::from_le_bytes(bytes));
        }
    };
    let len = CHUNK.min(size);
    let mut buf = vec![0u8; len as usize];
    file.read_exact(&mut buf)?;
    add(&buf);
    file.seek(SeekFrom::Start(size - len))?;
    file.read_exact(&mut buf)?;
    add(&buf);
    Ok(format!("{hash:016x}"))
}

/// How well a subtitle's release name fits a file name, 0–100: the share of the
/// release's words (resolution, source, codec, group …) that the file name has too.
/// A matching movie hash is a perfect fit.
pub fn match_score(result: &SubtitleResult, file_name: &str) -> i64 {
    if result.moviehash_match {
        return 100;
    }
    let words = |s: &str| -> Vec<String> {
        s.to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .map(str::to_owned)
            .collect()
    };
    let stem = Path::new(file_name)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| file_name.to_owned());
    let file_words = words(&stem);
    let release = result
        .release
        .clone()
        .or_else(|| {
            result.file_name.as_deref().map(|f| {
                Path::new(f)
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default()
            })
        })
        .unwrap_or_default();
    let mut release_words = words(&release);
    release_words.dedup();
    if release_words.is_empty() || file_words.is_empty() {
        return 0;
    }
    let shared = release_words
        .iter()
        .filter(|w| file_words.contains(w))
        .count();
    // 95 at most: only the hash is certain.
    (shared as i64 * 95) / release_words.len().max(file_words.len()) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_params_are_sorted_and_normalized() {
        let query = SearchQuery {
            languages: vec!["EN".into(), "de".into()],
            moviehash: Some("8E245D9679D31E12".into()),
            imdb_id: Some("tt0068646".into()),
            tmdb_id: Some(238),
            query: Some("  Der Pate ".into()),
            kind: Some("movie".into()),
            ..Default::default()
        };
        let names: Vec<&str> = query.params().iter().map(|(k, _)| *k).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted);
        assert_eq!(
            query.params(),
            [
                ("imdb_id", "68646".to_owned()),
                ("languages", "de,en".to_owned()),
                ("moviehash", "8e245d9679d31e12".to_owned()),
                ("query", "der pate".to_owned()),
                ("tmdb_id", "238".to_owned()),
                ("type", "movie".to_owned()),
            ]
        );
        let episode = SearchQuery {
            parent_imdb_id: Some("tt0903747".into()),
            parent_tmdb_id: Some(1396),
            season_number: Some(1),
            episode_number: Some(2),
            ..Default::default()
        };
        let names: Vec<&str> = episode.params().iter().map(|(k, _)| *k).collect();
        assert_eq!(
            names,
            [
                "episode_number",
                "parent_imdb_id",
                "parent_tmdb_id",
                "season_number"
            ]
        );
    }

    #[test]
    fn moviehash_sums_size_head_and_tail() {
        let dir = tempfile::tempdir().unwrap();
        // 128 KiB of zeros: the hash is the size.
        let zeros = dir.path().join("zeros.bin");
        std::fs::write(&zeros, vec![0u8; 131_072]).unwrap();
        assert_eq!(moviehash(&zeros).unwrap(), "0000000000020000");

        // One word in the head, one in the tail, one in the middle (not hashed).
        let mut data = vec![0u8; 200_000];
        data[0] = 1;
        data[100_000] = 0xff;
        data[199_992..200_000].copy_from_slice(&2u64.to_le_bytes());
        let file = dir.path().join("data.bin");
        std::fs::write(&file, &data).unwrap();
        assert_eq!(
            moviehash(&file).unwrap(),
            format!("{:016x}", 200_000u64 + 1 + 2)
        );

        // Smaller than 64 KiB: head and tail are the whole file.
        let small = dir.path().join("small.bin");
        std::fs::write(&small, 5u64.to_le_bytes()).unwrap();
        assert_eq!(moviehash(&small).unwrap(), format!("{:016x}", 8u64 + 10));
    }

    #[test]
    fn release_names_are_scored_against_the_file_name() {
        let result = |release: &str| SubtitleResult {
            file_id: 1,
            file_name: None,
            subtitle_id: "1".into(),
            language: Some("de".into()),
            release: Some(release.into()),
            download_count: 0,
            ratings: 0.0,
            votes: 0,
            hearing_impaired: false,
            foreign_parts_only: false,
            fps: None,
            hd: false,
            from_trusted: false,
            machine_translated: false,
            moviehash_match: false,
            uploader: None,
            upload_date: None,
            file_count: 1,
        };
        let file = "Der.Pate.1972.German.DL.1080p.BluRay.x264-GRP.mkv";
        let exact = match_score(
            &result("Der.Pate.1972.German.DL.1080p.BluRay.x264-GRP"),
            file,
        );
        let other = match_score(&result("The.Godfather.1972.720p.WEB-DL.x265-XYZ"), file);
        assert_eq!(exact, 95);
        assert!(other < 30, "{other}");
        assert!(match_score(&result(""), file) == 0);
        let mut hashed = result("whatever");
        hashed.moviehash_match = true;
        assert_eq!(match_score(&hashed, file), 100);
    }
}
