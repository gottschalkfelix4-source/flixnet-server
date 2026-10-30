//! Reading audio tags and turning them into the artist → album → track chain.
//!
//! Tags come from [lofty] (pure Rust, reads only the tag blocks): it is fast enough
//! to run inside the scan itself — one ffprobe process per file would make the first
//! scan of a large collection take minutes — and it exposes everything music needs
//! (album artist, disc/track numbers, MusicBrainz ids, ReplayGain/R128, embedded
//! pictures and lyrics) uniformly across FLAC, MP3, MP4/ALAC, Ogg/Opus and WAV.
//! ffprobe still runs afterwards as for every file (streams, exact duration).
//! Files lofty cannot read (WMA, broken tags) fall back to the folder layout
//! `Artist/Album/01 - Title.ext`.

use std::path::{Component, Path};
use std::sync::LazyLock;

use flixnet_db::music::AudioTags;
use lofty::config::{ParseOptions, ParsingMode};
use lofty::picture::PictureType;
use lofty::prelude::*;
use lofty::probe::Probe;
use lofty::tag::Tag;
use regex::Regex;

use crate::scan::{normalize_key, sort_title};

/// Audio files a music library imports.
pub const AUDIO_EXTENSIONS: &[&str] = &[
    "flac", "mp3", "m4a", "m4b", "mp4a", "aac", "alac", "ogg", "oga", "opus", "wav", "wma", "aif",
    "aiff", "ape", "wv", "mka",
];

/// Shown when the tags name no artist / album.
pub const UNKNOWN_ARTIST: &str = "Unbekannter Interpret";
pub const UNKNOWN_ALBUM: &str = "Unbekanntes Album";
/// Album artist of compilations without one.
pub const VARIOUS_ARTISTS: &str = "Verschiedene Interpreten";

/// Age rating given to tracks tagged as explicit (parental controls).
pub const EXPLICIT_AGE: i64 = 16;

pub fn is_audio_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| AUDIO_EXTENSIONS.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

fn clean(value: Option<&str>) -> Option<String> {
    value
        .map(|v| v.trim_matches(|c: char| c.is_whitespace() || c == '\0'))
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
}

/// Leading number of a value like `3`, `03/12` or `1 of 2`.
fn leading_number(value: &str) -> Option<i64> {
    let digits: String = value
        .trim()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok().filter(|n| *n > 0)
}

/// `-6.48 dB` → -6.48.
fn decibels(value: &str) -> Option<f64> {
    let number: String = value
        .trim()
        .chars()
        .take_while(|c| c.is_ascii_digit() || matches!(c, '-' | '+' | '.' | ','))
        .collect();
    number
        .replace(',', ".")
        .parse()
        .ok()
        .filter(|v: &f64| v.is_finite())
}

/// R128 gain (Opus, Q7.8 fixed point relative to -23 LUFS) on the ReplayGain scale
/// (reference -18 LUFS): +5 dB.
fn r128_to_replaygain(value: &str) -> Option<f64> {
    let q78: f64 = value.trim().parse().ok()?;
    Some(q78 / 256.0 + 5.0)
}

fn split_genres(values: impl Iterator<Item = String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for value in values {
        for part in value.split([';', '/', '|', '\0']) {
            let part = part.trim();
            // ID3v1-style "(17)" numbers carry no readable name.
            if part.is_empty() || part.starts_with('(') && part.ends_with(')') {
                continue;
            }
            if !out.iter().any(|g| g.eq_ignore_ascii_case(part)) {
                out.push(part.to_owned());
            }
        }
    }
    out
}

fn from_tag(tag: &Tag, out: &mut AudioTags) {
    let get = |key: ItemKey| clean(tag.get_string(key));
    out.title = clean(tag.title().as_deref());
    let artists: Vec<String> = tag
        .get_strings(ItemKey::TrackArtist)
        .filter_map(|a| clean(Some(a)))
        .collect();
    out.artist = (!artists.is_empty()).then(|| artists.join(", "));
    out.album_artist = get(ItemKey::AlbumArtist);
    out.album = clean(tag.album().as_deref());
    out.track_number = tag
        .track()
        .map(i64::from)
        .filter(|n| *n > 0)
        .or_else(|| get(ItemKey::TrackNumber).and_then(|v| leading_number(&v)));
    out.track_total = tag.track_total().map(i64::from).filter(|n| *n > 0);
    out.disc_number = tag
        .disk()
        .map(i64::from)
        .filter(|n| *n > 0)
        .or_else(|| get(ItemKey::DiscNumber).and_then(|v| leading_number(&v)));
    out.disc_total = tag.disk_total().map(i64::from).filter(|n| *n > 0);
    out.year = tag
        .date()
        .map(|d| i64::from(d.year))
        .or_else(|| get(ItemKey::OriginalReleaseDate).and_then(|v| leading_number(&v)))
        .filter(|y| (1000..=9999).contains(y));
    out.genres = split_genres(
        tag.get_strings(ItemKey::Genre)
            .filter_map(|g| clean(Some(g))),
    );
    out.compilation = get(ItemKey::FlagCompilation).is_some_and(|v| v == "1" || v == "true");
    // iTunes advisory: 1 = explicit, 2 = clean (4 = explicit in older files).
    out.explicit = get(ItemKey::ParentalAdvisory).is_some_and(|v| matches!(v.as_str(), "1" | "4"));
    out.track_gain_db = get(ItemKey::ReplayGainTrackGain)
        .and_then(|v| decibels(&v))
        .or_else(|| get(ItemKey::R128TrackGain).and_then(|v| r128_to_replaygain(&v)));
    out.track_peak = get(ItemKey::ReplayGainTrackPeak).and_then(|v| decibels(&v));
    out.album_gain_db = get(ItemKey::ReplayGainAlbumGain)
        .and_then(|v| decibels(&v))
        .or_else(|| get(ItemKey::R128AlbumGain).and_then(|v| r128_to_replaygain(&v)));
    out.album_peak = get(ItemKey::ReplayGainAlbumPeak).and_then(|v| decibels(&v));
    out.has_cover = !tag.pictures().is_empty();
    out.has_lyrics = get(ItemKey::Lyrics).is_some() || get(ItemKey::UnsyncLyrics).is_some();
    out.mb_recording_id = get(ItemKey::MusicBrainzRecordingId);
    out.mb_track_id = get(ItemKey::MusicBrainzTrackId);
    out.mb_release_id = get(ItemKey::MusicBrainzReleaseId);
    out.mb_release_group_id = get(ItemKey::MusicBrainzReleaseGroupId);
    out.mb_artist_id = get(ItemKey::MusicBrainzArtistId);
    out.mb_album_artist_id = get(ItemKey::MusicBrainzReleaseArtistId);
}

fn options() -> ParseOptions {
    ParseOptions::new().parsing_mode(ParsingMode::Relaxed)
}

/// Reads the tags of an audio file (blocking). Missing values are filled from the
/// folder layout, so the result is always usable.
pub fn read_tags(path: &Path, relative: &Path) -> AudioTags {
    let mut tags = AudioTags::default();
    let read = Probe::open(path)
        .and_then(|p| p.options(options()).guess_file_type().map_err(Into::into))
        .and_then(Probe::read);
    match read {
        Ok(file) => {
            if let Some(tag) = file.primary_tag().or_else(|| file.first_tag()) {
                from_tag(tag, &mut tags);
            }
            // Pictures and lyrics may sit in a secondary tag (e.g. ID3v2 next to APE).
            for tag in file.tags() {
                tags.has_cover |= !tag.pictures().is_empty();
                tags.has_lyrics |= tag.get_string(ItemKey::Lyrics).is_some()
                    || tag.get_string(ItemKey::UnsyncLyrics).is_some();
            }
            let duration = file.properties().duration().as_millis() as i64;
            tags.duration_ms = (duration > 0).then_some(duration);
        }
        Err(err) => {
            tracing::debug!(path = %path.display(), %err, "no readable tags; using folder names");
        }
    }
    fill_from_path(&mut tags, relative);
    tags
}

static DISC_FOLDER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:cd|disc|disk|dvd)\s*[-_ ]?\s*(\d{1,2})$").expect("valid regex")
});
/// `01 - Title`, `01. Title`, `1-02 Title` (disc-track), `A1 Title` is left alone.
static TRACK_PREFIX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:(\d)[-.](\d{2})|(\d{1,3}))(?:\s*[-._)]\s*|\s+)(.+)$").expect("valid regex")
});

fn folders(relative: &Path) -> Vec<String> {
    relative
        .parent()
        .into_iter()
        .flat_map(Path::components)
        .filter_map(|c| match c {
            Component::Normal(n) => Some(n.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect()
}

/// Fills what the tags left open from `Artist/Album[/CD1]/NN - Title.ext`.
fn fill_from_path(tags: &mut AudioTags, relative: &Path) {
    let mut dirs = folders(relative);
    // A disc folder ("CD2") belongs to the album folder above it.
    if let Some(last) = dirs.last()
        && let Some(caps) = DISC_FOLDER.captures(last)
    {
        if tags.disc_number.is_none() {
            tags.disc_number = caps[1].parse().ok();
        }
        dirs.pop();
    }
    let stem = relative
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (number, disc, rest) = match TRACK_PREFIX.captures(&stem) {
        Some(caps) => match (caps.get(1), caps.get(2), caps.get(3)) {
            (Some(d), Some(t), _) => (
                t.as_str().parse().ok(),
                d.as_str().parse().ok(),
                caps[4].to_owned(),
            ),
            (_, _, Some(t)) => (t.as_str().parse().ok(), None, caps[4].to_owned()),
            _ => (None, None, stem.clone()),
        },
        None => (None, None, stem.clone()),
    };
    if tags.track_number.is_none() {
        tags.track_number = number.filter(|n: &i64| *n > 0);
    }
    if tags.disc_number.is_none() {
        tags.disc_number = disc;
    }
    if tags.title.is_none() {
        // "Artist - Title" file names: keep the title part.
        let title = match rest.split_once(" - ") {
            Some((_, title)) if tags.artist.is_some() || dirs.len() >= 2 => title.to_owned(),
            _ => rest,
        };
        tags.title = clean(Some(&title)).or_else(|| clean(Some(&stem)));
    }
    if tags.album.is_none() {
        tags.album = dirs.last().cloned();
    }
    if tags.album_artist.is_none() && tags.artist.is_none() && dirs.len() >= 2 {
        tags.album_artist = dirs.get(dirs.len() - 2).cloned();
    }
}

/// The album artist an album is filed under.
pub fn album_artist_name(tags: &AudioTags) -> String {
    tags.album_artist
        .clone()
        .or_else(|| tags.compilation.then(|| VARIOUS_ARTISTS.to_owned()))
        .or_else(|| {
            // "A, B" from several artist values: file under the first.
            tags.artist
                .as_deref()
                .map(|a| a.split(", ").next().unwrap_or(a).to_owned())
        })
        .unwrap_or_else(|| UNKNOWN_ARTIST.to_owned())
}

/// One level of the music chain to create.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MusicSpec {
    pub kind: &'static str,
    pub key: String,
    pub title: String,
    pub year: Option<i64>,
    pub index_number: Option<i64>,
    pub parent_index_number: Option<i64>,
    pub provider_ids: Vec<(&'static str, String)>,
}

/// artist → album → track for one file. Keys: artists by name (a mix of tagged and
/// untagged files still lands on one artist), albums by MusicBrainz release when
/// tagged, else by artist + album name, tracks by MusicBrainz release track id, else
/// by album + disc + number + title. Several files with the same key (FLAC and MP3 of
/// one track) become versions of one track.
pub fn chain(tags: &AudioTags) -> [MusicSpec; 3] {
    let artist_name = album_artist_name(tags);
    let artist_key = format!("local:artist:{}", normalize_key(&artist_name));
    let mut artist_ids = Vec::new();
    let artist_mbid = tags.mb_album_artist_id.clone().or_else(|| {
        // Without an album artist the track artist is the album artist.
        (tags.album_artist.is_none() && !tags.compilation)
            .then(|| tags.mb_artist_id.clone())
            .flatten()
    });
    if let Some(id) = artist_mbid {
        artist_ids.push(("musicbrainz_artist", id));
    }

    let album_title = tags
        .album
        .clone()
        .unwrap_or_else(|| UNKNOWN_ALBUM.to_owned());
    let album_key = match &tags.mb_release_id {
        Some(id) => format!("mb:release:{id}"),
        None => format!(
            "local:album:{}:{}",
            normalize_key(&artist_name),
            normalize_key(&album_title)
        ),
    };
    let mut album_ids = Vec::new();
    if let Some(id) = &tags.mb_release_id {
        album_ids.push(("musicbrainz_album", id.clone()));
    }
    if let Some(id) = &tags.mb_release_group_id {
        album_ids.push(("musicbrainz_releasegroup", id.clone()));
    }

    let title = tags.title.clone().unwrap_or_else(|| album_title.clone());
    let track_key = match &tags.mb_track_id {
        Some(id) => format!("mb:track:{id}"),
        None => format!(
            "{album_key}:d{}:t{}:{}",
            tags.disc_number.unwrap_or(1),
            tags.track_number.unwrap_or(0),
            normalize_key(&title)
        ),
    };
    let mut track_ids = Vec::new();
    if let Some(id) = &tags.mb_track_id {
        track_ids.push(("musicbrainz_track", id.clone()));
    }
    if let Some(id) = &tags.mb_recording_id {
        track_ids.push(("musicbrainz_recording", id.clone()));
    }

    [
        MusicSpec {
            kind: "artist",
            key: artist_key,
            title: artist_name,
            year: None,
            index_number: None,
            parent_index_number: None,
            provider_ids: artist_ids,
        },
        MusicSpec {
            kind: "album",
            key: album_key,
            title: album_title,
            year: tags.year,
            index_number: None,
            parent_index_number: None,
            provider_ids: album_ids,
        },
        MusicSpec {
            kind: "track",
            key: track_key,
            title,
            year: tags.year,
            index_number: tags.track_number,
            parent_index_number: Some(tags.disc_number.unwrap_or(1)),
            provider_ids: track_ids,
        },
    ]
}

/// Sort title for music: like other titles (articles dropped), so "The Beatles"
/// files under B.
pub fn music_sort_title(title: &str) -> String {
    sort_title(title)
}

/// The best embedded picture of a file (front cover first), read on demand by the
/// image cache. Blocking.
pub fn embedded_picture(path: &Path) -> Option<Vec<u8>> {
    let file = Probe::open(path)
        .ok()?
        .options(options())
        .guess_file_type()
        .ok()?
        .read()
        .ok()?;
    let mut best: Option<(u8, Vec<u8>)> = None;
    for tag in file.tags() {
        for picture in tag.pictures() {
            let rank = match picture.pic_type() {
                PictureType::CoverFront => 0,
                PictureType::Other => 1,
                _ => 2,
            };
            if best.as_ref().is_none_or(|(r, _)| rank < *r) {
                best = Some((rank, picture.data().to_vec()));
            }
        }
    }
    best.map(|(_, data)| data)
}

/// Embedded lyrics text of a file, if any. Blocking.
pub fn embedded_lyrics(path: &Path) -> Option<String> {
    let file = Probe::open(path)
        .ok()?
        .options(options())
        .guess_file_type()
        .ok()?
        .read()
        .ok()?;
    file.tags().iter().find_map(|tag| {
        clean(tag.get_string(ItemKey::Lyrics))
            .or_else(|| clean(tag.get_string(ItemKey::UnsyncLyrics)))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_layout_fills_missing_tags() {
        let mut tags = AudioTags::default();
        fill_from_path(
            &mut tags,
            Path::new("Die Ärzte/Planet Punk (1995)/CD2/03 - Schunder-Song.mp3"),
        );
        assert_eq!(tags.album.as_deref(), Some("Planet Punk (1995)"));
        assert_eq!(tags.album_artist.as_deref(), Some("Die Ärzte"));
        assert_eq!(tags.disc_number, Some(2));
        assert_eq!(tags.track_number, Some(3));
        assert_eq!(tags.title.as_deref(), Some("Schunder-Song"));

        let mut tags = AudioTags::default();
        fill_from_path(&mut tags, Path::new("Album/1-07 Lied.flac"));
        assert_eq!((tags.disc_number, tags.track_number), (Some(1), Some(7)));
        assert_eq!(tags.title.as_deref(), Some("Lied"));
        assert_eq!(tags.album_artist, None, "one folder: album only");

        // Tags win over the folder names.
        let mut tags = AudioTags {
            title: Some("Echt".into()),
            track_number: Some(9),
            album: Some("Richtig".into()),
            ..Default::default()
        };
        fill_from_path(&mut tags, Path::new("X/Y/01 - Falsch.mp3"));
        assert_eq!(tags.title.as_deref(), Some("Echt"));
        assert_eq!(tags.track_number, Some(9));
        assert_eq!(tags.album.as_deref(), Some("Richtig"));
    }

    #[test]
    fn gains_and_numbers_parse_leniently() {
        assert_eq!(decibels("-6.48 dB"), Some(-6.48));
        assert_eq!(decibels("+1,5 dB"), Some(1.5));
        assert_eq!(decibels("0.988547"), Some(0.988547));
        assert_eq!(decibels("n/a"), None);
        assert_eq!(r128_to_replaygain("-2560"), Some(-5.0));
        assert_eq!(leading_number("03/12"), Some(3));
        assert_eq!(leading_number("0"), None);
        assert_eq!(
            split_genres(
                ["Rock; Pop".to_owned(), "rock".to_owned(), "(17)".to_owned()].into_iter()
            ),
            ["Rock", "Pop"]
        );
    }

    #[test]
    fn chains_group_compilations_and_versions() {
        let base = AudioTags {
            title: Some("Lied".into()),
            artist: Some("Anna".into()),
            album: Some("Sommer".into()),
            track_number: Some(2),
            ..Default::default()
        };
        let [artist, album, track] = chain(&base);
        assert_eq!(artist.title, "Anna");
        assert_eq!(artist.key, "local:artist:anna");
        assert_eq!(album.key, "local:album:anna:sommer");
        assert_eq!(track.key, "local:album:anna:sommer:d1:t2:lied");
        assert_eq!(track.parent_index_number, Some(1));

        // A compilation without album artist goes to "Verschiedene Interpreten".
        let compilation = AudioTags {
            compilation: true,
            ..base.clone()
        };
        assert_eq!(chain(&compilation)[0].title, VARIOUS_ARTISTS);

        // MusicBrainz ids make the keys and become provider ids.
        let tagged = AudioTags {
            mb_release_id: Some("rel-1".into()),
            mb_track_id: Some("trk-1".into()),
            mb_artist_id: Some("art-1".into()),
            ..base
        };
        let [artist, album, track] = chain(&tagged);
        assert_eq!(
            artist.provider_ids,
            [("musicbrainz_artist", "art-1".to_owned())]
        );
        assert_eq!(album.key, "mb:release:rel-1");
        assert_eq!(track.key, "mb:track:trk-1");
    }
}
