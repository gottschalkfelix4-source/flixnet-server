//! The text an item is embedded with: everything that describes what it is about.

use flixnet_db::ai::EmbeddingSource;
use sha2::{Digest, Sha256};

fn list(field: &Option<String>) -> Vec<&str> {
    field
        .as_deref()
        .map(|s| {
            s.split('|')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Title, original title, year, genres, tagline, overview, keywords, cast and crew,
/// in that order (the model truncates long texts at the end).
pub fn embedding_text(s: &EmbeddingSource) -> String {
    let mut out = s.title.trim().to_owned();
    if let Some(original) = s.original_title.as_deref().map(str::trim)
        && !original.is_empty()
        && !original.eq_ignore_ascii_case(s.title.trim())
    {
        out.push_str(&format!(" ({original})"));
    }
    if let Some(year) = s.year {
        out.push_str(&format!(", {year}"));
    }
    out.push_str(if s.kind == "series" {
        ". Serie."
    } else {
        ". Film."
    });
    let genres = list(&s.genres);
    if !genres.is_empty() {
        out.push_str(&format!(" Genres: {}.", genres.join(", ")));
    }
    for text in [&s.tagline, &s.overview].into_iter().flatten() {
        let text = text.trim();
        if !text.is_empty() {
            out.push(' ');
            out.push_str(text);
            if !text.ends_with(['.', '!', '?', '…']) {
                out.push('.');
            }
        }
    }
    let keywords = list(&s.keywords);
    if !keywords.is_empty() {
        out.push_str(&format!(" Schlagwörter: {}.", keywords.join(", ")));
    }
    let cast = list(&s.cast);
    if !cast.is_empty() {
        out.push_str(&format!(" Mit {}.", cast.join(", ")));
    }
    let directors = list(&s.directors);
    if !directors.is_empty() {
        out.push_str(&format!(" Von {}.", directors.join(", ")));
    }
    out
}

/// Fingerprint of the text plus the model, so a changed text or model re-embeds.
pub fn text_hash(model_key: &str, text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(model_key.as_bytes());
    hasher.update([0]);
    hasher.update(text.as_bytes());
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_contains_every_descriptive_field() {
        let source = EmbeddingSource {
            id: 1,
            kind: "movie".into(),
            title: "Der Pate".into(),
            original_title: Some("The Godfather".into()),
            year: Some(1972),
            overview: Some("Die Geschichte der Corleone-Familie".into()),
            tagline: Some("Ein Angebot, das man nicht ablehnen kann.".into()),
            genres: Some("Drama|Krimi".into()),
            keywords: Some("mafia|new york".into()),
            cast: Some("Marlon Brando|Al Pacino".into()),
            directors: Some("Francis Ford Coppola".into()),
        };
        assert_eq!(
            embedding_text(&source),
            "Der Pate (The Godfather), 1972. Film. Genres: Drama, Krimi. Ein Angebot, das man nicht \
             ablehnen kann. Die Geschichte der Corleone-Familie. Schlagwörter: mafia, new york. \
             Mit Marlon Brando, Al Pacino. Von Francis Ford Coppola."
        );
        let bare = EmbeddingSource {
            id: 2,
            kind: "series".into(),
            title: "Dark".into(),
            original_title: Some("Dark".into()),
            ..Default::default()
        };
        assert_eq!(embedding_text(&bare), "Dark. Serie.");
        assert_ne!(text_hash("a", "x"), text_hash("b", "x"));
        assert_eq!(text_hash("a", "x"), text_hash("a", "x"));
    }
}
