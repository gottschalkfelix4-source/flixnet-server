//! Where a recording is written and what the library learns about it (pure
//! functions).
//!
//! Recordings land in two folders below the recordings root, laid out the way the
//! library scanner expects them:
//!
//! * `Serien/<Title>/Season 01/<Title> - S01E05 - <Episode title>.ts`, or — without
//!   episode numbers — `Serien/<Title>/Season 2026/<Title> - 2026-10-30 - <Episode
//!   title>.ts` (air date).
//! * `Filme/<Title> (<Year>)/<Title> (<Year>).ts`.
//!
//! A recording interrupted by a server restart continues in a new part file
//! (`… - part2.ts`; the first part is renamed to `… - part1.ts`). Next to each
//! recording an NFO carries the guide's texts and age rating, so the library shows
//! them (and parental controls apply) even without a metadata provider.

use std::path::{Path, PathBuf};

use crate::stream::civil_from_days;

/// Folder of series episodes below the recordings root.
pub const SERIES_DIR: &str = "Serien";
/// Folder of films below the recordings root.
pub const MOVIES_DIR: &str = "Filme";

/// What is known about a recording when naming it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RecordingMeta {
    pub title: String,
    pub subtitle: Option<String>,
    pub description: Option<String>,
    pub categories: Vec<String>,
    pub season: Option<i64>,
    pub episode: Option<i64>,
    pub year: Option<i64>,
    pub rating: Option<String>,
    pub start_ms: i64,
    /// Recorded by a series rule.
    pub series_rule: bool,
    /// Local time = UTC + this many minutes (air dates).
    pub utc_offset_min: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    Episode,
    Movie,
}

fn is_film(categories: &[String]) -> bool {
    categories.iter().any(|c| {
        let c = c.to_lowercase();
        c.contains("film") || c.contains("movie")
    })
}

/// Episode of a series, or a film.
pub fn layout(meta: &RecordingMeta) -> Layout {
    let numbered = meta.season.is_some() || meta.episode.is_some();
    if numbered || meta.series_rule {
        Layout::Episode
    } else if is_film(&meta.categories) {
        Layout::Movie
    } else if meta
        .subtitle
        .as_deref()
        .is_some_and(|s| !s.trim().is_empty())
    {
        Layout::Episode
    } else {
        Layout::Movie
    }
}

/// A file or folder name: no path separators, reserved or control characters, no
/// trailing dots or spaces, not too long.
pub fn sanitize(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|c| !matches!(c, '*' | '?' | '"' | '<' | '>' | '|'))
        .map(|c| match c {
            '/' | '\\' | ':' => ' ',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect();
    let mut out = cleaned
        .split_whitespace()
        .filter(|word| !word.chars().all(|c| c == '.'))
        .collect::<Vec<_>>()
        .join(" ");
    if out.chars().count() > 120 {
        out = out.chars().take(120).collect();
    }
    let out = out.trim_end_matches(['.', ' ']).trim_start_matches('.');
    let out = if out.is_empty() { "Aufnahme" } else { out };
    // Windows device names.
    let stem = out.split('.').next().unwrap_or(out).to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && stem.as_bytes()[3].is_ascii_digit());
    if reserved {
        format!("_{out}")
    } else {
        out.to_owned()
    }
}

/// `YYYY-MM-DD` of a time in local time.
pub fn local_date(ms: i64, utc_offset_min: i64) -> (i64, i64, i64) {
    let local = ms + utc_offset_min * 60_000;
    civil_from_days(local.div_euclid(86_400_000))
}

/// Path of the recording below the recordings root (first part, `.ts`).
pub fn relative_path(meta: &RecordingMeta) -> PathBuf {
    let title = sanitize(&meta.title);
    let subtitle = meta
        .subtitle
        .as_deref()
        .map(sanitize)
        .filter(|s| !s.is_empty() && *s != "Aufnahme" && s != &title);
    match layout(meta) {
        Layout::Episode => {
            let (season_folder, marker) = match (meta.season, meta.episode) {
                (Some(s), Some(e)) => (format!("Season {s:02}"), format!("S{s:02}E{e:02}")),
                (None, Some(e)) => ("Season 01".to_owned(), format!("S01E{e:02}")),
                _ => {
                    let (y, m, d) = local_date(meta.start_ms, meta.utc_offset_min);
                    (format!("Season {y}"), format!("{y:04}-{m:02}-{d:02}"))
                }
            };
            let mut file = format!("{title} - {marker}");
            if let Some(subtitle) = subtitle {
                file.push_str(" - ");
                file.push_str(&subtitle);
            }
            file.push_str(".ts");
            [SERIES_DIR, &title, &season_folder, &file].iter().collect()
        }
        Layout::Movie => {
            let year = meta
                .year
                .filter(|y| (1880..=2200).contains(y))
                .unwrap_or_else(|| local_date(meta.start_ms, meta.utc_offset_min).0);
            let name = format!("{title} ({year})");
            [MOVIES_DIR, &name, &format!("{name}.ts")].iter().collect()
        }
    }
}

/// `X.ts` → `X - part2.ts`.
pub fn part_path(first: &Path, part: u32) -> PathBuf {
    let stem = first
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    first.with_file_name(format!("{stem} - part{part}.ts"))
}

/// The file of a part: the first part keeps the plain name while it is the only one.
pub fn file_of_part(first: &Path, part: u32, parts: u32) -> PathBuf {
    if parts <= 1 {
        first.to_owned()
    } else {
        part_path(first, part)
    }
}

/// A path that does not exist yet: `X.ts`, `X (2).ts`, `X (3).ts`, …
pub fn unused(path: PathBuf, exists: impl Fn(&Path) -> bool) -> PathBuf {
    if !exists(&path) && !exists(&part_path(&path, 1)) {
        return path;
    }
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    (2..)
        .map(|n| path.with_file_name(format!("{stem} ({n}).ts")))
        .find(|p| !exists(p) && !exists(&part_path(p, 1)))
        .expect("an unused name")
}

fn xml(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .map(|c| match c {
            '&' => "&amp;".to_owned(),
            '<' => "&lt;".to_owned(),
            '>' => "&gt;".to_owned(),
            '"' => "&quot;".to_owned(),
            c => c.to_string(),
        })
        .collect()
}

/// The NFO written next to a recording (`<episodedetails>` or `<movie>`).
pub fn nfo(meta: &RecordingMeta, channel: &str) -> String {
    let (y, m, d) = local_date(meta.start_ms, meta.utc_offset_min);
    let aired = format!("{y:04}-{m:02}-{d:02}");
    let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    let tag = |out: &mut String, name: &str, value: &str| {
        out.push_str(&format!("  <{name}>{}</{name}>\n", xml(value)));
    };
    match layout(meta) {
        Layout::Episode => {
            out.push_str("<episodedetails>\n");
            tag(
                &mut out,
                "title",
                meta.subtitle.as_deref().unwrap_or(&meta.title),
            );
            tag(&mut out, "showtitle", &meta.title);
            if let Some(season) = meta.season {
                tag(&mut out, "season", &season.to_string());
            }
            if let Some(episode) = meta.episode {
                tag(&mut out, "episode", &episode.to_string());
            }
            tag(&mut out, "aired", &aired);
        }
        Layout::Movie => {
            out.push_str("<movie>\n");
            tag(&mut out, "title", &meta.title);
            let year = meta.year.unwrap_or(y);
            tag(&mut out, "year", &year.to_string());
            tag(&mut out, "premiered", &aired);
        }
    }
    if let Some(plot) = &meta.description {
        tag(&mut out, "plot", plot);
    }
    if let Some(rating) = &meta.rating {
        tag(&mut out, "mpaa", rating);
    }
    for genre in &meta.categories {
        tag(&mut out, "genre", genre);
    }
    tag(&mut out, "studio", channel);
    out.push_str(match layout(meta) {
        Layout::Episode => "</episodedetails>\n",
        Layout::Movie => "</movie>\n",
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-10-30 19:15 UTC (20:15 in Berlin).
    const START: i64 = 1_793_387_700_000;

    fn meta(title: &str) -> RecordingMeta {
        RecordingMeta {
            title: title.into(),
            start_ms: START,
            utc_offset_min: 60,
            ..RecordingMeta::default()
        }
    }

    fn path(p: PathBuf) -> String {
        p.to_string_lossy().replace('\\', "/")
    }

    #[test]
    fn episodes_get_series_season_and_sxxeyy() {
        let m = RecordingMeta {
            season: Some(3),
            episode: Some(7),
            subtitle: Some("Der Fall: Teil 1/2".into()),
            ..meta("Tatort")
        };
        assert_eq!(
            path(relative_path(&m)),
            "Serien/Tatort/Season 03/Tatort - S03E07 - Der Fall Teil 1 2.ts"
        );
        // Without numbers: by air date (local), in a season of the year.
        let daily = RecordingMeta {
            series_rule: true,
            ..meta("Tagesschau")
        };
        assert_eq!(
            path(relative_path(&daily)),
            "Serien/Tagesschau/Season 2026/Tagesschau - 2026-10-30.ts"
        );
        assert_eq!(layout(&daily), Layout::Episode);
    }

    #[test]
    fn films_get_title_and_year() {
        let film = RecordingMeta {
            categories: vec!["Spielfilm".into()],
            year: Some(1999),
            ..meta("Matrix")
        };
        assert_eq!(
            path(relative_path(&film)),
            "Filme/Matrix (1999)/Matrix (1999).ts"
        );
        // A one-off without episode details is filed as a film of the broadcast year.
        assert_eq!(
            path(relative_path(&meta("Konzert: Live"))),
            "Filme/Konzert Live (2026)/Konzert Live (2026).ts"
        );
        // An episode title makes it an episode.
        let episode = RecordingMeta {
            subtitle: Some("Folge eins".into()),
            ..meta("Doku")
        };
        assert_eq!(layout(&episode), Layout::Episode);
    }

    #[test]
    fn names_are_safe() {
        assert_eq!(sanitize("../../etc/passwd"), "etc passwd");
        assert_eq!(sanitize("Was?! Ein \"Test\"..."), "Was! Ein Test");
        assert_eq!(sanitize("CON"), "_CON");
        assert_eq!(sanitize("  \u{7}  "), "Aufnahme");
        assert_eq!(sanitize(&"x".repeat(300)).len(), 120);
        assert_eq!(sanitize("Café Größe"), "Café Größe");
    }

    #[test]
    fn parts_and_unused_names() {
        let first = PathBuf::from("Filme/A (2026)/A (2026).ts");
        assert_eq!(
            path(part_path(&first, 2)),
            "Filme/A (2026)/A (2026) - part2.ts"
        );
        assert_eq!(file_of_part(&first, 1, 1), first);
        assert_eq!(
            path(file_of_part(&first, 1, 2)),
            "Filme/A (2026)/A (2026) - part1.ts"
        );
        let taken = [
            "Filme/A (2026)/A (2026).ts",
            "Filme/A (2026)/A (2026) (2) - part1.ts",
        ];
        let found = unused(first, |p| taken.contains(&path(p.to_owned()).as_str()));
        assert_eq!(path(found), "Filme/A (2026)/A (2026) (3).ts");
    }

    #[test]
    fn nfo_carries_texts_and_rating() {
        let m = RecordingMeta {
            season: Some(1),
            episode: Some(2),
            subtitle: Some("Tom & Jerry <3".into()),
            description: Some("Eine Folge.".into()),
            rating: Some("FSK 12".into()),
            categories: vec!["Serie".into()],
            ..meta("Cartoon")
        };
        let nfo = nfo(&m, "Testkanal");
        assert!(nfo.contains("<episodedetails>"), "{nfo}");
        assert!(
            nfo.contains("<title>Tom &amp; Jerry &lt;3</title>"),
            "{nfo}"
        );
        assert!(nfo.contains("<showtitle>Cartoon</showtitle>"), "{nfo}");
        assert!(nfo.contains("<season>1</season>"), "{nfo}");
        assert!(nfo.contains("<aired>2026-10-30</aired>"), "{nfo}");
        assert!(nfo.contains("<mpaa>FSK 12</mpaa>"), "{nfo}");
        let film = nfo_of_film();
        assert!(
            film.starts_with("<?xml") && film.contains("<movie>"),
            "{film}"
        );
        assert!(film.contains("<year>2026</year>"), "{film}");
    }

    fn nfo_of_film() -> String {
        nfo(&meta("Film"), "Testkanal")
    }
}
