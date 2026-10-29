//! Matching tuner channels to guide channels.
//!
//! In order of trust: the playlist's `tvg-id` equals the XMLTV id (1.0); a normalised
//! name equals a display name or the id's stem (0.95); the channel number equals a
//! numeric display name and the names are similar (0.9); otherwise the best
//! Jaro–Winkler similarity of the normalised names, scaled to at most 0.9. Below
//! [`THRESHOLD`] a channel stays unmatched and waits for the admin.
//!
//! Normalising folds case, umlauts and punctuation and drops what providers add to
//! names: quality tags (HD, FHD, 4K, …), country prefixes (`DE:`, `[AT]`) and domain
//! suffixes of ids (`daserste.de`).

use std::collections::HashMap;

use flixnet_db::livetv::{self, Channel, EpgChannel, EpgMatch};
use flixnet_db::{Db, DbError};

/// Lowest score accepted automatically.
pub const THRESHOLD: f64 = 0.8;

const QUALITY: &[&str] = &[
    "hd", "fhd", "uhd", "sd", "4k", "8k", "hevc", "h264", "h265", "1080p", "1080i", "720p", "576i",
    "50fps", "hdr", "raw",
];
const COUNTRIES: &[&str] = &[
    "de", "at", "ch", "uk", "gb", "us", "fr", "it", "es", "nl", "pl", "tr", "be", "dk", "se", "no",
    "fi", "pt", "ger", "deu",
];
const DOMAINS: &[&str] = &[
    "com", "net", "org", "tv", "de", "at", "ch", "uk", "fr", "it", "es",
];

fn fold(c: char) -> &'static str {
    match c {
        'ä' | 'Ä' => "ae",
        'ö' | 'Ö' => "oe",
        'ü' | 'Ü' => "ue",
        'ß' => "ss",
        'á' | 'à' | 'â' | 'Á' | 'À' | 'Â' => "a",
        'é' | 'è' | 'ê' | 'ë' | 'É' | 'È' | 'Ê' => "e",
        'í' | 'ì' | 'î' | 'ï' => "i",
        'ó' | 'ò' | 'ô' | 'Ó' => "o",
        'ú' | 'ù' | 'û' => "u",
        'ç' => "c",
        'ñ' => "n",
        '&' => " and ",
        '+' => " plus ",
        _ => "",
    }
}

/// Lower-case words without punctuation, umlauts folded.
fn words(text: &str) -> Vec<String> {
    let mut folded = String::with_capacity(text.len());
    for c in text.chars() {
        let replacement = fold(c);
        if !replacement.is_empty() {
            folded.push_str(replacement);
        } else if c.is_alphanumeric() {
            folded.extend(c.to_lowercase());
        } else {
            folded.push(' ');
        }
    }
    folded.split_whitespace().map(str::to_owned).collect()
}

/// A channel name reduced to what identifies it (`"DE: Das Erste HD"` → `daserste`).
pub fn normalize(name: &str) -> String {
    // A leading country tag (`DE:`, `DE |`, `[DE]`, `DE -`) when more follows.
    let trimmed = name.trim();
    let mut rest = trimmed;
    if let Some(inner) = trimmed.strip_prefix('[').and_then(|r| r.split_once(']')) {
        if COUNTRIES.contains(&inner.0.trim().to_lowercase().as_str()) {
            rest = inner.1;
        }
    } else if let Some((head, tail)) = trimmed.split_once([':', '|'])
        && COUNTRIES.contains(&head.trim().to_lowercase().as_str())
        && !tail.trim().is_empty()
    {
        rest = tail;
    } else if let Some((head, tail)) = trimmed.split_once(" - ")
        && COUNTRIES.contains(&head.trim().to_lowercase().as_str())
    {
        rest = tail;
    }
    let kept: Vec<String> = words(rest)
        .into_iter()
        .filter(|w| !QUALITY.contains(&w.as_str()))
        .collect();
    let joined = kept.concat();
    if joined.is_empty() {
        words(trimmed).concat()
    } else {
        joined
    }
}

/// An XMLTV id without its domain suffix (`daserste.de` → `daserste`, `ZDF.de` →
/// `zdf`); ids without dots are normalised like names.
pub fn id_stem(id: &str) -> String {
    let lower = id.trim().to_lowercase();
    let mut parts: Vec<&str> = lower.split('.').collect();
    while parts.len() > 1 && parts.last().is_some_and(|p| DOMAINS.contains(p)) {
        parts.pop();
    }
    normalize(&parts.join(" "))
}

/// A guide channel prepared for matching.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub source_id: i64,
    pub channel_id: String,
    id_lower: String,
    stem: String,
    names: Vec<String>,
    numbers: Vec<String>,
}

impl Candidate {
    pub fn new(source_id: i64, channel_id: &str, display_names: &[String]) -> Self {
        let mut names: Vec<String> = Vec::new();
        let mut numbers = Vec::new();
        for name in display_names {
            let trimmed = name.trim();
            if !trimmed.is_empty() && trimmed.chars().all(|c| c.is_ascii_digit() || c == '.') {
                numbers.push(trimmed.to_owned());
                continue;
            }
            let normalized = normalize(trimmed);
            if !normalized.is_empty() && !names.contains(&normalized) {
                names.push(normalized);
            }
        }
        Self {
            source_id,
            channel_id: channel_id.to_owned(),
            id_lower: channel_id.trim().to_lowercase(),
            stem: id_stem(channel_id),
            names,
            numbers,
        }
    }
}

/// What a tuner channel offers for matching.
#[derive(Debug, Clone, Default)]
pub struct ChannelKey<'a> {
    pub tvg_id: Option<&'a str>,
    pub name: &'a str,
    pub tvg_name: Option<&'a str>,
    pub number: Option<&'a str>,
}

/// Index over all candidates: exact lookups first, fuzzy comparison only within a
/// bucket of names sharing the first letter.
pub struct Matcher {
    candidates: Vec<Candidate>,
    by_id: HashMap<String, usize>,
    by_name: HashMap<String, usize>,
    by_letter: HashMap<char, Vec<usize>>,
}

impl Matcher {
    pub fn new(candidates: Vec<Candidate>) -> Self {
        let mut by_id = HashMap::new();
        let mut by_name = HashMap::new();
        let mut by_letter: HashMap<char, Vec<usize>> = HashMap::new();
        for (i, c) in candidates.iter().enumerate() {
            by_id.entry(c.id_lower.clone()).or_insert(i);
            for name in c.names.iter().chain(std::iter::once(&c.stem)) {
                if name.is_empty() {
                    continue;
                }
                by_name.entry(name.clone()).or_insert(i);
                if let Some(first) = name.chars().next() {
                    let bucket = by_letter.entry(first).or_default();
                    if bucket.last() != Some(&i) {
                        bucket.push(i);
                    }
                }
            }
        }
        Self {
            candidates,
            by_id,
            by_name,
            by_letter,
        }
    }

    pub fn candidate(&self, index: usize) -> &Candidate {
        &self.candidates[index]
    }

    /// The best candidate and its score, if any reaches [`THRESHOLD`].
    pub fn best(&self, channel: &ChannelKey<'_>) -> Option<(usize, f64)> {
        if let Some(id) = channel.tvg_id.map(|i| i.trim().to_lowercase())
            && let Some(&i) = self.by_id.get(&id)
        {
            return Some((i, 1.0));
        }
        let mut names: Vec<String> = [Some(channel.name), channel.tvg_name]
            .into_iter()
            .flatten()
            .map(normalize)
            .filter(|n| !n.is_empty())
            .collect();
        if let Some(id) = channel.tvg_id {
            let stem = id_stem(id);
            if !stem.is_empty() {
                names.push(stem);
            }
        }
        names.dedup();
        for name in &names {
            if let Some(&i) = self.by_name.get(name) {
                return Some((i, 0.95));
            }
        }

        let mut best: Option<(usize, f64)> = None;
        for name in &names {
            let Some(first) = name.chars().next() else {
                continue;
            };
            for &i in self.by_letter.get(&first).into_iter().flatten() {
                let c = &self.candidates[i];
                let similarity = c
                    .names
                    .iter()
                    .chain(std::iter::once(&c.stem))
                    .map(|n| strsim::jaro_winkler(name, n))
                    .fold(0.0, f64::max);
                let numbered = channel
                    .number
                    .is_some_and(|n| c.numbers.iter().any(|m| m == n.trim()));
                let score = if numbered && similarity >= 0.6 {
                    0.9
                } else {
                    similarity * 0.9
                };
                if best.is_none_or(|(_, s)| score > s) {
                    best = Some((i, score));
                }
            }
        }
        best.filter(|(_, score)| *score >= THRESHOLD)
    }
}

/// Builds the matcher over guide channels as stored.
pub fn matcher(epg_channels: &[EpgChannel]) -> Matcher {
    Matcher::new(
        epg_channels
            .iter()
            .map(|e| Candidate::new(e.epg_source_id, &e.channel_id, &e.names()))
            .collect(),
    )
}

/// Matches every channel that has no manual match. Returns how many got a guide.
pub async fn auto_match(db: &Db) -> Result<usize, DbError> {
    let channels = livetv::list_channels(db, false).await?;
    let epg = livetv::list_epg_channels(db, None).await?;
    let matches = tokio::task::spawn_blocking(move || compute(&channels, &epg))
        .await
        .expect("matching does not panic");
    let matched = matches.iter().filter(|m| m.kind.is_some()).count();
    livetv::set_epg_matches(db, &matches).await?;
    Ok(matched)
}

/// The matches for `channels` (manual ones are skipped).
pub fn compute(channels: &[Channel], epg: &[EpgChannel]) -> Vec<EpgMatch> {
    let matcher = matcher(epg);
    channels
        .iter()
        .filter(|c| c.epg_match.as_deref() != Some("manual"))
        .map(|c| {
            let key = ChannelKey {
                tvg_id: c.tvg_id.as_deref(),
                name: &c.name,
                tvg_name: c.tvg_name.as_deref(),
                number: c.number.as_deref(),
            };
            match matcher.best(&key) {
                Some((i, score)) => {
                    let candidate = matcher.candidate(i);
                    EpgMatch {
                        channel_id: c.id,
                        epg_source_id: Some(candidate.source_id),
                        epg_channel_id: Some(candidate.channel_id.clone()),
                        kind: Some("auto"),
                        score: Some((score * 1000.0).round() / 1000.0),
                    }
                }
                None => EpgMatch {
                    channel_id: c.id,
                    epg_source_id: None,
                    epg_channel_id: None,
                    kind: None,
                    score: None,
                },
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    fn guide() -> Matcher {
        Matcher::new(vec![
            Candidate::new(1, "daserste.de", &names(&["Das Erste", "ARD", "1"])),
            Candidate::new(1, "zdf.de", &names(&["ZDF", "2"])),
            Candidate::new(1, "zdfneo.de", &names(&["ZDFneo"])),
            Candidate::new(1, "prosieben.de", &names(&["ProSieben"])),
            Candidate::new(1, "ProSiebenMaxx.de", &names(&["ProSieben MAXX"])),
            Candidate::new(1, "rtl.de", &names(&["RTL Television"])),
            Candidate::new(1, "rtlplus.de", &names(&["RTL+", "RTLup"])),
            Candidate::new(1, "kika.de", &names(&["KiKA"])),
            Candidate::new(1, "3sat.de", &names(&["3sat", "8"])),
            Candidate::new(1, "phoenix.de", &names(&["Phönix"])),
        ])
    }

    fn best(m: &Matcher, key: ChannelKey<'_>) -> Option<(String, f64)> {
        m.best(&key)
            .map(|(i, s)| (m.candidate(i).channel_id.clone(), s))
    }

    fn by_name(name: &str) -> ChannelKey<'_> {
        ChannelKey {
            name,
            ..Default::default()
        }
    }

    #[test]
    fn normalizes_provider_names() {
        assert_eq!(normalize("DE: Das Erste HD"), "daserste");
        assert_eq!(normalize("[AT] ORF 1 FHD"), "orf1");
        assert_eq!(normalize("DE | ZDF neo 1080p"), "zdfneo");
        assert_eq!(normalize("Phönix"), "phoenix");
        assert_eq!(normalize("HD"), "hd", "a name of only tags is kept");
        assert_eq!(id_stem("daserste.de"), "daserste");
        assert_eq!(id_stem("ZDF.de"), "zdf");
        assert_eq!(
            id_stem("I10.12345.schedulesdirect.org"),
            "i1012345schedulesdirect"
        );
    }

    #[test]
    fn exact_ids_and_names_win() {
        let m = guide();
        let by_id = ChannelKey {
            tvg_id: Some("ZDF.de"),
            name: "irgendwas",
            ..Default::default()
        };
        assert_eq!(best(&m, by_id), Some(("zdf.de".into(), 1.0)));
        assert_eq!(
            best(&m, by_name("DE: Das Erste HD")),
            Some(("daserste.de".into(), 0.95))
        );
        assert_eq!(
            best(&m, by_name("ARD HD")),
            Some(("daserste.de".into(), 0.95))
        );
        assert_eq!(
            best(&m, by_name("ZDF neo HD")),
            Some(("zdfneo.de".into(), 0.95))
        );
        assert_eq!(
            best(&m, by_name("Phoenix HD")),
            Some(("phoenix.de".into(), 0.95))
        );
        // The id's stem: a playlist id that differs from the guide's in case and domain.
        let stem = ChannelKey {
            tvg_id: Some("KIKA.de"),
            name: "Kinderkanal",
            ..Default::default()
        };
        assert_eq!(best(&m, stem), Some(("kika.de".into(), 1.0)));
    }

    #[test]
    fn fuzzy_and_numbers() {
        let m = guide();
        let (id, score) = best(&m, by_name("ProSieben Maxx HD+")).unwrap();
        assert_eq!(id, "ProSiebenMaxx.de");
        assert!((THRESHOLD..0.95).contains(&score), "{score}");
        let (id, _) = best(&m, by_name("RTL Televison")).unwrap();
        assert_eq!(id, "rtl.de", "typo still matches");

        let numbered = ChannelKey {
            name: "3 Sat Kultur",
            number: Some("8"),
            ..Default::default()
        };
        let (id, score) = best(&m, numbered).unwrap();
        assert_eq!((id.as_str(), score), ("3sat.de", 0.9));

        assert_eq!(
            best(&m, by_name("Bibel TV")),
            None,
            "unknown channels stay unmatched"
        );
        assert_eq!(best(&m, by_name("")), None);
    }
}
