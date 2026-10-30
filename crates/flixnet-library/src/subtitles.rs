//! Subtitles from OpenSubtitles.com: settings, searching for a movie or episode,
//! downloading as a sidecar file (or into the config store when the library folder
//! is not writable) and the optional automatic download for new items.

use std::path::{Path, PathBuf};
use std::time::Duration;

use flixnet_core::jobs::{JobContext, JobHandler, Pool};
use flixnet_core::scheduler::TaskDef;
use flixnet_db::subtitles::{self as store, DownloadedSubtitle, NewDownloadedSubtitle};
use flixnet_db::{Db, items, media, settings};
use serde::{Deserialize, Serialize};

use crate::parse::language_code;
pub use crate::providers::opensubtitles::{
    DEFAULT_BASE_URL, OpenSubtitles, OpenSubtitlesConfig, OpenSubtitlesError, SearchQuery,
    SubtitleResult, match_score, moviehash,
};

pub const SETTINGS_KEY: &str = "opensubtitles";
/// Overrides the API root (tests point it at a stub server).
pub const BASE_URL_ENV: &str = "FLIXNET_OPENSUBTITLES_URL";
/// Automatic downloads per run: the daily quota is small (20 with a free account).
const AUTO_DOWNLOADS_PER_RUN: usize = 10;

/// Admin-editable OpenSubtitles settings (stored as JSON setting `opensubtitles`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SubtitleSettings {
    pub api_key: Option<String>,
    /// Optional account: more downloads per day.
    pub username: Option<String>,
    pub password: Option<String>,
    /// Download subtitles for new movies and episodes automatically.
    pub auto_download: bool,
    /// ISO 639-1 codes for automatic downloads (`de`, `en`).
    pub languages: Vec<String>,
    /// Items added from this moment on count as new (set when switched on).
    pub auto_since_ms: Option<i64>,
    /// API root; not editable through the API (tests and stubs).
    pub base_url: Option<String>,
}

impl SubtitleSettings {
    pub fn client(&self) -> Option<OpenSubtitles> {
        let key = self
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|k| !k.is_empty())?;
        let mut config = OpenSubtitlesConfig::new(key);
        config.username = self.username.clone().filter(|u| !u.trim().is_empty());
        config.password = self.password.clone().filter(|p| !p.is_empty());
        if let Some(url) = self
            .base_url
            .clone()
            .or_else(|| std::env::var(BASE_URL_ENV).ok())
            .filter(|u| u.starts_with("http"))
        {
            config.base_url = url;
        }
        Some(OpenSubtitles::new(config))
    }
}

pub async fn load_settings(db: &Db) -> anyhow::Result<SubtitleSettings> {
    Ok(settings::get(db, SETTINGS_KEY).await?.unwrap_or_default())
}

/// A valid OpenSubtitles language code: `de`, `en`, `pt-br`, `zh-cn`.
pub fn is_language(code: &str) -> bool {
    let b = code.as_bytes();
    (b.len() == 2 && b.iter().all(u8::is_ascii_lowercase))
        || (b.len() == 5
            && b[2] == b'-'
            && b[..2].iter().chain(&b[3..]).all(u8::is_ascii_lowercase))
}

/// ISO 639-2 code stored on the stream (`de` → `deu`); unknown codes stay as given.
pub fn stream_language(code: &str) -> String {
    let base = code.split('-').next().unwrap_or(code);
    language_code(base).unwrap_or(base).to_owned()
}

// ---- Search -----------------------------------------------------------------------

/// The file to find subtitles for and what to ask OpenSubtitles.
#[derive(Debug, Clone)]
pub struct Target {
    pub file: media::MediaFile,
    pub query: SearchQuery,
}

/// Search for a movie or episode: provider ids (the series' for episodes, with
/// season and episode number), the title as a fallback, and the movie hash of the
/// file. `file_id` picks one version; default is the first main file.
pub async fn target(
    db: &Db,
    item_id: i64,
    file_id: Option<i64>,
    languages: &[String],
) -> anyhow::Result<Option<Target>> {
    let Some(item) = items::get(db, item_id).await? else {
        return Ok(None);
    };
    if item.kind != "movie" && item.kind != "episode" {
        return Ok(None);
    }
    let files = media::files_for_item(db, item_id).await?;
    let Some(file) = files
        .into_iter()
        .filter(|f| f.role == "main" && f.missing_since.is_none())
        .find(|f| file_id.is_none_or(|id| id == f.id))
    else {
        return Ok(None);
    };
    let ids_of = |ids: &[(String, String)], provider: &str| {
        ids.iter()
            .find(|(p, _)| p == provider)
            .map(|(_, v)| v.clone())
    };
    let mut query = SearchQuery {
        languages: languages.to_vec(),
        ..Default::default()
    };
    if item.kind == "movie" {
        let ids = items::provider_ids(db, item_id).await?;
        query.kind = Some("movie".to_owned());
        query.tmdb_id = ids_of(&ids, "tmdb").and_then(|v| v.parse().ok());
        query.imdb_id = ids_of(&ids, "imdb");
        if query.tmdb_id.is_none() && query.imdb_id.is_none() {
            query.query = Some(item.title.clone());
        }
    } else {
        query.kind = Some("episode".to_owned());
        query.season_number = item.parent_index_number;
        query.episode_number = item.index_number;
        let series = match item.series_id {
            Some(id) => items::get(db, id).await?,
            None => None,
        };
        let ids = match &series {
            Some(s) => items::provider_ids(db, s.id).await?,
            None => Vec::new(),
        };
        query.parent_tmdb_id = ids_of(&ids, "tmdb").and_then(|v| v.parse().ok());
        query.parent_imdb_id = ids_of(&ids, "imdb");
        if query.parent_tmdb_id.is_none() && query.parent_imdb_id.is_none() {
            query.query = series.map(|s| s.title).or(Some(item.title.clone()));
        }
    }
    let path = PathBuf::from(&file.path);
    query.moviehash = tokio::task::spawn_blocking(move || moviehash(&path))
        .await?
        .map_err(|err| tracing::debug!(%err, "no movie hash"))
        .ok();
    Ok(Some(Target { file, query }))
}

/// A search result with its fit for the file (see [`match_score`]).
#[derive(Debug, Clone)]
pub struct Ranked {
    pub result: SubtitleResult,
    pub score: i64,
}

/// Results best first: fit, then downloads.
pub fn rank(results: Vec<SubtitleResult>, file_path: &str) -> Vec<Ranked> {
    let name = Path::new(file_path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut ranked: Vec<Ranked> = results
        .into_iter()
        .map(|result| Ranked {
            score: match_score(&result, &name),
            result,
        })
        .collect();
    ranked.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then(b.result.download_count.cmp(&a.result.download_count))
    });
    ranked
}

// ---- Download ---------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    #[error(transparent)]
    Provider(#[from] OpenSubtitlesError),
    #[error("could not store the subtitle file: {0}")]
    Store(std::io::Error),
    #[error(transparent)]
    Db(#[from] flixnet_db::DbError),
}

#[derive(Debug, Clone)]
pub struct Saved {
    pub stream_index: i64,
    pub path: PathBuf,
    /// In the config store (the library folder was not writable).
    pub in_store: bool,
    /// Downloads left today, if OpenSubtitles said.
    pub remaining: Option<i64>,
}

/// What to download and how to label it.
#[derive(Debug, Clone)]
pub struct Wanted<'a> {
    /// OpenSubtitles file id (from a search).
    pub subtitle_file_id: i64,
    /// OpenSubtitles language code (`de`).
    pub language: &'a str,
    pub hearing_impaired: bool,
    pub forced: bool,
    pub release: Option<&'a str>,
}

/// File name for the subtitle: `<video>.<lang>[.sdh][.forced][.<n>].srt`, which the
/// scanner reads back as language and flags.
fn subtitle_name(video: &Path, wanted: &Wanted<'_>, n: usize) -> String {
    let stem = video
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "subtitle".to_owned());
    let mut name = format!("{stem}.{}", wanted.language);
    if wanted.hearing_impaired {
        name.push_str(".sdh");
    }
    if wanted.forced {
        name.push_str(".forced");
    }
    if n > 1 {
        name.push_str(&format!(".{n}"));
    }
    name.push_str(".srt");
    name
}

/// Writes into `dir` under a free name; `Err` when the folder cannot be written.
async fn write_new(
    dir: &Path,
    video: &Path,
    wanted: &Wanted<'_>,
    bytes: &[u8],
) -> std::io::Result<PathBuf> {
    use tokio::io::AsyncWriteExt;
    for n in 1..=50 {
        let path = dir.join(subtitle_name(video, wanted, n));
        match tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .await
        {
            Ok(mut file) => {
                if let Err(err) = file.write_all(bytes).await {
                    drop(file);
                    let _ = tokio::fs::remove_file(&path).await;
                    return Err(err);
                }
                file.flush().await?;
                return Ok(path);
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "no free subtitle file name",
    ))
}

/// Stores subtitle bytes next to the video if its folder is writable, otherwise in
/// `<store_dir>/<file id>/`. Returns the path and whether it went to the store.
pub async fn store_file(
    video: &Path,
    file_id: i64,
    store_dir: &Path,
    wanted: &Wanted<'_>,
    bytes: &[u8],
) -> std::io::Result<(PathBuf, bool)> {
    if let Some(dir) = video.parent() {
        match write_new(dir, video, wanted, bytes).await {
            Ok(path) => return Ok((path, false)),
            Err(err) => {
                tracing::info!(dir = %dir.display(), %err, "library folder not writable; using the subtitle store");
            }
        }
    }
    let dir = store_dir.join(file_id.to_string());
    tokio::fs::create_dir_all(&dir).await?;
    write_new(&dir, video, wanted, bytes)
        .await
        .map(|p| (p, true))
}

/// Downloads a subtitle for a media file, stores it and registers it as an external
/// subtitle stream. All network and file I/O happens before the (short) DB write.
pub async fn download(
    db: &Db,
    client: &OpenSubtitles,
    store_dir: &Path,
    file: &media::MediaFile,
    wanted: &Wanted<'_>,
) -> Result<Saved, DownloadError> {
    let link = client.download_link(wanted.subtitle_file_id).await?;
    let bytes = client.fetch(&link.link).await?;
    let video = PathBuf::from(&file.path);
    let (path, in_store) = store_file(&video, file.id, store_dir, wanted, &bytes)
        .await
        .map_err(DownloadError::Store)?;
    let path_text = path.to_string_lossy().into_owned();
    let language = stream_language(wanted.language);
    let provider_file_id = wanted.subtitle_file_id.to_string();
    let stream_index = store::register(
        db,
        &NewDownloadedSubtitle {
            file_id: file.id,
            path: &path_text,
            language: Some(&language),
            is_sdh: wanted.hearing_impaired,
            is_forced: wanted.forced,
            provider: "opensubtitles",
            provider_file_id: Some(&provider_file_id),
            release_name: wanted.release,
        },
    )
    .await?;
    tracing::info!(file = file.id, path = %path.display(), "subtitle downloaded from OpenSubtitles");
    Ok(Saved {
        stream_index,
        path,
        in_store,
        remaining: link.remaining,
    })
}

/// Adds downloaded subtitles that a re-probe would lose (files in the config store;
/// sidecars are found by the scanner anyway) as external streams.
pub fn attach_downloaded(streams: &mut Vec<media::Stream>, downloaded: &[DownloadedSubtitle]) {
    for d in downloaded {
        let known = streams
            .iter()
            .any(|s| s.external_path.as_deref() == Some(d.path.as_str()));
        if known || !Path::new(&d.path).is_file() {
            continue;
        }
        let idx = streams
            .iter()
            .map(|s| s.idx + 1)
            .filter(|i| *i > 1000)
            .max()
            .unwrap_or(1000);
        streams.push(media::Stream {
            idx,
            kind: "subtitle".to_owned(),
            codec: Some("srt".to_owned()),
            language: d.language.clone(),
            is_forced: d.is_forced,
            is_sdh: d.is_sdh,
            is_external: true,
            external_path: Some(d.path.clone()),
            ..Default::default()
        });
    }
}

// ---- Automatic downloads ----------------------------------------------------------

/// The result to download automatically: right language, complete (not only foreign
/// parts), human-made and without hearing-impaired notes preferred, best fit first.
pub fn pick_automatic(ranked: &[Ranked], language: &str) -> Option<Ranked> {
    let mut candidates: Vec<&Ranked> = ranked
        .iter()
        .filter(|r| r.result.language.as_deref() == Some(language))
        .filter(|r| !r.result.foreign_parts_only && r.result.file_count == 1)
        .collect();
    candidates.sort_by_key(|r| {
        (
            !r.result.moviehash_match,
            r.result.machine_translated,
            r.result.hearing_impaired,
            -r.score,
            -r.result.download_count,
        )
    });
    candidates.first().map(|r| (*r).clone())
}

/// One run: for files added since automatic downloads were switched on, fetch a
/// subtitle in every configured language the file does not have yet. Returns the
/// number of downloads.
pub async fn auto_download(db: &Db, store_dir: &Path) -> anyhow::Result<usize> {
    let settings = load_settings(db).await?;
    if !settings.auto_download || settings.languages.is_empty() {
        return Ok(0);
    }
    let Some(client) = settings.client() else {
        return Ok(0);
    };
    let since = settings.auto_since_ms.unwrap_or(0);
    let mut downloads = 0;
    'files: for file_id in store::recent_main_files(db, since, 200).await? {
        let streams = media::streams(db, file_id).await?;
        for language in &settings.languages {
            if downloads >= AUTO_DOWNLOADS_PER_RUN {
                break 'files;
            }
            let wanted_language = stream_language(language);
            let has = streams.iter().any(|s| {
                s.kind == "subtitle"
                    && s.language.as_deref().is_some_and(|l| {
                        l.eq_ignore_ascii_case(&wanted_language) || l.eq_ignore_ascii_case(language)
                    })
            });
            if has || store::attempted(db, file_id, language).await? {
                continue;
            }
            let Some(file) = media::get_file(db, file_id).await? else {
                continue 'files;
            };
            let Some(item_id) = file.item_id else {
                continue 'files;
            };
            let Some(target) =
                target(db, item_id, Some(file_id), std::slice::from_ref(language)).await?
            else {
                continue 'files;
            };
            let results = match client.search(&target.query).await {
                Ok(results) => results,
                Err(OpenSubtitlesError::RateLimited) => break 'files,
                Err(err) => {
                    tracing::warn!(file = file_id, %err, "OpenSubtitles search failed");
                    break 'files;
                }
            };
            let Some(best) = pick_automatic(&rank(results, &target.file.path), language) else {
                store::record_attempt(db, file_id, language, false).await?;
                continue;
            };
            let wanted = Wanted {
                subtitle_file_id: best.result.file_id,
                language,
                hearing_impaired: best.result.hearing_impaired,
                forced: false,
                release: best.result.release.as_deref(),
            };
            match download(db, &client, store_dir, &target.file, &wanted).await {
                Ok(_) => {
                    store::record_attempt(db, file_id, language, true).await?;
                    downloads += 1;
                }
                Err(DownloadError::Provider(
                    OpenSubtitlesError::QuotaExhausted(_) | OpenSubtitlesError::RateLimited,
                )) => break 'files,
                Err(err) => {
                    tracing::warn!(file = file_id, %err, "automatic subtitle download failed");
                    store::record_attempt(db, file_id, language, false).await?;
                }
            }
        }
    }
    Ok(downloads)
}

pub struct AutoDownloadSubtitles {
    /// `<config>/subtitles`: where subtitles go when a library folder is read-only.
    pub store_dir: PathBuf,
}

impl JobHandler for AutoDownloadSubtitles {
    const KIND: &'static str = "subtitles.auto_download";
    const POOL: Pool = Pool::Io;
    const MAX_ATTEMPTS: i64 = 1;
    type Payload = ();

    async fn run(&self, ctx: JobContext, _: ()) -> anyhow::Result<()> {
        let n = auto_download(ctx.db(), &self.store_dir).await?;
        if n > 0 {
            tracing::info!(downloads = n, "subtitles downloaded automatically");
        }
        Ok(())
    }
}

pub fn tasks() -> Vec<TaskDef> {
    vec![TaskDef::new::<AutoDownloadSubtitles>(
        "download_subtitles",
        "Untertitel für neue Filme und Folgen von OpenSubtitles laden (wenn eingeschaltet)",
        Duration::from_secs(60 * 60),
        &(),
    )]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_codes() {
        assert!(is_language("de") && is_language("pt-br") && is_language("zh-cn"));
        assert!(
            !is_language("DE") && !is_language("deu") && !is_language("d") && !is_language("pt_br")
        );
        assert_eq!(stream_language("de"), "deu");
        assert_eq!(stream_language("pt-br"), "por");
        assert_eq!(stream_language("xx"), "xx");
    }

    #[test]
    fn names_carry_language_and_flags() {
        let video = Path::new("/media/Film (2020)/Film.2020.1080p.mkv");
        let mut wanted = Wanted {
            subtitle_file_id: 1,
            language: "de",
            hearing_impaired: false,
            forced: false,
            release: None,
        };
        assert_eq!(subtitle_name(video, &wanted, 1), "Film.2020.1080p.de.srt");
        wanted.hearing_impaired = true;
        assert_eq!(
            subtitle_name(video, &wanted, 2),
            "Film.2020.1080p.de.sdh.2.srt"
        );
        // The scanner reads them back.
        let info = crate::parse::subtitle_info(
            "Film.2020.1080p",
            Path::new("Film.2020.1080p.de.sdh.2.srt"),
        );
        assert_eq!(info, Some((Some("deu".to_owned()), false, true)));
    }

    #[tokio::test]
    async fn stores_next_to_the_video_or_in_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let wanted = Wanted {
            subtitle_file_id: 7,
            language: "en",
            hearing_impaired: false,
            forced: false,
            release: None,
        };
        let video = dir.path().join("lib/Film.mkv");
        std::fs::create_dir_all(video.parent().unwrap()).unwrap();
        let store = dir.path().join("store");
        let (first, in_store) = store_file(&video, 3, &store, &wanted, b"1").await.unwrap();
        assert!(!in_store);
        assert_eq!(first, dir.path().join("lib/Film.en.srt"));
        let (second, _) = store_file(&video, 3, &store, &wanted, b"2").await.unwrap();
        assert_eq!(
            second,
            dir.path().join("lib/Film.en.2.srt"),
            "never overwrites"
        );

        // The video's folder cannot be written: the store takes it.
        let gone = dir.path().join("missing/Film.mkv");
        let (path, in_store) = store_file(&gone, 3, &store, &wanted, b"3").await.unwrap();
        assert!(in_store);
        assert_eq!(path, store.join("3/Film.en.srt"));
        assert_eq!(std::fs::read(path).unwrap(), b"3");
    }

    #[test]
    fn downloaded_files_survive_a_reprobe() {
        let dir = tempfile::tempdir().unwrap();
        let stored = dir.path().join("Film.de.srt");
        std::fs::write(&stored, "1").unwrap();
        let entry = |path: &Path| DownloadedSubtitle {
            id: 1,
            file_id: 1,
            path: path.to_string_lossy().into_owned(),
            language: Some("deu".into()),
            is_sdh: false,
            is_forced: false,
            provider: "opensubtitles".into(),
            provider_file_id: None,
            release_name: None,
            created_at: 0,
        };
        let mut streams = vec![
            media::Stream {
                idx: 0,
                kind: "video".into(),
                ..Default::default()
            },
            media::Stream {
                idx: 1000,
                kind: "subtitle".into(),
                is_external: true,
                external_path: Some("/x/Film.en.srt".into()),
                ..Default::default()
            },
        ];
        attach_downloaded(
            &mut streams,
            &[
                entry(&stored),
                entry(Path::new("/gone/Film.de.srt")),
                entry(Path::new("/x/Film.en.srt")),
            ],
        );
        assert_eq!(
            streams.len(),
            3,
            "existing sidecar and missing file are skipped"
        );
        assert_eq!(streams[2].idx, 1001);
        assert_eq!(streams[2].language.as_deref(), Some("deu"));
    }
}
