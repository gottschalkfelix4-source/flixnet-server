//! Turning parsed names into item identities (content keys) and titles.

use std::path::Path;

use crate::parse::{ParsedEpisode, ParsedMovie, ProviderHints, parse_episode, parse_movie};

/// Lower-case alphanumerics with single spaces: the identity part of a title.
pub fn normalize_key(title: &str) -> String {
    let mut out = String::with_capacity(title.len());
    for word in title
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
    {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&word.to_lowercase());
    }
    out
}

/// Sort title without leading articles ("The Matrix" → "matrix", "Der Pate" → "pate").
pub fn sort_title(title: &str) -> String {
    let lower = title.trim().to_lowercase();
    for article in ["the ", "a ", "an ", "der ", "die ", "das ", "ein ", "eine "] {
        if let Some(rest) = lower.strip_prefix(article)
            && !rest.is_empty()
        {
            return rest.trim().to_owned();
        }
    }
    lower
}

/// One level of the item hierarchy to create (series → season → episode, or movie).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemSpec {
    pub kind: &'static str,
    pub key: String,
    pub title: String,
    pub year: Option<i64>,
    pub index_number: Option<i64>,
    pub index_end: Option<i64>,
    pub parent_index_number: Option<i64>,
    pub premiere_date: Option<String>,
    pub provider_ids: Vec<(&'static str, String)>,
}

impl ItemSpec {
    fn new(kind: &'static str, key: String, title: String) -> Self {
        Self {
            kind,
            key,
            title,
            year: None,
            index_number: None,
            index_end: None,
            parent_index_number: None,
            premiere_date: None,
            provider_ids: Vec::new(),
        }
    }
}

fn provider_ids(ids: &ProviderHints) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    if let Some(id) = ids.tmdb {
        out.push(("tmdb", id.to_string()));
    }
    if let Some(id) = ids.tvdb {
        out.push(("tvdb", id.to_string()));
    }
    if let Some(id) = &ids.imdb {
        out.push(("imdb", id.clone()));
    }
    out
}

fn stem(relative: &Path) -> String {
    relative
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The movie item a file belongs to.
pub fn movie(relative: &Path) -> (ItemSpec, ParsedMovie) {
    let parsed = parse_movie(relative);
    let title = if parsed.title.is_empty() {
        stem(relative)
    } else {
        parsed.title.clone()
    };
    let key = if let Some(id) = parsed.ids.tmdb {
        format!("tmdb:movie:{id}")
    } else if let Some(id) = &parsed.ids.imdb {
        format!("imdb:{id}")
    } else {
        format!(
            "local:movie:{}:{}",
            normalize_key(&title),
            parsed.year.unwrap_or(0)
        )
    };
    let mut spec = ItemSpec::new("movie", key, title);
    spec.year = parsed.year.map(i64::from);
    spec.provider_ids = provider_ids(&parsed.ids);
    (spec, parsed)
}

/// The series item a file belongs to (used for episodes and series extras).
pub fn series(parsed: &ParsedEpisode, relative: &Path) -> ItemSpec {
    let title = if parsed.series_title.is_empty() {
        stem(relative)
    } else {
        parsed.series_title.clone()
    };
    let key = if let Some(id) = parsed.ids.tvdb {
        format!("tvdb:series:{id}")
    } else if let Some(id) = parsed.ids.tmdb {
        format!("tmdb:series:{id}")
    } else if let Some(id) = &parsed.ids.imdb {
        format!("imdb:{id}")
    } else {
        format!(
            "local:series:{}:{}",
            normalize_key(&title),
            parsed.series_year.unwrap_or(0)
        )
    };
    let mut spec = ItemSpec::new("series", key, title);
    spec.year = parsed.series_year.map(i64::from);
    spec.provider_ids = provider_ids(&parsed.ids);
    spec
}

/// The full chain for an episode file: series, optional season, episode.
pub fn episode(relative: &Path) -> (Vec<ItemSpec>, ParsedEpisode) {
    let parsed = parse_episode(relative);
    let series = series(&parsed, relative);
    let mut chain = vec![series.clone()];

    // Daily shows group by year; absolute numbering hangs directly under the series.
    let season_number = parsed.season.or_else(|| parsed.air_date.map(|d| d.year));
    if let Some(season) = season_number {
        let title = if season == 0 {
            "Specials".to_owned()
        } else {
            format!("Staffel {season}")
        };
        let mut spec = ItemSpec::new("season", format!("{}:s{season}", series.key), title);
        spec.index_number = Some(i64::from(season));
        chain.push(spec);
    }

    let (key_suffix, title, index, end) = if let Some(&first) = parsed.episodes.first() {
        let last = *parsed.episodes.last().expect("non-empty");
        (
            format!("s{}e{first}", season_number.unwrap_or(0)),
            parsed
                .episode_title
                .clone()
                .unwrap_or_else(|| format!("Folge {first}")),
            Some(i64::from(first)),
            (last > first).then_some(i64::from(last)),
        )
    } else if let Some(date) = parsed.air_date {
        let iso = format!("{:04}-{:02}-{:02}", date.year, date.month, date.day);
        (
            format!("d{iso}"),
            parsed.episode_title.clone().unwrap_or_else(|| iso.clone()),
            None,
            None,
        )
    } else if let Some(absolute) = parsed.absolute {
        (
            format!("a{absolute}"),
            parsed
                .episode_title
                .clone()
                .unwrap_or_else(|| format!("Folge {absolute}")),
            Some(i64::from(absolute)),
            None,
        )
    } else {
        let name = stem(relative);
        (format!("f:{}", normalize_key(&name)), name, None, None)
    };

    let mut spec = ItemSpec::new("episode", format!("{}:{key_suffix}", series.key), title);
    spec.index_number = index;
    spec.index_end = end;
    spec.parent_index_number = season_number.map(i64::from);
    spec.premiere_date = parsed
        .air_date
        .map(|d| format!("{:04}-{:02}-{:02}", d.year, d.month, d.day));
    chain.push(spec);
    (chain, parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_prefer_provider_ids() {
        let (m, _) = movie(Path::new("Inception (2010) {tmdb-27205}/Inception.mkv"));
        assert_eq!(m.key, "tmdb:movie:27205");
        let (m, _) = movie(Path::new("Der Pate (1972)/Der.Pate.1972.German.mkv"));
        assert_eq!(m.key, "local:movie:der pate:1972");
        assert_eq!(sort_title(&m.title), "pate");
    }

    #[test]
    fn episode_chain() {
        let (chain, _) = episode(Path::new(
            "Dark (2017) [tvdbid-334824]/Staffel 1/Dark.S01E02E03.mkv",
        ));
        let kinds: Vec<_> = chain.iter().map(|s| s.kind).collect();
        assert_eq!(kinds, ["series", "season", "episode"]);
        assert_eq!(chain[0].key, "tvdb:series:334824");
        assert_eq!(chain[1].key, "tvdb:series:334824:s1");
        assert_eq!(chain[2].key, "tvdb:series:334824:s1e2");
        assert_eq!(
            (chain[2].index_number, chain[2].index_end),
            (Some(2), Some(3))
        );

        let (chain, _) = episode(Path::new("One Piece/One Piece - 1071.mkv"));
        assert_eq!(chain.len(), 2, "absolute numbering has no season");
        assert_eq!(chain[1].key, "local:series:one piece:0:a1071");
    }
}
