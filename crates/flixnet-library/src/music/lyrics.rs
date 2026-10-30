//! Lyrics: LRC files next to the track (`Title.lrc`) or lyrics embedded in the tags,
//! synced (timestamps) or plain.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde::Serialize;

/// One line of synced lyrics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LyricLine {
    pub time_ms: i64,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Lyrics {
    /// Lines have meaningful timestamps.
    pub synced: bool,
    /// Synced: sorted by time. Plain: one entry per line, all at 0.
    pub lines: Vec<LyricLine>,
    /// `sidecar` (an .lrc/.txt next to the file) or `embedded`.
    pub source: &'static str,
}

static TIMESTAMP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\[(\d{1,3}):(\d{1,2})(?:[.:](\d{1,3}))?\]").expect("valid regex")
});
static OFFSET: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^\[offset:\s*([+-]?\d+)\s*\]").expect("valid regex"));
/// `[ar:…]`, `[ti:…]`, `[length:…]` and other ID tags.
static ID_TAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\[[A-Za-z#]+:[^\]]*\]\s*$").expect("valid regex"));

fn fraction_ms(digits: Option<&str>) -> i64 {
    match digits {
        None => 0,
        Some(d) => {
            let value: i64 = d.parse().unwrap_or(0);
            match d.len() {
                1 => value * 100,
                2 => value * 10,
                _ => value,
            }
        }
    }
}

/// Parses LRC text. Lines without timestamps make the result plain text; a line may
/// carry several timestamps (`[00:12.00][01:30.00]Chorus`). `[offset:+500]` shifts
/// everything earlier by 500 ms (LRC convention: positive = sooner).
pub fn parse(text: &str, source: &'static str) -> Option<Lyrics> {
    let text = text.trim_start_matches('\u{feff}');
    let mut offset = 0_i64;
    let mut synced: Vec<LyricLine> = Vec::new();
    let mut plain: Vec<String> = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if let Some(caps) = OFFSET.captures(line) {
            offset = caps[1].parse().unwrap_or(0);
            continue;
        }
        if ID_TAG.is_match(line) && !TIMESTAMP.is_match(line) {
            continue;
        }
        let mut times = Vec::new();
        let mut rest = line;
        while let Some(caps) = TIMESTAMP.captures(rest) {
            let whole = caps.get(0).expect("match");
            if whole.start() != 0 {
                break;
            }
            let minutes: i64 = caps[1].parse().unwrap_or(0);
            let seconds: i64 = caps[2].parse().unwrap_or(0);
            times.push(
                minutes * 60_000 + seconds * 1000 + fraction_ms(caps.get(3).map(|m| m.as_str())),
            );
            rest = rest[whole.end()..].trim_start();
        }
        // Enhanced LRC word timings (`<00:12.30>`) are dropped from the text.
        let text = strip_word_times(rest);
        if times.is_empty() {
            plain.push(text);
        } else {
            for time in times {
                synced.push(LyricLine {
                    time_ms: (time - offset).max(0),
                    text: text.clone(),
                });
            }
        }
    }
    if !synced.is_empty() {
        synced.sort_by_key(|l| l.time_ms);
        return Some(Lyrics {
            synced: true,
            lines: synced,
            source,
        });
    }
    // Plain text: trim surrounding blank lines, keep inner ones (verse breaks).
    while plain.last().is_some_and(|l| l.is_empty()) {
        plain.pop();
    }
    let first = plain.iter().position(|l| !l.is_empty())?;
    Some(Lyrics {
        synced: false,
        lines: plain[first..]
            .iter()
            .map(|text| LyricLine {
                time_ms: 0,
                text: text.clone(),
            })
            .collect(),
        source,
    })
}

fn strip_word_times(text: &str) -> String {
    static WORD: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"<\d{1,3}:\d{1,2}(?:[.:]\d{1,3})?>").expect("valid regex"));
    WORD.replace_all(text, "").trim().to_owned()
}

/// Sidecar lyrics files for a track, most specific first: `<stem>.lrc`, `<stem>.txt`.
fn sidecars(audio: &Path) -> Vec<PathBuf> {
    ["lrc", "LRC", "txt"]
        .iter()
        .map(|ext| audio.with_extension(ext))
        .collect()
}

/// Lyrics of an audio file: a sidecar `.lrc` first (usually synced), then lyrics
/// embedded in the tags. Blocking (reads files).
pub fn find(audio: &Path) -> Option<Lyrics> {
    for path in sidecars(audio) {
        if let Ok(bytes) = std::fs::read(&path)
            && let Some(lyrics) = parse(&String::from_utf8_lossy(&bytes), "sidecar")
        {
            return Some(lyrics);
        }
    }
    super::tags::embedded_lyrics(audio).and_then(|text| parse(&text, "embedded"))
}

/// Whether a sidecar lyrics file exists (cheap check during scans).
pub fn has_sidecar(audio: &Path) -> bool {
    sidecars(audio).iter().any(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synced_lrc_with_repeats_offset_and_tags() {
        let text = "\u{feff}[ar:Anna]\n[ti:Lied]\n[offset:+250]\n\
                    [00:01.00]Erste Zeile\n[00:03.5][00:10.250]Refrain <00:03.80>la\n\
                    [00:02.00]\n";
        let lyrics = parse(text, "sidecar").unwrap();
        assert!(lyrics.synced);
        let lines: Vec<(i64, &str)> = lyrics
            .lines
            .iter()
            .map(|l| (l.time_ms, l.text.as_str()))
            .collect();
        assert_eq!(
            lines,
            [
                (750, "Erste Zeile"),
                (1750, ""),
                (3250, "Refrain la"),
                (10_000, "Refrain la"),
            ]
        );
    }

    #[test]
    fn plain_text_is_kept_line_by_line() {
        let lyrics = parse("\n\nStrophe eins\n\nStrophe zwei\n\n", "embedded").unwrap();
        assert!(!lyrics.synced);
        let lines: Vec<&str> = lyrics.lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(lines, ["Strophe eins", "", "Strophe zwei"]);
        assert_eq!(parse("  \n[ar:Nur Tags]\n", "embedded"), None);
    }
}
