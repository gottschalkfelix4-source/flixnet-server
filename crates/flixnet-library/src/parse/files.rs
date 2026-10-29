//! File types and roles: main video, extra, sample, subtitle.

use std::path::{Component, Path};
use std::sync::LazyLock;

use regex::Regex;
use serde::Serialize;

const VIDEO_EXTENSIONS: &[&str] = &[
    "mkv", "mp4", "m4v", "avi", "mov", "wmv", "ts", "m2ts", "mts", "webm", "mpg", "mpeg", "vob",
    "flv", "ogv", "3gp", "divx", "iso", "strm",
];
const SUBTITLE_EXTENSIONS: &[&str] = &["srt", "ass", "ssa", "vtt", "sub", "idx", "sup", "smi"];

pub fn is_video_file(path: &Path) -> bool {
    has_extension(path, VIDEO_EXTENSIONS)
}

pub fn is_subtitle_file(path: &Path) -> bool {
    has_extension(path, SUBTITLE_EXTENSIONS)
}

fn has_extension(path: &Path, list: &[&str]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| list.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtraKind {
    Trailer,
    BehindTheScenes,
    DeletedScene,
    Featurette,
    Interview,
    Scene,
    Short,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "role", content = "kind")]
pub enum FileRole {
    /// The movie or episode itself.
    Main,
    /// Bonus material attached to a movie/series.
    Extra(ExtraKind),
    /// Release samples: never imported.
    Sample,
    /// Not a video (or a hidden/system file).
    Ignored,
}

fn extra_folder(name: &str) -> Option<ExtraKind> {
    Some(match name.to_ascii_lowercase().as_str() {
        "trailers" | "trailer" => ExtraKind::Trailer,
        "behind the scenes" | "behindthescenes" => ExtraKind::BehindTheScenes,
        "deleted scenes" | "deleted" => ExtraKind::DeletedScene,
        "featurettes" | "featurette" => ExtraKind::Featurette,
        "interviews" => ExtraKind::Interview,
        "scenes" => ExtraKind::Scene,
        "shorts" => ExtraKind::Short,
        "extras" | "other" | "others" | "bonus" | "specials features" | "special features" => {
            ExtraKind::Other
        }
        _ => return None,
    })
}

/// Folders that group files but say nothing about the title (`Extras`, `Sample`, `Season 1`).
pub fn is_container_folder(name: &str) -> bool {
    extra_folder(name).is_some()
        || name.eq_ignore_ascii_case("sample")
        || name.eq_ignore_ascii_case("samples")
        || super::episode::season_folder(name).is_some()
}

static EXTRA_SUFFIX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)-(trailer|behindthescenes|deleted|featurette|interview|scene|short|other)$")
        .expect("valid regex")
});
static SAMPLE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(^|[ ._-])sample([ ._-]|$)").expect("valid regex"));

/// Decides what a library-relative file is.
pub fn classify(relative: &Path) -> FileRole {
    let name = relative
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    if name.starts_with('.') || !is_video_file(relative) {
        return FileRole::Ignored;
    }
    let folders: Vec<String> = relative
        .parent()
        .into_iter()
        .flat_map(Path::components)
        .filter_map(|c| match c {
            Component::Normal(n) => Some(n.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    if folders
        .iter()
        .any(|f| f.starts_with('.') || f.eq_ignore_ascii_case("@eaDir"))
    {
        return FileRole::Ignored;
    }
    if folders
        .iter()
        .any(|f| f.eq_ignore_ascii_case("sample") || f.eq_ignore_ascii_case("samples"))
    {
        return FileRole::Sample;
    }
    if let Some(kind) = folders.iter().rev().find_map(|f| extra_folder(f)) {
        return FileRole::Extra(kind);
    }
    let stem = relative
        .file_stem()
        .map(|s| s.to_string_lossy())
        .unwrap_or_default();
    if let Some(caps) = EXTRA_SUFFIX.captures(&stem) {
        return FileRole::Extra(match caps[1].to_ascii_lowercase().as_str() {
            "trailer" => ExtraKind::Trailer,
            "behindthescenes" => ExtraKind::BehindTheScenes,
            "deleted" => ExtraKind::DeletedScene,
            "featurette" => ExtraKind::Featurette,
            "interview" => ExtraKind::Interview,
            "scene" => ExtraKind::Scene,
            "short" => ExtraKind::Short,
            _ => ExtraKind::Other,
        });
    }
    if SAMPLE.is_match(&stem) {
        return FileRole::Sample;
    }
    FileRole::Main
}

static PART: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:^|[ ._-])(?:cd|dvd|disc|disk|part|pt)[ ._-]?(\d{1,2})(?:[ ._-]|$)")
        .expect("valid regex")
});

/// Multi-file movies: `Movie cd1`, `Movie - Part 2`, `Movie.Disc.3`.
pub fn part_number(stem: &str) -> Option<u16> {
    PART.captures(stem).and_then(|caps| caps[1].parse().ok())
}

/// Language and flags of a sidecar subtitle relative to its video:
/// `Movie.de.forced.srt` → (Some("deu"), forced = true, sdh = false).
pub fn subtitle_info(video_stem: &str, subtitle: &Path) -> Option<(Option<String>, bool, bool)> {
    let stem = subtitle.file_stem()?.to_string_lossy();
    let rest = stem.strip_prefix(video_stem)?;
    let mut language = None;
    let (mut forced, mut sdh) = (false, false);
    for part in rest.split(['.', '_', ' ', '-']).filter(|p| !p.is_empty()) {
        match part.to_ascii_lowercase().as_str() {
            "forced" | "foreign" => forced = true,
            "sdh" | "cc" | "hi" => sdh = true,
            "default" => {}
            code => {
                if language.is_none() {
                    language = language_code(code).map(str::to_owned);
                }
            }
        }
    }
    Some((language, forced, sdh))
}

/// Maps 2-letter, 3-letter and common names to ISO 639-2/B-ish codes used by ffprobe.
pub fn language_code(code: &str) -> Option<&'static str> {
    Some(match code {
        "de" | "ger" | "deu" | "german" | "deutsch" => "deu",
        "en" | "eng" | "english" => "eng",
        "fr" | "fre" | "fra" | "french" => "fra",
        "es" | "spa" | "spanish" => "spa",
        "it" | "ita" | "italian" => "ita",
        "nl" | "dut" | "nld" | "dutch" => "nld",
        "pl" | "pol" | "polish" => "pol",
        "pt" | "por" | "portuguese" => "por",
        "ru" | "rus" | "russian" => "rus",
        "tr" | "tur" | "turkish" => "tur",
        "ja" | "jpn" | "japanese" => "jpn",
        "ko" | "kor" | "korean" => "kor",
        "zh" | "chi" | "zho" | "chinese" => "zho",
        "sv" | "swe" | "swedish" => "swe",
        "da" | "dan" | "danish" => "dan",
        "no" | "nor" | "norwegian" => "nor",
        "fi" | "fin" | "finnish" => "fin",
        "cs" | "cze" | "ces" | "czech" => "ces",
        "hu" | "hun" | "hungarian" => "hun",
        "ar" | "ara" | "arabic" => "ara",
        _ => return None,
    })
}
