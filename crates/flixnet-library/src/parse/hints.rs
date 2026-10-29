//! Provider id hints embedded in names: `{tmdb-603}`, `[imdbid-tt0133093]`, `[tvdbid-81189]`.

use std::sync::LazyLock;

use regex::Regex;
use serde::Serialize;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ProviderHints {
    pub tmdb: Option<u64>,
    pub tvdb: Option<u64>,
    /// IMDb id including the `tt` prefix.
    pub imdb: Option<String>,
}

impl ProviderHints {
    /// Fills fields that are still empty from `other` (outer folders first, file last).
    pub fn merge(&mut self, other: ProviderHints) {
        if other.tmdb.is_some() {
            self.tmdb = other.tmdb;
        }
        if other.tvdb.is_some() {
            self.tvdb = other.tvdb;
        }
        if other.imdb.is_some() {
            self.imdb = other.imdb;
        }
    }

    pub fn is_empty(&self) -> bool {
        self.tmdb.is_none() && self.tvdb.is_none() && self.imdb.is_none()
    }
}

static HINT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)[\[{(]\s*(tmdb|tmdbid|tvdb|tvdbid|imdb|imdbid)\s*[-=: ]\s*([a-z0-9]+)\s*[\]})]",
    )
    .expect("valid regex")
});

/// Bare IMDb ids anywhere in a name: `tt0133093`.
static BARE_IMDB: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(tt\d{7,9})\b").expect("valid regex"));

/// Extracts hints and returns the name with the hint markers removed.
pub fn extract(name: &str) -> (ProviderHints, String) {
    let mut hints = ProviderHints::default();
    for caps in HINT.captures_iter(name) {
        let value = &caps[2];
        match caps[1].to_ascii_lowercase().as_str() {
            "tmdb" | "tmdbid" => hints.tmdb = value.parse().ok(),
            "tvdb" | "tvdbid" => hints.tvdb = value.parse().ok(),
            _ => {
                let id = value.to_ascii_lowercase();
                if id.starts_with("tt") && id[2..].bytes().all(|b| b.is_ascii_digit()) {
                    hints.imdb = Some(id);
                }
            }
        }
    }
    let mut cleaned = HINT.replace_all(name, " ").into_owned();
    if hints.imdb.is_none()
        && let Some(caps) = BARE_IMDB.captures(&cleaned)
    {
        hints.imdb = Some(caps[1].to_ascii_lowercase());
        cleaned = BARE_IMDB.replace_all(&cleaned, " ").into_owned();
    }
    (hints, cleaned)
}
