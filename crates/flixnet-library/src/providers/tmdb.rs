//! The Movie Database (TMDB) API v3.
//!
//! German first (`de-DE`), English as fallback for missing texts. Responses are
//! cached in the database so rescans and refreshes do not burn API quota.

use std::time::Duration;

use flixnet_db::{Db, cache};
use serde::Deserialize;
use serde::de::DeserializeOwned;

pub const DEFAULT_BASE_URL: &str = "https://api.themoviedb.org/3";
pub const IMAGE_BASE_URL: &str = "https://image.tmdb.org/t/p/original";

const DETAIL_TTL_MS: i64 = 7 * 24 * 60 * 60 * 1000;
const SEARCH_TTL_MS: i64 = 24 * 60 * 60 * 1000;

#[derive(Debug, thiserror::Error)]
pub enum TmdbError {
    #[error("TMDB-API-Schlüssel ist ungültig")]
    InvalidKey,
    #[error("nicht bei TMDB gefunden")]
    NotFound,
    #[error("TMDB-Anfrage fehlgeschlagen: {0}")]
    Http(#[from] reqwest::Error),
    #[error("TMDB antwortete mit Status {0}")]
    Status(u16),
    #[error("unerwartete TMDB-Antwort: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Db(#[from] flixnet_db::DbError),
}

#[derive(Debug, Clone)]
pub struct TmdbConfig {
    /// v3 API key or v4 read access token (starts with "eyJ").
    pub api_key: String,
    pub language: String,
    pub fallback_language: String,
    /// Country for certifications (FSK = "DE").
    pub country: String,
    pub base_url: String,
}

impl TmdbConfig {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            language: "de-DE".to_owned(),
            fallback_language: "en-US".to_owned(),
            country: "DE".to_owned(),
            base_url: DEFAULT_BASE_URL.to_owned(),
        }
    }
}

#[derive(Clone)]
pub struct Tmdb {
    http: reqwest::Client,
    config: TmdbConfig,
    db: Db,
}

// ---- Response types (only the fields Flixnet uses) ---------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct SearchResults<T> {
    // A function default avoids serde's implied `T: Default` bound.
    #[serde(default = "Vec::new")]
    pub results: Vec<T>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MovieResult {
    pub id: i64,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub original_title: String,
    pub release_date: Option<String>,
    #[serde(default)]
    pub popularity: f64,
    pub poster_path: Option<String>,
    pub overview: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TvResult {
    pub id: i64,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub original_name: String,
    pub first_air_date: Option<String>,
    #[serde(default)]
    pub popularity: f64,
    pub poster_path: Option<String>,
    pub overview: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FindResults {
    #[serde(default)]
    pub movie_results: Vec<MovieResult>,
    #[serde(default)]
    pub tv_results: Vec<TvResult>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Named {
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CastMember {
    pub id: i64,
    pub name: String,
    pub character: Option<String>,
    #[serde(default)]
    pub order: i64,
    pub profile_path: Option<String>,
    /// TV aggregate credits list roles instead of one character.
    #[serde(default)]
    pub roles: Vec<Role>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Role {
    pub character: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CrewMember {
    pub id: i64,
    pub name: String,
    #[serde(default)]
    pub job: String,
    pub profile_path: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Credits {
    #[serde(default)]
    pub cast: Vec<CastMember>,
    #[serde(default)]
    pub crew: Vec<CrewMember>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ImageInfo {
    pub file_path: String,
    pub iso_639_1: Option<String>,
    #[serde(default)]
    pub vote_average: f64,
    pub width: Option<i64>,
    pub height: Option<i64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Images {
    #[serde(default)]
    pub posters: Vec<ImageInfo>,
    #[serde(default)]
    pub backdrops: Vec<ImageInfo>,
    #[serde(default)]
    pub logos: Vec<ImageInfo>,
    /// Episode stills.
    #[serde(default)]
    pub stills: Vec<ImageInfo>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ExternalIds {
    pub imdb_id: Option<String>,
    pub tvdb_id: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReleaseDate {
    #[serde(default)]
    pub certification: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CountryReleases {
    pub iso_3166_1: String,
    #[serde(default)]
    pub release_dates: Vec<ReleaseDate>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ReleaseDates {
    #[serde(default)]
    pub results: Vec<CountryReleases>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ContentRating {
    pub iso_3166_1: String,
    #[serde(default)]
    pub rating: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ContentRatings {
    #[serde(default)]
    pub results: Vec<ContentRating>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MovieDetails {
    pub id: i64,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub original_title: String,
    pub overview: Option<String>,
    pub tagline: Option<String>,
    pub runtime: Option<i64>,
    pub release_date: Option<String>,
    pub vote_average: Option<f64>,
    #[serde(default)]
    pub genres: Vec<Named>,
    #[serde(default)]
    pub production_companies: Vec<Named>,
    pub imdb_id: Option<String>,
    #[serde(default)]
    pub credits: Credits,
    #[serde(default)]
    pub images: Images,
    #[serde(default)]
    pub release_dates: ReleaseDates,
    #[serde(default)]
    pub external_ids: ExternalIds,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SeasonSummary {
    pub season_number: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TvDetails {
    pub id: i64,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub original_name: String,
    pub overview: Option<String>,
    pub tagline: Option<String>,
    pub first_air_date: Option<String>,
    #[serde(default)]
    pub episode_run_time: Vec<i64>,
    pub vote_average: Option<f64>,
    #[serde(default)]
    pub genres: Vec<Named>,
    #[serde(default)]
    pub networks: Vec<Named>,
    #[serde(default)]
    pub created_by: Vec<CrewMember>,
    #[serde(default)]
    pub aggregate_credits: Credits,
    #[serde(default)]
    pub images: Images,
    #[serde(default)]
    pub content_ratings: ContentRatings,
    #[serde(default)]
    pub external_ids: ExternalIds,
    #[serde(default)]
    pub seasons: Vec<SeasonSummary>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EpisodeDetails {
    pub episode_number: i64,
    pub name: Option<String>,
    pub overview: Option<String>,
    pub air_date: Option<String>,
    pub runtime: Option<i64>,
    pub still_path: Option<String>,
    pub vote_average: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SeasonDetails {
    pub season_number: i64,
    pub name: Option<String>,
    pub overview: Option<String>,
    pub air_date: Option<String>,
    pub poster_path: Option<String>,
    #[serde(default)]
    pub episodes: Vec<EpisodeDetails>,
}

impl Tmdb {
    pub fn new(config: TmdbConfig, db: Db) -> Self {
        Self {
            http: crate::http::client(),
            config,
            db,
        }
    }

    pub fn config(&self) -> &TmdbConfig {
        &self.config
    }

    /// GET with response cache, auth, and retry on rate limiting.
    async fn get<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
        ttl_ms: i64,
    ) -> Result<T, TmdbError> {
        let mut sorted: Vec<_> = query.iter().map(|(k, v)| format!("{k}={v}")).collect();
        sorted.sort();
        let cache_key = format!("{path}?{}", sorted.join("&"));
        if let Some(body) = cache::get(&self.db, "tmdb", &cache_key).await? {
            return Ok(serde_json::from_str(&body)?);
        }

        let url = format!("{}{path}", self.config.base_url.trim_end_matches('/'));
        let bearer = self.config.api_key.starts_with("eyJ");
        for attempt in 0..4 {
            let mut request = self.http.get(&url).query(query);
            request = if bearer {
                request.bearer_auth(&self.config.api_key)
            } else {
                request.query(&[("api_key", &self.config.api_key)])
            };
            let response = request.send().await?;
            match response.status().as_u16() {
                200 => {
                    let body = response.text().await?;
                    let parsed = serde_json::from_str(&body)?;
                    cache::put(&self.db, "tmdb", &cache_key, &body, ttl_ms).await?;
                    return Ok(parsed);
                }
                401 => return Err(TmdbError::InvalidKey),
                404 => return Err(TmdbError::NotFound),
                429 if attempt < 3 => {
                    let wait = response
                        .headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.parse::<u64>().ok())
                        .unwrap_or(1)
                        .min(10);
                    tokio::time::sleep(Duration::from_secs(wait)).await;
                }
                status => return Err(TmdbError::Status(status)),
            }
        }
        Err(TmdbError::Status(429))
    }

    fn lang(&self) -> (&'static str, String) {
        ("language", self.config.language.clone())
    }

    pub async fn search_movie(
        &self,
        title: &str,
        year: Option<i64>,
    ) -> Result<Vec<MovieResult>, TmdbError> {
        let mut query = vec![
            ("query", title.to_owned()),
            ("include_adult", "false".to_owned()),
            self.lang(),
        ];
        if let Some(year) = year {
            query.push(("year", year.to_string()));
        }
        let results: SearchResults<MovieResult> =
            self.get("/search/movie", &query, SEARCH_TTL_MS).await?;
        // A wrong year in the file name should not hide the right movie.
        if results.results.is_empty() && year.is_some() {
            return Box::pin(self.search_movie(title, None)).await;
        }
        Ok(results.results)
    }

    pub async fn search_tv(
        &self,
        title: &str,
        year: Option<i64>,
    ) -> Result<Vec<TvResult>, TmdbError> {
        let mut query = vec![("query", title.to_owned()), self.lang()];
        if let Some(year) = year {
            query.push(("first_air_date_year", year.to_string()));
        }
        let results: SearchResults<TvResult> =
            self.get("/search/tv", &query, SEARCH_TTL_MS).await?;
        if results.results.is_empty() && year.is_some() {
            return Box::pin(self.search_tv(title, None)).await;
        }
        Ok(results.results)
    }

    /// Looks up by an external id: `source` is `imdb_id` or `tvdb_id`.
    pub async fn find(&self, external_id: &str, source: &str) -> Result<FindResults, TmdbError> {
        self.get(
            &format!("/find/{external_id}"),
            &[("external_source", source.to_owned()), self.lang()],
            DETAIL_TTL_MS,
        )
        .await
    }

    fn image_languages(&self) -> String {
        let primary = self.config.language.split('-').next().unwrap_or("de");
        format!("{primary},en,null")
    }

    pub async fn movie(&self, id: i64) -> Result<MovieDetails, TmdbError> {
        let query = [
            (
                "append_to_response",
                "credits,images,release_dates,external_ids".to_owned(),
            ),
            ("include_image_language", self.image_languages()),
            self.lang(),
        ];
        let mut details: MovieDetails = self
            .get(&format!("/movie/{id}"), &query, DETAIL_TTL_MS)
            .await?;
        if is_blank(&details.overview) || is_blank(&details.tagline) {
            let fallback: MovieDetails = self
                .get(
                    &format!("/movie/{id}"),
                    &[("language", self.config.fallback_language.clone())],
                    DETAIL_TTL_MS,
                )
                .await?;
            if is_blank(&details.overview) {
                details.overview = fallback.overview;
            }
            if is_blank(&details.tagline) {
                details.tagline = fallback.tagline;
            }
        }
        Ok(details)
    }

    pub async fn tv(&self, id: i64) -> Result<TvDetails, TmdbError> {
        let query = [
            (
                "append_to_response",
                "aggregate_credits,images,content_ratings,external_ids".to_owned(),
            ),
            ("include_image_language", self.image_languages()),
            self.lang(),
        ];
        let mut details: TvDetails = self
            .get(&format!("/tv/{id}"), &query, DETAIL_TTL_MS)
            .await?;
        if is_blank(&details.overview) {
            let fallback: TvDetails = self
                .get(
                    &format!("/tv/{id}"),
                    &[("language", self.config.fallback_language.clone())],
                    DETAIL_TTL_MS,
                )
                .await?;
            details.overview = fallback.overview;
        }
        Ok(details)
    }

    pub async fn season(&self, tv_id: i64, season: i64) -> Result<SeasonDetails, TmdbError> {
        let mut details: SeasonDetails = self
            .get(
                &format!("/tv/{tv_id}/season/{season}"),
                &[self.lang()],
                DETAIL_TTL_MS,
            )
            .await?;
        // Fill missing German episode texts from English.
        if details.episodes.iter().any(|e| is_blank(&e.overview))
            && let Ok(fallback) = self
                .get::<SeasonDetails>(
                    &format!("/tv/{tv_id}/season/{season}"),
                    &[("language", self.config.fallback_language.clone())],
                    DETAIL_TTL_MS,
                )
                .await
        {
            for episode in &mut details.episodes {
                if is_blank(&episode.overview)
                    && let Some(en) = fallback
                        .episodes
                        .iter()
                        .find(|e| e.episode_number == episode.episode_number)
                {
                    episode.overview = en.overview.clone();
                }
            }
        }
        Ok(details)
    }
}

fn is_blank(text: &Option<String>) -> bool {
    text.as_deref().is_none_or(|t| t.trim().is_empty())
}

pub fn image_url(path: &str) -> String {
    format!("{IMAGE_BASE_URL}{path}")
}
