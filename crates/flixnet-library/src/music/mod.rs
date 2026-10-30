//! Music libraries: tags, lyrics, and the metadata refresh of albums and artists
//! (folder and embedded artwork, MusicBrainz, Cover Art Archive, fanart.tv).
//!
//! Nothing here holds a write transaction across file or network I/O: everything is
//! read or fetched first and written with the small helpers of `flixnet_db`.

pub mod lyrics;
pub mod tags;

use std::path::{Path, PathBuf};

use flixnet_db::metadata::{self, Authority, ItemFields, NewImage};
use flixnet_db::{Db, items, music, userdata};

use crate::providers::musicbrainz::{self, MusicBrainz, Release};
use crate::providers::{Fanart, fanart_from_settings, musicbrainz_from_settings};
use crate::scan::normalize_key;
pub use tags::{AUDIO_EXTENSIONS, embedded_picture, is_audio_file, read_tags};

const IMAGE_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "webp"];
/// Automatic MusicBrainz matches need at least this score (0–1).
pub const MATCH_THRESHOLD: f64 = 0.85;

fn find_image(dir: &Path, names: &[&str]) -> Option<PathBuf> {
    names.iter().find_map(|name| {
        IMAGE_EXTENSIONS.iter().find_map(|ext| {
            let candidate = dir.join(format!("{name}.{ext}"));
            candidate.is_file().then_some(candidate)
        })
    })
}

/// The library folder a file lies in (its path minus the relative part).
fn library_root(path: &Path, relative: &Path) -> PathBuf {
    let mut root = path.to_path_buf();
    for _ in 0..relative.components().count() {
        root.pop();
    }
    root
}

/// Folders of an album's files, plus the folder above disc folders (`Album/CD1`).
fn album_dirs(files: &[music::AlbumFile]) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    for file in files {
        let path = Path::new(&file.path);
        let relative = Path::new(&file.relative_path);
        let Some(dir) = path.parent() else { continue };
        let root = library_root(path, relative);
        let mut candidates = vec![dir.to_path_buf()];
        let is_disc_folder = dir
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .is_some_and(|n| {
                let n = n.trim();
                ["cd", "disc", "disk"].iter().any(|p| {
                    n.strip_prefix(p)
                        .is_some_and(|rest| rest.trim().parse::<u32>().is_ok())
                })
            });
        if is_disc_folder && let Some(parent) = dir.parent() {
            candidates.push(parent.to_path_buf());
        }
        for candidate in candidates {
            // Never the library root itself: its folder.jpg belongs to nobody.
            if candidate != root && !dirs.contains(&candidate) {
                dirs.push(candidate);
            }
        }
    }
    dirs
}

/// Local album artwork: `cover`/`folder`/`front` images next to the tracks, plus the
/// cover embedded in a track.
fn album_art(dirs: &[PathBuf], cover_file: Option<&str>) -> (Vec<NewImage>, Vec<NewImage>) {
    let mut local = Vec::new();
    for (kind, names) in [
        (
            "poster",
            &["cover", "folder", "front", "album", "poster"][..],
        ),
        ("backdrop", &["fanart", "backdrop", "background"][..]),
    ] {
        if let Some(path) = dirs.iter().find_map(|d| find_image(d, names)) {
            local.push(NewImage {
                kind: kind.to_owned(),
                path: Some(path.to_string_lossy().into_owned()),
                score: 10.0,
                ..Default::default()
            });
        }
    }
    let embedded = cover_file
        .map(|file| {
            vec![NewImage {
                kind: "poster".to_owned(),
                path: Some(file.to_owned()),
                // Below folder art (local), above Cover Art Archive.
                score: 20.0,
                ..Default::default()
            }]
        })
        .unwrap_or_default();
    (local, embedded)
}

/// Refreshes one music item (albums and artists; tracks have nothing of their own).
pub async fn refresh(db: &Db, item_id: i64) -> anyhow::Result<()> {
    let Some(item) = items::get(db, item_id).await? else {
        return Ok(());
    };
    match item.kind.as_str() {
        "album" => {
            let mb = musicbrainz_from_settings(db).await?;
            refresh_album(db, &item, mb.as_ref()).await?;
        }
        "artist" => {
            let mb = musicbrainz_from_settings(db).await?;
            let fanart = fanart_from_settings(db).await?;
            refresh_artist(db, &item, mb.as_ref(), fanart.as_ref()).await?;
        }
        _ => {}
    }
    userdata::update_identity(db, item_id).await?;
    Ok(())
}

/// Album: year, genres and runtime from its tracks' tags, folder/embedded cover, then
/// MusicBrainz (ids, release date, genres, track ids) and the Cover Art Archive.
pub async fn refresh_album(
    db: &Db,
    album: &items::Item,
    mb: Option<&MusicBrainz>,
) -> anyhow::Result<()> {
    let summary = music::album_summary(db, album.id).await?;
    if summary.tracks == 0 {
        return Ok(());
    }
    metadata::apply_fields(
        db,
        album.id,
        &ItemFields {
            year: summary.year,
            ..Default::default()
        },
        Authority::Provider,
    )
    .await?;
    music::set_runtime(db, album.id, Some(summary.duration_ms).filter(|d| *d > 0)).await?;
    if !summary.genres.is_empty() {
        metadata::set_genres(db, album.id, &summary.genres).await?;
    }

    let files = music::files_below(db, album.id).await?;
    let dirs = album_dirs(&files);
    let cover_file = summary.cover_file.clone();
    let (local, embedded) =
        tokio::task::spawn_blocking(move || album_art(&dirs, cover_file.as_deref())).await?;
    metadata::replace_images(db, album.id, "local", &local).await?;
    metadata::replace_images(db, album.id, "embedded", &embedded).await?;

    if let Some(mb) = mb
        && let Err(err) = apply_musicbrainz_album(db, mb, album, &summary).await
    {
        tracing::warn!(album = album.id, %err, "MusicBrainz lookup failed");
    }
    metadata::mark_refreshed(db, album.id).await?;
    if let Some(artist_id) = album.parent_id {
        fallback_artist_poster(db, artist_id).await?;
    }
    Ok(())
}

/// Score of a release search hit for an album (0–~1).
pub fn release_score(
    release: &Release,
    title: &str,
    artist: &str,
    tracks: i64,
    various: bool,
) -> f64 {
    let similarity = |a: &str, b: &str| {
        let (a, b) = (normalize_key(a), normalize_key(b));
        if a == b {
            1.0
        } else {
            strsim::normalized_levenshtein(&a, &b)
        }
    };
    let title_score = similarity(&release.title, title);
    let credit = musicbrainz::credit_name(&release.artist_credit);
    let artist_score = if various {
        // "Various Artists" vs. "Verschiedene Interpreten": the name differs by language.
        1.0
    } else {
        similarity(&credit, artist)
    };
    let mb_score = release.score.unwrap_or(100) as f64 / 100.0;
    let track_score = match release.total_tracks() {
        0 => 0.5,
        n if n == tracks => 1.0,
        n if (n - tracks).abs() <= 2 => 0.6,
        _ => 0.0,
    };
    title_score * 0.45 + artist_score * 0.25 + mb_score * 0.15 + track_score * 0.15
}

fn provider_id(ids: &[(String, String)], provider: &str) -> Option<String> {
    ids.iter()
        .find(|(p, _)| p == provider)
        .map(|(_, v)| v.clone())
}

async fn apply_musicbrainz_album(
    db: &Db,
    mb: &MusicBrainz,
    album: &items::Item,
    summary: &music::AlbumSummary,
) -> anyhow::Result<()> {
    let ids = items::provider_ids(db, album.id).await?;
    let artist = match album.parent_id {
        Some(id) => items::get(db, id).await?.map(|a| a.title),
        None => None,
    }
    .unwrap_or_default();
    let various = summary.compilation || artist == tags::VARIOUS_ARTISTS;

    let (release_id, confidence) = match provider_id(&ids, "musicbrainz_album") {
        Some(id) => (Some(id), 1.0),
        None => {
            let hits = mb
                .search_releases(&album.title, (!various).then_some(artist.as_str()))
                .await?;
            let best = hits
                .iter()
                .map(|r| {
                    (
                        r,
                        release_score(r, &album.title, &artist, summary.tracks, various),
                    )
                })
                .max_by(|a, b| a.1.total_cmp(&b.1));
            match best {
                Some((release, score)) if score >= MATCH_THRESHOLD => {
                    (Some(release.id.clone()), score)
                }
                best => {
                    items::set_match(db, album.id, "failed", best.map(|b| b.1)).await?;
                    return Ok(());
                }
            }
        }
    };
    let Some(release_id) = release_id else {
        return Ok(());
    };
    let Some(release) = mb.release(&release_id).await? else {
        items::set_match(db, album.id, "failed", Some(0.0)).await?;
        return Ok(());
    };

    metadata::set_provider_id(db, album.id, "musicbrainz_album", &release.id).await?;
    if let Some(group) = &release.release_group {
        metadata::set_provider_id(db, album.id, "musicbrainz_releasegroup", &group.id).await?;
    }
    if let Some(credit) = release.artist_credit.first() {
        metadata::set_provider_id(db, album.id, "musicbrainz_albumartist", &credit.artist.id)
            .await?;
    }
    let date = release.date.clone().filter(|d| d.len() >= 4);
    metadata::apply_fields(
        db,
        album.id,
        &ItemFields {
            premiere_date: date.clone().filter(|d| d.len() == 10),
            year: summary
                .year
                .is_none()
                .then(|| {
                    release
                        .release_group
                        .as_ref()
                        .and_then(|g| g.first_release_date.clone())
                        .or(date)
                        .and_then(|d| d.get(..4)?.parse().ok())
                })
                .flatten(),
            ..Default::default()
        },
        Authority::Provider,
    )
    .await?;
    if summary.genres.is_empty() {
        let mut genres = musicbrainz::top_genres(&release.genres, 3);
        if genres.is_empty()
            && let Some(group) = &release.release_group
        {
            genres = musicbrainz::top_genres(&group.genres, 3);
        }
        if !genres.is_empty() {
            metadata::set_genres(db, album.id, &genres).await?;
        }
    }

    // Track ids by disc and position: stable identities for play counts.
    let tracks = items::children(db, album.id).await?;
    for medium in &release.media {
        for track in &medium.tracks {
            let Some(ours) = tracks.iter().find(|t| {
                t.kind == "track"
                    && t.parent_index_number.unwrap_or(1) == medium.position
                    && t.index_number == Some(track.position)
            }) else {
                continue;
            };
            let existing = items::provider_ids(db, ours.id).await?;
            if provider_id(&existing, "musicbrainz_track").is_none() {
                metadata::set_provider_id(db, ours.id, "musicbrainz_track", &track.id).await?;
            }
            if let Some(recording) = &track.recording
                && provider_id(&existing, "musicbrainz_recording").is_none()
            {
                metadata::set_provider_id(db, ours.id, "musicbrainz_recording", &recording.id)
                    .await?;
            }
            userdata::update_identity(db, ours.id).await?;
        }
    }

    let group = release.release_group.as_ref().map(|g| g.id.as_str());
    let covers = match mb.front_cover(&release.id, group).await {
        Ok(Some(url)) => vec![NewImage {
            kind: "poster".to_owned(),
            url: Some(url),
            score: 10.0,
            ..Default::default()
        }],
        Ok(None) => Vec::new(),
        Err(err) => {
            tracing::warn!(album = album.id, %err, "Cover Art Archive lookup failed");
            Vec::new()
        }
    };
    metadata::replace_images(db, album.id, "coverartarchive", &covers).await?;
    items::set_match(db, album.id, "matched", Some(confidence.min(1.0))).await?;
    Ok(())
}

/// Artist: folder artwork (`artist.jpg` in `Artist/`), MusicBrainz id, fanart.tv
/// images; without any picture the cover of one of their albums stands in.
pub async fn refresh_artist(
    db: &Db,
    artist: &items::Item,
    mb: Option<&MusicBrainz>,
    fanart: Option<&Fanart>,
) -> anyhow::Result<()> {
    let files = music::files_below(db, artist.id).await?;
    // `Artist/Album/Track`: the first folder is the artist's.
    let artist_dir = files.iter().find_map(|f| {
        let relative = Path::new(&f.relative_path);
        let first = relative.components().next()?;
        (relative.components().count() >= 3)
            .then(|| library_root(Path::new(&f.path), relative).join(first))
    });
    let local = match artist_dir {
        Some(dir) => {
            tokio::task::spawn_blocking(move || {
                let mut out = Vec::new();
                for (kind, names) in [
                    ("poster", &["artist", "folder", "poster"][..]),
                    ("backdrop", &["fanart", "backdrop", "background"][..]),
                    ("logo", &["logo", "clearlogo"][..]),
                    ("banner", &["banner"][..]),
                ] {
                    if let Some(path) = find_image(&dir, names) {
                        out.push(NewImage {
                            kind: kind.to_owned(),
                            path: Some(path.to_string_lossy().into_owned()),
                            score: 10.0,
                            ..Default::default()
                        });
                    }
                }
                out
            })
            .await?
        }
        None => Vec::new(),
    };
    metadata::replace_images(db, artist.id, "local", &local).await?;

    if let Some(mb) = mb
        && artist.title != tags::VARIOUS_ARTISTS
        && artist.title != tags::UNKNOWN_ARTIST
    {
        match artist_mbid(db, mb, artist).await {
            Ok(Some(mbid)) => {
                if let Some(fanart) = fanart {
                    match fanart.artist_images(&mbid).await {
                        Ok(images) => {
                            metadata::replace_images(db, artist.id, "fanart", &images).await?
                        }
                        Err(err) => {
                            tracing::warn!(artist = artist.id, %err, "fanart.tv lookup failed")
                        }
                    }
                }
            }
            Ok(None) => {}
            Err(err) => tracing::warn!(artist = artist.id, %err, "MusicBrainz lookup failed"),
        }
    }
    metadata::mark_refreshed(db, artist.id).await?;
    fallback_artist_poster(db, artist.id).await?;
    Ok(())
}

/// The artist's MusicBrainz id: from the tags, else an unambiguous search hit.
async fn artist_mbid(
    db: &Db,
    mb: &MusicBrainz,
    artist: &items::Item,
) -> anyhow::Result<Option<String>> {
    let ids = items::provider_ids(db, artist.id).await?;
    if let Some(id) = provider_id(&ids, "musicbrainz_artist") {
        // Validates the id and warms the cache for later use.
        if mb.artist(&id).await?.is_some() {
            items::set_match(db, artist.id, "matched", Some(1.0)).await?;
        }
        return Ok(Some(id));
    }
    let wanted = normalize_key(&artist.title);
    let hits = mb.search_artists(&artist.title).await?;
    let exact: Vec<_> = hits
        .iter()
        .filter(|a| a.score.unwrap_or(0) >= 90 && normalize_key(&a.name) == wanted)
        .collect();
    // Two artists with the same name ("Nirvana"): leave it to the tags.
    if let [only] = exact.as_slice() {
        metadata::set_provider_id(db, artist.id, "musicbrainz_artist", &only.id).await?;
        items::set_match(db, artist.id, "matched", Some(0.9)).await?;
        return Ok(Some(only.id.clone()));
    }
    items::set_match(db, artist.id, "failed", None).await?;
    Ok(None)
}

/// Without a picture of their own, an artist shows the cover of an album.
async fn fallback_artist_poster(db: &Db, artist_id: i64) -> anyhow::Result<()> {
    let images = metadata::images(db, artist_id).await?;
    if images
        .iter()
        .any(|i| i.kind == "poster" && i.source != "album")
    {
        metadata::replace_images(db, artist_id, "album", &[]).await?;
        return Ok(());
    }
    let mut cover = None;
    for album in items::children(db, artist_id).await? {
        if let Some(best) = metadata::images(db, album.id)
            .await?
            .into_iter()
            .find(|i| i.kind == "poster")
        {
            cover = Some(best);
            break;
        }
    }
    let fallback: Vec<NewImage> = cover
        .map(|c| NewImage {
            kind: "poster".to_owned(),
            path: c.path,
            url: c.url,
            score: 1.0,
            ..Default::default()
        })
        .into_iter()
        .collect();
    let current: Vec<(Option<String>, Option<String>)> = images
        .iter()
        .filter(|i| i.source == "album")
        .map(|i| (i.path.clone(), i.url.clone()))
        .collect();
    let wanted: Vec<(Option<String>, Option<String>)> = fallback
        .iter()
        .map(|i| (i.path.clone(), i.url.clone()))
        .collect();
    if current != wanted {
        metadata::replace_images(db, artist_id, "album", &fallback).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(json: &str) -> Release {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn release_scores_prefer_title_artist_and_track_count() {
        let right = release(
            r#"{"id":"1","title":"Planet Punk","score":100,"track-count":12,
                "artist-credit":[{"name":"Die Ärzte","artist":{"id":"a","name":"Die Ärzte"}}]}"#,
        );
        let wrong_artist = release(
            r#"{"id":"2","title":"Planet Punk","score":90,"track-count":12,
                "artist-credit":[{"name":"Punk Tribute Band","artist":{"id":"b","name":"x"}}]}"#,
        );
        let live = release(
            r#"{"id":"3","title":"Planet Punk Live","score":80,"track-count":20,
                "artist-credit":[{"name":"Die Ärzte","artist":{"id":"a","name":"Die Ärzte"}}]}"#,
        );
        let score = |r: &Release| release_score(r, "Planet Punk", "Die Ärzte", 12, false);
        assert!(score(&right) >= MATCH_THRESHOLD, "{}", score(&right));
        assert!(
            score(&wrong_artist) < MATCH_THRESHOLD,
            "{}",
            score(&wrong_artist)
        );
        assert!(score(&live) < MATCH_THRESHOLD, "{}", score(&live));
    }
}
