//! fanart.tv: high-quality title logos, clear art, banners and backgrounds.

use flixnet_db::metadata::NewImage;
use flixnet_db::{Db, cache};
use serde::Deserialize;

pub const DEFAULT_BASE_URL: &str = "https://webservice.fanart.tv/v3";
const TTL_MS: i64 = 7 * 24 * 60 * 60 * 1000;

#[derive(Clone)]
pub struct Fanart {
    http: reqwest::Client,
    api_key: String,
    base_url: String,
    language: String,
    db: Db,
}

#[derive(Debug, Clone, Deserialize)]
struct Art {
    url: String,
    lang: Option<String>,
    likes: Option<String>,
}

/// fanart.tv groups images by type; only the ones Flixnet uses are listed.
#[derive(Debug, Clone, Default, Deserialize)]
struct Response {
    #[serde(default)]
    hdmovielogo: Vec<Art>,
    #[serde(default)]
    movielogo: Vec<Art>,
    #[serde(default)]
    moviebackground: Vec<Art>,
    #[serde(default)]
    moviebanner: Vec<Art>,
    #[serde(default)]
    moviethumb: Vec<Art>,
    #[serde(default)]
    movieposter: Vec<Art>,
    #[serde(default)]
    hdtvlogo: Vec<Art>,
    #[serde(default)]
    clearlogo: Vec<Art>,
    #[serde(default)]
    showbackground: Vec<Art>,
    #[serde(default)]
    tvbanner: Vec<Art>,
    #[serde(default)]
    tvthumb: Vec<Art>,
    #[serde(default)]
    tvposter: Vec<Art>,
    // Music artists (by MusicBrainz id).
    #[serde(default)]
    artistthumb: Vec<Art>,
    #[serde(default)]
    artistbackground: Vec<Art>,
    #[serde(default)]
    hdmusiclogo: Vec<Art>,
    #[serde(default)]
    musiclogo: Vec<Art>,
    #[serde(default)]
    musicbanner: Vec<Art>,
}

impl Fanart {
    /// Images for a music artist by its MusicBrainz id.
    pub async fn artist_images(&self, mbid: &str) -> anyhow::Result<Vec<NewImage>> {
        let id: String = mbid
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
            .collect();
        let Some(r) = self.fetch(&format!("/music/{id}")).await? else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        out.extend(self.rank("poster", &[&r.artistthumb], 2));
        out.extend(self.rank("backdrop", &[&r.artistbackground], 3));
        out.extend(self.rank("logo", &[&r.hdmusiclogo, &r.musiclogo], 2));
        out.extend(self.rank("banner", &[&r.musicbanner], 1));
        Ok(out)
    }

    pub fn new(api_key: impl Into<String>, language: &str, db: Db) -> Self {
        Self {
            http: crate::http::client(),
            api_key: api_key.into(),
            base_url: DEFAULT_BASE_URL.to_owned(),
            language: language.split('-').next().unwrap_or("de").to_owned(),
            db,
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    async fn fetch(&self, path: &str) -> anyhow::Result<Option<Response>> {
        if let Some(body) = cache::get(&self.db, "fanart", path).await? {
            return Ok(Some(serde_json::from_str(&body)?));
        }
        let url = format!("{}{path}", self.base_url.trim_end_matches('/'));
        let response = self
            .http
            .get(&url)
            .query(&[("api_key", &self.api_key)])
            .send()
            .await?;
        match response.status().as_u16() {
            200 => {
                let body = response.text().await?;
                let parsed = serde_json::from_str(&body)?;
                cache::put(&self.db, "fanart", path, &body, TTL_MS).await?;
                Ok(Some(parsed))
            }
            404 => Ok(None),
            401 | 403 => anyhow::bail!("fanart.tv-Schlüssel ist ungültig"),
            status => anyhow::bail!("fanart.tv antwortete mit Status {status}"),
        }
    }

    /// Images for a movie by its TMDB id.
    pub async fn movie_images(&self, tmdb_id: i64) -> anyhow::Result<Vec<NewImage>> {
        let Some(r) = self.fetch(&format!("/movies/{tmdb_id}")).await? else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        out.extend(self.rank("logo", &[&r.hdmovielogo, &r.movielogo], 3));
        out.extend(self.rank("backdrop", &[&r.moviebackground], 3));
        out.extend(self.rank("banner", &[&r.moviebanner], 1));
        out.extend(self.rank("thumb", &[&r.moviethumb], 1));
        out.extend(self.rank("poster", &[&r.movieposter], 2));
        Ok(out)
    }

    /// Images for a series by its TVDB id.
    pub async fn series_images(&self, tvdb_id: i64) -> anyhow::Result<Vec<NewImage>> {
        let Some(r) = self.fetch(&format!("/tv/{tvdb_id}")).await? else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        out.extend(self.rank("logo", &[&r.hdtvlogo, &r.clearlogo], 3));
        out.extend(self.rank("backdrop", &[&r.showbackground], 3));
        out.extend(self.rank("banner", &[&r.tvbanner], 1));
        out.extend(self.rank("thumb", &[&r.tvthumb], 1));
        out.extend(self.rank("poster", &[&r.tvposter], 2));
        Ok(out)
    }

    /// Metadata language first, then English, then textless; more likes first.
    /// Earlier groups (HD logos) beat later ones (SD logos).
    fn rank(&self, kind: &str, groups: &[&Vec<Art>], keep: usize) -> Vec<NewImage> {
        let mut all: Vec<NewImage> = groups
            .iter()
            .enumerate()
            .flat_map(|(group, arts)| arts.iter().map(move |art| (group, art)))
            .map(|(group, art)| {
                let lang = art.lang.as_deref().filter(|l| !l.is_empty() && *l != "00");
                let language_weight = match lang {
                    Some(l) if l == self.language => 3.0,
                    Some("en") => 2.0,
                    None => 1.5,
                    _ => 0.0,
                };
                let likes: f64 = art
                    .likes
                    .as_deref()
                    .and_then(|l| l.parse().ok())
                    .unwrap_or(0.0);
                NewImage {
                    kind: kind.to_owned(),
                    url: Some(art.url.clone()),
                    language: lang.map(str::to_owned),
                    // Above TMDB (max ~40) for logos: fanart.tv logos are the curated ones.
                    score: language_weight * 10.0 + 15.0 - group as f64 * 5.0
                        + likes.min(50.0) / 10.0,
                    ..Default::default()
                }
            })
            .collect();
        all.sort_by(|a, b| b.score.total_cmp(&a.score));
        all.truncate(keep);
        all
    }
}
