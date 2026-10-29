//! Title/year splitting and name normalization.

use std::sync::LazyLock;

use regex::Regex;

use super::tags;

/// Title, year and whatever text followed them (release tags).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TitleYear {
    pub title: String,
    pub year: Option<u16>,
    pub rest: String,
}

static EDITION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\{edition-([^}]+)\}").expect("valid regex"));

/// Plex-style `{edition-Director's Cut}`.
pub fn extract_edition(name: &str) -> (Option<String>, String) {
    match EDITION.captures(name) {
        Some(caps) => (
            Some(caps[1].trim().to_owned()),
            EDITION.replace_all(name, " ").into_owned(),
        ),
        None => (None, name.to_owned()),
    }
}

/// A single digit, a dot, a single digit: channel layouts like `5.1`, `2.0`.
/// (`1968.1080p` must split, so multi-digit neighbours do not match.)
static DOTTED_NUMBER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(^|\D)(\d)\.(\d)(\D|$)").expect("valid regex"));
static BRACKETS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[\[\](){}]").expect("valid regex"));
static SPACES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").expect("valid regex"));

/// Turns release-style names into space-separated words:
/// `The.Matrix.1999.1080p.DD5.1.H.264-GRP` → `The Matrix 1999 1080p DD5·1 H·264-GRP`.
/// Dots between digits survive as `·` so `5.1` and `H.264` stay one token.
pub fn normalize(name: &str) -> String {
    // Dots separate words only in release-style names ("The.Matrix.1999"); in names
    // that use spaces they belong to the title ("Kill Bill Vol. 1", "Mr. Robot").
    let dots_are_separators = name.matches('.').count() > name.matches(' ').count();
    let protected = DOTTED_NUMBER.replace_all(name, "${1}${2}\u{b7}${3}${4}");
    let mut out = String::with_capacity(protected.len());
    let mut chars = protected.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '_' => out.push(' '),
            // "H.264" → keep the dot before a digit after a single letter (h/x)
            '.' if !dots_are_separators => out.push('.'),
            '.' => {
                let prev_is_hx = out
                    .chars()
                    .last()
                    .is_some_and(|p| matches!(p, 'h' | 'H' | 'x' | 'X'))
                    && out
                        .chars()
                        .rev()
                        .nth(1)
                        .is_none_or(|p| !p.is_alphanumeric());
                if prev_is_hx && chars.peek().is_some_and(char::is_ascii_digit) {
                    // drop the dot: "H.264" → "H264"
                } else {
                    out.push(' ');
                }
            }
            c => out.push(c),
        }
    }
    let out = BRACKETS.replace_all(&out, " ");
    SPACES.replace_all(out.trim(), " ").into_owned()
}

fn year_of(token: &str) -> Option<u16> {
    let digits = token.trim_matches(|c: char| !c.is_ascii_digit());
    if digits.len() == 4 && token.chars().filter(char::is_ascii_digit).count() == 4 {
        let year: u16 = digits.parse().ok()?;
        (1890..=2100).contains(&year).then_some(year)
    } else {
        None
    }
}

/// Splits `Title Year tags…` (already normalized or raw).
///
/// The year is the *last* plausible year that is not the first word (so `1917 2019`
/// keeps "1917" as title and `2001 A Space Odyssey 1968` works). Without a year the
/// title ends at the first release tag.
pub fn split_title(raw: &str) -> TitleYear {
    let normalized = normalize(raw);
    let tokens: Vec<&str> = normalized.split(' ').filter(|t| !t.is_empty()).collect();

    // Titles end at the first release tag. The year may still follow soft tags such as
    // editions or languages ("Alpha Remastered (2020)"), but never technical ones
    // (resolution, source, codec), which only appear after the year.
    let first_tag = tokens
        .iter()
        .enumerate()
        .skip(1)
        .find(|(_, t)| tags::is_tag_token(t))
        .map_or(tokens.len(), |(i, _)| i);
    let first_technical = tokens
        .iter()
        .enumerate()
        .skip(1)
        .find(|(_, t)| tags::is_technical_tag(t))
        .map_or(tokens.len(), |(i, _)| i);

    // A lone year as the only word is a title ("1917"), hence the search starts at 1.
    let year_index = (1..first_technical.min(tokens.len()))
        .rev()
        .find(|&i| year_of(tokens[i]).is_some());

    let (title_end, year, rest_start) = match year_index {
        // Tags between title and year ("Remastered") stay in `rest` for tag parsing.
        Some(i) if first_tag < i => (first_tag, year_of(tokens[i]), first_tag),
        Some(i) => (i, year_of(tokens[i]), i + 1),
        None => (first_tag, None, first_tag),
    };

    let title = clean_title(&tokens[..title_end].join(" "));
    TitleYear {
        title,
        year,
        rest: tokens[rest_start.min(tokens.len())..].join(" "),
    }
}

/// Text after an episode marker: `- Episode Title 1080p WEB` → ("Episode Title", "1080p WEB").
pub fn title_before_tags(text: &str) -> (Option<String>, &str) {
    let mut end = text.len();
    let mut offset = 0;
    for token in text.split(' ') {
        if !token.is_empty() && tags::is_tag_token(token) {
            end = offset;
            break;
        }
        offset += token.len() + 1;
    }
    let title = clean_title(&text[..end.min(text.len())]);
    let rest = &text[end.min(text.len())..];
    ((!title.is_empty()).then_some(title), rest)
}

/// Trims separators and stray punctuation around a title.
fn clean_title(title: &str) -> String {
    let trimmed = title
        .trim_matches(|c: char| c.is_whitespace() || matches!(c, '-' | '–' | ',' | ':' | '+'))
        .trim_start_matches('.');
    SPACES.replace_all(trimmed, " ").into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_keeps_numbers_and_codecs_together() {
        assert_eq!(
            normalize("The.Matrix.1999.DD5.1.H.264-GRP"),
            "The Matrix 1999 DD5·1 H264-GRP"
        );
        assert_eq!(normalize("Some_Movie_[2020]"), "Some Movie 2020");
    }

    #[test]
    fn year_is_last_plausible_year_not_first_word() {
        let t = split_title("1917 (2019)");
        assert_eq!((t.title.as_str(), t.year), ("1917", Some(2019)));
        let t = split_title("2001.A.Space.Odyssey.1968.1080p");
        assert_eq!(
            (t.title.as_str(), t.year),
            ("2001 A Space Odyssey", Some(1968))
        );
        let t = split_title("Blade Runner 2049 (2017)");
        assert_eq!(
            (t.title.as_str(), t.year),
            ("Blade Runner 2049", Some(2017))
        );
    }
}
