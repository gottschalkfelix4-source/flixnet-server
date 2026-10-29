//! File and folder name parsing (a guessit-style parser for movies and TV).
//!
//! Inputs are paths *relative to the library root*, so folder conventions
//! (`Movie (2020)/`, `Show (2019)/Season 01/`) can be used together with the file name.
//! Everything here is pure string work: no I/O, no allocation-heavy machinery,
//! and deterministic results that are pinned by a large table of test cases.

mod episode;
mod files;
mod hints;
mod tags;
mod title;

use std::path::{Component, Path};

use serde::Serialize;

pub use episode::AirDate;
pub use files::{ExtraKind, FileRole, classify, is_subtitle_file, is_video_file, subtitle_info};
pub use hints::ProviderHints;
pub use tags::{ReleaseTags, Source};

/// A movie file name understood as title, year and release details.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ParsedMovie {
    pub title: String,
    pub year: Option<u16>,
    /// e.g. "Director's Cut", from `{edition-…}` or edition tags.
    pub edition: Option<String>,
    /// Multi-file movies: `cd1`, `part2`, `disc 3`.
    pub part: Option<u16>,
    pub ids: ProviderHints,
    pub tags: ReleaseTags,
}

/// An episode file understood as series, season and episode numbers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ParsedEpisode {
    pub series_title: String,
    pub series_year: Option<u16>,
    /// `None` when only an absolute number or air date is known.
    pub season: Option<u16>,
    /// One entry per episode; double episodes (`S01E01-E02`) list both.
    pub episodes: Vec<u16>,
    /// Absolute numbering (anime: `Show - 105`).
    pub absolute: Option<u16>,
    /// Daily shows: `Show 2024-03-15`.
    pub air_date: Option<AirDate>,
    pub episode_title: Option<String>,
    pub ids: ProviderHints,
    pub tags: ReleaseTags,
}

/// Parses a movie from its library-relative path (`Folder/File.ext` or `File.ext`).
///
/// A `Title (Year)` folder is preferred for title/year because it is usually curated;
/// the file name still contributes release tags, edition and part.
pub fn parse_movie(relative: &Path) -> ParsedMovie {
    let stem = file_stem(relative);
    let folders = folders(relative);

    let mut ids = ProviderHints::default();
    for folder in &folders {
        ids.merge(hints::extract(folder).0);
    }
    let (file_ids, stem_clean) = hints::extract(&stem);
    ids.merge(file_ids);

    let (edition_hint, stem_clean) = title::extract_edition(&stem_clean);
    let from_file = title::split_title(&stem_clean);
    let tags = tags::parse(&from_file.rest);
    let part = files::part_number(&stem_clean);

    // The innermost folder that looks like "Title (Year)" wins over the file name.
    let from_folder = folders
        .iter()
        .rev()
        .filter(|f| !files::is_container_folder(f))
        .map(|f| title::split_title(&hints::extract(f).1))
        .find(|parsed| parsed.year.is_some() && !parsed.title.is_empty());

    // "Alpha Remastered (2020)/Alpha.mkv": the folder may carry the edition.
    let folder_edition = from_folder
        .as_ref()
        .and_then(|folder| tags::parse(&folder.rest).edition);
    let (title, year) = match from_folder {
        Some(folder) => (folder.title, folder.year),
        None => (from_file.title, from_file.year),
    };

    ParsedMovie {
        title,
        year,
        edition: edition_hint
            .or_else(|| tags.edition.clone())
            .or(folder_edition),
        part,
        ids,
        tags,
    }
}

/// Parses an episode from its library-relative path, e.g.
/// `Show (2019)/Season 01/Show.S01E02.Title.1080p.mkv`.
pub fn parse_episode(relative: &Path) -> ParsedEpisode {
    let stem = file_stem(relative);
    let folders = folders(relative);

    // Series = first folder under the library root (if any).
    let series_folder = folders.first().filter(|f| !files::is_container_folder(f));
    let mut ids = ProviderHints::default();
    let series_from_folder = series_folder.map(|f| {
        let (folder_ids, clean) = hints::extract(f);
        ids.merge(folder_ids);
        title::split_title(&clean)
    });
    let season_from_folder = folders.iter().rev().find_map(|f| episode::season_folder(f));

    let (file_ids, stem_clean) = hints::extract(&stem);
    ids.merge(file_ids);
    let normalized = title::normalize(&stem_clean);
    let found = episode::find(&normalized, season_from_folder.is_some());

    let (series_title, series_year) = match series_from_folder {
        Some(folder) if !folder.title.is_empty() => (folder.title, folder.year),
        _ => {
            let prefix = found
                .as_ref()
                .map_or(normalized.as_str(), |f| &normalized[..f.start]);
            let parsed = title::split_title(prefix);
            (parsed.title, parsed.year)
        }
    };

    let mut parsed = ParsedEpisode {
        series_title,
        series_year,
        season: season_from_folder,
        ids,
        ..Default::default()
    };

    match found {
        Some(found) => {
            if found.season.is_some() {
                parsed.season = found.season;
            }
            parsed.episodes = found.episodes;
            parsed.absolute = found.absolute;
            parsed.air_date = found.air_date;
            let after = &normalized[found.end..];
            let (episode_title, tag_text) = title::title_before_tags(after);
            parsed.episode_title = episode_title;
            parsed.tags = tags::parse(tag_text);
        }
        None => {
            parsed.tags = tags::parse(&normalized);
        }
    }
    parsed
}

fn file_stem(relative: &Path) -> String {
    relative
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Folder names between the library root and the file, outermost first.
fn folders(relative: &Path) -> Vec<String> {
    let parent = relative.parent().unwrap_or(Path::new(""));
    parent
        .components()
        .filter_map(|c| match c {
            Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect()
}
