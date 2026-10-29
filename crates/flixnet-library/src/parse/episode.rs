//! Season/episode markers in normalized names.

use std::sync::LazyLock;

use regex::Regex;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct AirDate {
    pub year: u16,
    pub month: u8,
    pub day: u8,
}

/// An episode marker and where it sits in the normalized name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub season: Option<u16>,
    pub episodes: Vec<u16>,
    pub absolute: Option<u16>,
    pub air_date: Option<AirDate>,
    /// Byte range of the marker in the normalized name.
    pub start: usize,
    pub end: usize,
}

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("valid regex")
}

// S01E02, S01E02E03, S01E02-E03, S01E02-03, S01 E02, s1e2
static SXXEYY: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)\bS(\d{1,4})[ -]?E(\d{1,4})((?:[ -]?-?[ -]?E?\d{1,4}\b)*)"));
// 1x02, 1x02-03, 1x02x03
static NXNN: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)\b(\d{1,2})x(\d{2,3})((?:[-x]\d{2,3})*)\b"));
// Staffel 1 Folge 2 / Season 1 Episode 2 / Staffel 01 Episode 02
static WORDS: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?i)\b(?:staffel|season|series)[ -]?(\d{1,3})[ -]*(?:folge|episode|ep|teil)[ -]?(\d{1,4})\b",
    )
});
// 2024-03-15 / 2024 03 15
static DATE: LazyLock<Regex> = LazyLock::new(|| re(r"\b((?:19|20)\d\d)[ -](\d{2})[ -](\d{2})\b"));
// Episode only (season from folder): E05, Ep 5, Episode 5, Folge 5
static EPISODE_ONLY: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)\b(?:E|Ep|Episode|Folge|Teil)[ -]?(\d{1,4})\b"));
// Anime absolute: "Show - 105", "Show - 105v2"
static ABSOLUTE: LazyLock<Regex> = LazyLock::new(|| re(r"(?:^| )- (\d{1,4})(?:v\d)?(?: |$)"));
// Leading number inside a season folder: "02 - Title", "02 Title"
static LEADING: LazyLock<Regex> = LazyLock::new(|| re(r"^(\d{1,3})(?: - | |$)"));

static SEASON_FOLDER: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)^(?:season|staffel|series|serie|s)[ ._-]*(\d{1,4})$"));

/// `Season 01`, `Staffel 2`, `S03`, `Specials` (season 0).
pub fn season_folder(name: &str) -> Option<u16> {
    let name = name.trim();
    if name.eq_ignore_ascii_case("specials") || name.eq_ignore_ascii_case("special") {
        return Some(0);
    }
    SEASON_FOLDER
        .captures(name)
        .and_then(|caps| caps[1].parse().ok())
}

fn numbers(text: &str) -> impl Iterator<Item = u16> + '_ {
    text.split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse().ok())
}

/// Episode list for "first + continuation": `-03` is a range, `E03`/`x03` an addition.
fn episode_list(first: u16, continuation: &str) -> Vec<u16> {
    let mut episodes = vec![first];
    let extra: Vec<u16> = numbers(continuation).collect();
    let is_range = continuation.contains('-') && extra.len() == 1;
    match (is_range, extra.as_slice()) {
        (true, [last]) if *last > first && *last - first <= 30 => {
            episodes.extend(first + 1..=*last);
        }
        _ => episodes.extend(extra.into_iter().filter(|&e| e > first)),
    }
    episodes.dedup();
    episodes
}

/// Finds the episode marker. `has_season_folder` enables the looser patterns that are
/// only safe when the folder already tells us we are inside a season.
pub fn find(normalized: &str, has_season_folder: bool) -> Option<Found> {
    if let Some(caps) = SXXEYY.captures(normalized) {
        let m = caps.get(0).expect("whole match");
        let first: u16 = caps[2].parse().ok()?;
        return Some(Found {
            season: caps[1].parse().ok(),
            episodes: episode_list(first, &caps[3]),
            absolute: None,
            air_date: None,
            start: m.start(),
            end: m.end(),
        });
    }
    if let Some(caps) = WORDS.captures(normalized) {
        let m = caps.get(0).expect("whole match");
        return Some(Found {
            season: caps[1].parse().ok(),
            episodes: vec![caps[2].parse().ok()?],
            absolute: None,
            air_date: None,
            start: m.start(),
            end: m.end(),
        });
    }
    if let Some(caps) = NXNN.captures(normalized) {
        let m = caps.get(0).expect("whole match");
        // Avoid resolutions like "1920x1080" (excluded by digit counts) and years.
        let first: u16 = caps[2].parse().ok()?;
        return Some(Found {
            season: caps[1].parse().ok(),
            episodes: episode_list(first, &caps[3]),
            absolute: None,
            air_date: None,
            start: m.start(),
            end: m.end(),
        });
    }
    if let Some(caps) = DATE.captures(normalized) {
        let m = caps.get(0).expect("whole match");
        let (year, month, day): (u16, u8, u8) = (
            caps[1].parse().ok()?,
            caps[2].parse().ok()?,
            caps[3].parse().ok()?,
        );
        if (1..=12).contains(&month) && (1..=31).contains(&day) {
            return Some(Found {
                season: None,
                episodes: Vec::new(),
                absolute: None,
                air_date: Some(AirDate { year, month, day }),
                start: m.start(),
                end: m.end(),
            });
        }
    }
    if has_season_folder {
        if let Some(caps) = EPISODE_ONLY.captures(normalized) {
            let m = caps.get(0).expect("whole match");
            return Some(Found {
                season: None,
                episodes: vec![caps[1].parse().ok()?],
                absolute: None,
                air_date: None,
                start: m.start(),
                end: m.end(),
            });
        }
        if let Some(caps) = LEADING.captures(normalized) {
            let m = caps.get(1).expect("number");
            return Some(Found {
                season: None,
                episodes: vec![caps[1].parse().ok()?],
                absolute: None,
                air_date: None,
                start: m.start(),
                end: caps.get(0).expect("whole match").end(),
            });
        }
    }
    if let Some(caps) = ABSOLUTE.captures(normalized) {
        let number: u16 = caps[1].parse().ok()?;
        let m = caps.get(0).expect("whole match");
        // A 4-digit "- 2019" is a year, not an episode.
        if !(1890..=2100).contains(&number) || caps[1].len() < 4 {
            return Some(Found {
                season: None,
                episodes: Vec::new(),
                absolute: Some(number),
                air_date: None,
                start: m.start(),
                end: m.end(),
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_and_multi_episodes() {
        assert_eq!(episode_list(1, "-E02"), vec![1, 2]);
        assert_eq!(episode_list(1, "-03"), vec![1, 2, 3]);
        assert_eq!(episode_list(1, "E02E03"), vec![1, 2, 3]);
        assert_eq!(episode_list(5, ""), vec![5]);
    }

    #[test]
    fn season_folders() {
        assert_eq!(season_folder("Season 01"), Some(1));
        assert_eq!(season_folder("Staffel 3"), Some(3));
        assert_eq!(season_folder("S02"), Some(2));
        assert_eq!(season_folder("Specials"), Some(0));
        assert_eq!(season_folder("Extras"), None);
    }
}
