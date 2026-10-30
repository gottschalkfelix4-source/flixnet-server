//! Text helpers that need no model: keyword queries for scene search, snippets and a
//! title check that keeps generated texts from naming catalogue titles.

/// Words that describe the search rather than the scene ("die Szene, wo sie …").
const STOPWORDS: &[&str] = &[
    // German
    "der", "die", "das", "den", "dem", "des", "ein", "eine", "einer", "eines", "einem", "einen",
    "und", "oder", "aber", "mit", "von", "vom", "zu", "zum", "zur", "im", "in", "ins", "am", "an",
    "auf", "aus", "bei", "beim", "für", "über", "unter", "nach", "vor", "um", "als", "wie", "wo",
    "wer", "was", "wann", "warum", "welche", "welcher", "welches", "dass", "da", "dann", "so",
    "sich", "sie", "er", "es", "ich", "du", "wir", "ihr", "ihn", "ihm", "ihnen", "man", "mich",
    "dich", "uns", "euch", "sein", "seine", "ihre", "ist", "sind", "war", "waren", "wird",
    "werden", "hat", "haben", "hatte", "nicht", "kein", "keine", "noch", "schon", "auch", "nur",
    "sehr", "mal", "einmal", "gerade", "jemand", "etwas", "alle", "szene", "szenen", "stelle",
    "moment", "film", "filme", "folge", "folgen", "episode", "serie", "serien", "teil", "wo",
    "gibt", "kommt", "sagt", "sagen", "zeig", "zeige", "finde", "suche", "bitte",
    // English
    "the", "a", "an", "and", "or", "of", "to", "in", "on", "at", "with", "where", "when", "who",
    "what", "which", "that", "this", "is", "are", "was", "were", "he", "she", "they", "it", "his",
    "her", "their", "scene", "part", "movie", "show", "find", "please",
];

fn is_stopword(word: &str) -> bool {
    STOPWORDS.contains(&word)
}

/// Light stemming for German and English so "streiten" finds "Streit", "streitet":
/// common endings are cut while at least four letters remain.
fn stem(word: &str) -> String {
    const ENDINGS: &[&str] = &[
        "ungen", "ung", "heit", "keit", "ern", "en", "er", "es", "em", "et", "st", "te", "e", "n",
        "s", "t", "ing", "ed",
    ];
    let chars = word.chars().count();
    if chars <= 4 {
        return word.to_owned();
    }
    for ending in ENDINGS {
        // Single letters are cut only from longer words ("fight" stays).
        let keep = if ending.len() == 1 { 5 } else { 4 };
        if let Some(stripped) = word.strip_suffix(ending)
            && stripped.chars().count() >= keep
        {
            return stripped.to_owned();
        }
    }
    word.to_owned()
}

/// Lowercase words of a text (letters and digits).
pub fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// The search terms of a scene description: stopwords removed, stemmed.
pub fn scene_terms(text: &str) -> Vec<String> {
    let all = words(text);
    let mut terms: Vec<String> = all
        .iter()
        .filter(|w| !is_stopword(w) && w.chars().count() > 1)
        .map(|w| stem(w))
        .collect();
    if terms.is_empty() {
        // Only stopwords ("wer ist da"): search for them after all.
        terms = all;
    }
    let mut seen = std::collections::HashSet::new();
    terms.retain(|t| seen.insert(t.clone()));
    terms.truncate(8);
    terms
}

/// FTS5 query for scene search: every term as a quoted prefix, joined with OR, so a
/// chunk matching more terms ranks higher (bm25) without requiring all of them.
/// `None` when nothing searchable remains. Quotes cannot break out of the terms.
pub fn scene_fts_query(text: &str) -> Option<String> {
    let terms = scene_terms(text);
    if terms.is_empty() {
        return None;
    }
    Some(
        terms
            .iter()
            .map(|t| format!("\"{}\"*", t.replace('"', "")))
            .collect::<Vec<_>>()
            .join(" OR "),
    )
}

/// A window of about `max_chars` around the first matching term.
pub fn snippet(text: &str, terms: &[String], max_chars: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max_chars {
        return text.to_owned();
    }
    let lower = text.to_lowercase();
    let hit = terms
        .iter()
        .filter_map(|t| lower.find(t.as_str()))
        .min()
        .map(|byte| lower[..byte].chars().count())
        .unwrap_or(0);
    let start = hit.saturating_sub(max_chars / 3);
    let start = start.min(chars.len().saturating_sub(max_chars));
    let end = (start + max_chars).min(chars.len());
    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    out.extend(&chars[start..end]);
    if end < chars.len() {
        out.push('…');
    }
    out
}

/// Whether `text` contains one of `titles` as a whole phrase (case-insensitive;
/// titles shorter than four characters are ignored, they match too much).
pub fn mentions_title<'a>(text: &str, titles: impl IntoIterator<Item = &'a str>) -> bool {
    let haystack = format!(" {} ", words(text).join(" "));
    titles.into_iter().any(|title| {
        let needle = words(title).join(" ");
        needle.chars().count() >= 4 && haystack.contains(&format!(" {needle} "))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scene_descriptions_become_or_prefix_queries() {
        assert_eq!(
            scene_fts_query("die Szene wo sie am Strand streiten").as_deref(),
            Some("\"strand\"* OR \"streit\"*")
        );
        assert_eq!(
            scene_fts_query("the scene where they fight on the beach").as_deref(),
            Some("\"fight\"* OR \"beach\"*")
        );
        // Only stopwords: searched as they are.
        assert_eq!(
            scene_fts_query("wer ist da").as_deref(),
            Some("\"wer\"* OR \"ist\"* OR \"da\"*")
        );
        assert_eq!(scene_fts_query(" ?! "), None);
        // Quotes and operators stay inside the terms.
        assert_eq!(
            scene_fts_query("\"Hallo\" NEAR(x)").as_deref(),
            Some("\"hallo\"* OR \"near\"*")
        );
    }

    #[test]
    fn stemming_keeps_four_letters() {
        assert_eq!(stem("streiten"), "streit");
        assert_eq!(stem("strand"), "strand");
        assert_eq!(stem("haus"), "haus");
        assert_eq!(stem("zeitreisen"), "zeitreis");
    }

    #[test]
    fn snippets_center_on_the_hit() {
        let text = format!("{} Strand {}", "a ".repeat(100), "b ".repeat(100));
        let s = snippet(&text, &["strand".to_owned()], 40);
        assert!(s.contains("Strand"), "{s}");
        assert!(s.starts_with('…') && s.ends_with('…'));
        assert_eq!(snippet("kurz", &[], 40), "kurz");
    }

    #[test]
    fn titles_are_found_as_phrases() {
        let titles = ["Der Pate", "Up", "Geheimfilm"];
        assert!(mentions_title("Wie in Der Pate.", titles));
        assert!(mentions_title("„Geheimfilm“ und mehr", titles));
        assert!(!mentions_title("Upgrade und Patentante", titles));
        assert!(!mentions_title("Geheimfilme", titles));
    }
}
