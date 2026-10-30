//! Finds the Flixnet item for a movie or episode another service knows: by provider
//! ids (TMDB/IMDb/TVDB) first, then by title + year, episodes by series + season +
//! episode number.

use std::collections::HashMap;

use flixnet_db::Db;
use flixnet_db::history::{self, IndexItem};
use serde::Serialize;
use utoipa::ToSchema;

/// Provider ids as other services report them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExternalIds {
    pub tmdb: Option<String>,
    pub imdb: Option<String>,
    pub tvdb: Option<String>,
}

impl ExternalIds {
    fn pairs(&self) -> impl Iterator<Item = (&'static str, &str)> {
        [
            ("tmdb", self.tmdb.as_deref()),
            ("imdb", self.imdb.as_deref()),
            ("tvdb", self.tvdb.as_deref()),
        ]
        .into_iter()
        .filter_map(|(p, v)| v.map(|v| (p, v)))
        .filter(|(_, v)| !v.is_empty() && *v != "0")
    }

    pub fn is_empty(&self) -> bool {
        self.pairs().next().is_none()
    }

    /// Reads `tmdb`/`imdb`/`tvdb` pairs (any case) from an id list.
    pub fn from_pairs<'a>(pairs: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
        let mut ids = Self::default();
        for (provider, value) in pairs {
            let value = value.trim().to_owned();
            match provider.to_ascii_lowercase().as_str() {
                "tmdb" | "themoviedb" => ids.tmdb = Some(value),
                "imdb" => ids.imdb = Some(value),
                "tvdb" | "thetvdb" => ids.tvdb = Some(value),
                _ => {}
            }
        }
        ids
    }
}

/// How an item was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MatchMethod {
    ProviderId,
    TitleYear,
    Episode,
}

/// Lowercase letters and digits only ("Der Herr der Ringe: Die Gefährten" →
/// "derherrderringediegefährten").
pub fn normalize_title(title: &str) -> String {
    title
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Snapshot of the library for matching.
pub struct LibraryIndex {
    items: HashMap<i64, IndexItem>,
    /// (kind, provider, value) → item ids
    by_provider: HashMap<(String, String, String), Vec<i64>>,
    /// (kind, normalized title) → item ids
    by_title: HashMap<(String, String), Vec<i64>>,
    /// (series, season, episode) → episode id
    episodes: HashMap<(i64, i64, i64), i64>,
    /// item → provider ids
    ids_of: HashMap<i64, Vec<(String, String)>>,
}

impl LibraryIndex {
    pub async fn load(db: &Db) -> flixnet_db::Result<Self> {
        let (items, ids) = history::match_index(db).await?;
        let mut index = LibraryIndex {
            items: HashMap::new(),
            by_provider: HashMap::new(),
            by_title: HashMap::new(),
            episodes: HashMap::new(),
            ids_of: HashMap::new(),
        };
        for item in items {
            if item.kind == "episode" {
                if let (Some(series), Some(season), Some(first)) =
                    (item.series_id, item.season, item.episode)
                {
                    let last = item.episode_end.unwrap_or(first).max(first);
                    for n in first..=last.min(first + 50) {
                        index.episodes.entry((series, season, n)).or_insert(item.id);
                    }
                }
            } else {
                for title in [Some(&item.title), item.original_title.as_ref()]
                    .into_iter()
                    .flatten()
                {
                    let key = (item.kind.clone(), normalize_title(title));
                    let list = index.by_title.entry(key).or_default();
                    if !list.contains(&item.id) {
                        list.push(item.id);
                    }
                }
            }
            index.items.insert(item.id, item);
        }
        for (item_id, provider, value) in ids {
            let Some(item) = index.items.get(&item_id) else {
                continue;
            };
            let value = normalize_id(&provider, &value);
            index
                .by_provider
                .entry((item.kind.clone(), provider.clone(), value.clone()))
                .or_default()
                .push(item_id);
            index
                .ids_of
                .entry(item_id)
                .or_default()
                .push((provider, value));
        }
        Ok(index)
    }

    pub fn item(&self, id: i64) -> Option<&IndexItem> {
        self.items.get(&id)
    }

    /// Provider ids of an item.
    pub fn ids(&self, id: i64) -> ExternalIds {
        ExternalIds::from_pairs(
            self.ids_of
                .get(&id)
                .into_iter()
                .flatten()
                .map(|(p, v)| (p.as_str(), v.as_str())),
        )
    }

    fn by_ids(&self, kind: &str, ids: &ExternalIds) -> Option<i64> {
        ids.pairs().find_map(|(provider, value)| {
            self.by_provider
                .get(&(
                    kind.to_owned(),
                    provider.to_owned(),
                    normalize_id(provider, value),
                ))
                .and_then(|list| list.first().copied())
        })
    }

    /// By title and year; a title that carries the year itself ("Alpha (2020)", as
    /// Jellyfin names items it has no metadata for) is tried without it as well.
    fn by_title(&self, kind: &str, title: &str, year: Option<i64>) -> Option<i64> {
        let normalized = normalize_title(title);
        self.by_normalized_title(kind, &normalized, year)
            .or_else(|| {
                let (stripped, title_year) = split_trailing_year(&normalized)?;
                if year.is_some_and(|y| y != title_year) {
                    return None;
                }
                self.by_normalized_title(kind, stripped, Some(title_year))
            })
    }

    fn by_normalized_title(&self, kind: &str, title: &str, year: Option<i64>) -> Option<i64> {
        let candidates = self.by_title.get(&(kind.to_owned(), title.to_owned()))?;
        let year_of = |id: &i64| self.items.get(id).and_then(|i| i.year);
        match year {
            Some(year) => candidates
                .iter()
                .find(|id| year_of(id) == Some(year))
                .or_else(|| {
                    // Release years differ by one between databases now and then.
                    let near: Vec<&i64> = candidates
                        .iter()
                        .filter(|id| year_of(id).is_some_and(|y| (y - year).abs() == 1))
                        .collect();
                    (near.len() == 1).then(|| near[0])
                })
                .copied(),
            // Without a year only an unambiguous title counts.
            None => (candidates.len() == 1).then(|| candidates[0]),
        }
    }

    pub fn movie(
        &self,
        ids: &ExternalIds,
        title: &str,
        year: Option<i64>,
    ) -> Option<(i64, MatchMethod)> {
        if let Some(id) = self.by_ids("movie", ids) {
            return Some((id, MatchMethod::ProviderId));
        }
        self.by_title("movie", title, year)
            .map(|id| (id, MatchMethod::TitleYear))
    }

    pub fn series(
        &self,
        ids: &ExternalIds,
        title: &str,
        year: Option<i64>,
    ) -> Option<(i64, MatchMethod)> {
        if let Some(id) = self.by_ids("series", ids) {
            return Some((id, MatchMethod::ProviderId));
        }
        self.by_title("series", title, year)
            .or_else(|| year.and_then(|_| self.by_title("series", title, None)))
            .map(|id| (id, MatchMethod::TitleYear))
    }

    /// An episode: by its own provider ids, else via its series and the numbers.
    #[allow(clippy::too_many_arguments)]
    pub fn episode(
        &self,
        episode_ids: &ExternalIds,
        series_ids: &ExternalIds,
        series_title: &str,
        series_year: Option<i64>,
        season: Option<i64>,
        episode: Option<i64>,
    ) -> Option<(i64, MatchMethod)> {
        if let Some(id) = self.by_ids("episode", episode_ids) {
            return Some((id, MatchMethod::ProviderId));
        }
        let (series, _) = self.series(series_ids, series_title, series_year)?;
        let id = self.episodes.get(&(series, season?, episode?))?;
        Some((*id, MatchMethod::Episode))
    }

    /// The episode `season`/`number` of a series item.
    pub fn episode_of(&self, series: i64, season: i64, number: i64) -> Option<i64> {
        self.episodes.get(&(series, season, number)).copied()
    }
}

/// A normalized title that ends in a plausible release year ("alpha2020") split into
/// the title and the year; the title part must not be empty.
fn split_trailing_year(normalized: &str) -> Option<(&str, i64)> {
    let cut = normalized.len().checked_sub(4)?;
    let (title, digits) = (normalized.get(..cut)?, normalized.get(cut..)?);
    if title.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let year: i64 = digits.parse().ok()?;
    (1880..=2100).contains(&year).then_some((title, year))
}

/// A title without a trailing "(2020)" / "[2020]" / ".2020" (for display).
pub fn strip_year_suffix(title: &str, year: Option<i64>) -> &str {
    let Some(year) = year else {
        return title;
    };
    let trimmed = title.trim_end();
    for suffix in [
        format!("({year})"),
        format!("[{year}]"),
        format!(".{year}"),
        format!(" {year}"),
    ] {
        if let Some(rest) = trimmed.strip_suffix(&suffix) {
            let rest = rest.trim_end_matches([' ', '.', '-', '_']);
            if !rest.is_empty() {
                return rest;
            }
        }
    }
    title
}

fn normalize_id(provider: &str, value: &str) -> String {
    let value = value.trim();
    if provider == "imdb" {
        value.to_ascii_lowercase()
    } else {
        value.trim_start_matches('0').to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_normalize() {
        assert_eq!(normalize_title("Matrix, The"), "matrixthe");
        assert_eq!(
            normalize_title("Die Gefährten (Extended)"),
            "diegefährtenextended"
        );
        assert_eq!(normalize_title("  WALL·E "), "walle");
    }

    #[test]
    fn trailing_years_split_off() {
        assert_eq!(
            split_trailing_year("einfilmohneids2015"),
            Some(("einfilmohneids", 2015))
        );
        assert_eq!(split_trailing_year("1917"), None);
        assert_eq!(split_trailing_year("alpha1234"), None);
        assert_eq!(split_trailing_year("ab"), None);
        assert_eq!(strip_year_suffix("Alpha (2020)", Some(2020)), "Alpha");
        assert_eq!(strip_year_suffix("Alpha.2020", Some(2020)), "Alpha");
        assert_eq!(
            strip_year_suffix("Alpha (2020)", Some(2021)),
            "Alpha (2020)"
        );
        assert_eq!(strip_year_suffix("2020", Some(2020)), "2020");
        assert_eq!(strip_year_suffix("Alpha", None), "Alpha");
    }

    #[test]
    fn ids_from_pairs() {
        let ids = ExternalIds::from_pairs([("Tmdb", "603"), ("Imdb", "tt0133093"), ("X", "1")]);
        assert_eq!(ids.tmdb.as_deref(), Some("603"));
        assert_eq!(ids.imdb.as_deref(), Some("tt0133093"));
        assert!(ids.tvdb.is_none());
        assert!(ExternalIds::default().is_empty());
        assert!(ExternalIds::from_pairs([("tmdb", "0")]).is_empty());
    }
}
