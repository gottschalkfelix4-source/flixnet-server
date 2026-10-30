//! Spoken text with timestamps, for scene search: subtitle cues (WebVTT or SubRip)
//! cleaned of markup and grouped into windows of roughly 30–60 seconds.

/// One subtitle cue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cue {
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
}

/// A window of consecutive cues.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextChunk {
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
}

/// How cues are grouped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkOptions {
    /// A window is closed once it spans at least this long…
    pub target_ms: i64,
    /// …and never grows beyond this (a single longer cue still forms one window).
    pub max_ms: i64,
    /// A silence longer than this closes the window early (a new scene), as long as
    /// the window already spans `min_ms`.
    pub gap_ms: i64,
    pub min_ms: i64,
}

impl Default for ChunkOptions {
    fn default() -> Self {
        Self {
            target_ms: 45_000,
            max_ms: 60_000,
            gap_ms: 8_000,
            min_ms: 30_000,
        }
    }
}

/// Parses WebVTT or SubRip text (timestamps `00:01:02.345` / `00:01:02,345`, hours
/// optional). Cue settings, numbering, NOTE/STYLE blocks and markup are dropped;
/// cues without text are skipped. Cues come back sorted by start time.
pub fn parse_cues(text: &str) -> Vec<Cue> {
    let text = text
        .trim_start_matches('\u{feff}')
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let mut cues = Vec::new();
    for block in text.split("\n\n") {
        let mut lines = block
            .lines()
            .map(str::trim_end)
            .skip_while(|l| l.trim().is_empty());
        let mut timing = None;
        let mut body = Vec::new();
        for line in lines.by_ref() {
            if timing.is_none() {
                if let Some(t) = parse_timing(line) {
                    timing = Some(t);
                }
                // Anything before the timing line (cue number, identifier, header).
                continue;
            }
            body.push(line);
        }
        let Some((start_ms, end_ms)) = timing else {
            continue;
        };
        let cleaned = clean_text(&body.join(" "));
        if !cleaned.is_empty() && end_ms >= start_ms {
            cues.push(Cue {
                start_ms,
                end_ms,
                text: cleaned,
            });
        }
    }
    cues.sort_by_key(|c| (c.start_ms, c.end_ms));
    cues
}

fn parse_timing(line: &str) -> Option<(i64, i64)> {
    let (start, rest) = line.split_once("-->")?;
    let end = rest.split_whitespace().next()?;
    Some((parse_timestamp(start.trim())?, parse_timestamp(end)?))
}

/// `hh:mm:ss.mmm`, `mm:ss.mmm` or with a comma (SubRip).
fn parse_timestamp(s: &str) -> Option<i64> {
    let s = s.replace(',', ".");
    let (clock, frac) = s.split_once('.').unwrap_or((&s, "0"));
    let parts: Vec<i64> = clock
        .split(':')
        .map(|p| p.trim().parse::<i64>().ok())
        .collect::<Option<_>>()?;
    let (h, m, sec) = match parts.as_slice() {
        [h, m, s] => (*h, *m, *s),
        [m, s] => (0, *m, *s),
        _ => return None,
    };
    let digits: String = frac
        .chars()
        .take_while(char::is_ascii_digit)
        .take(3)
        .collect();
    let ms = match digits.len() {
        0 => 0,
        1 => digits.parse::<i64>().ok()? * 100,
        2 => digits.parse::<i64>().ok()? * 10,
        _ => digits.parse::<i64>().ok()?,
    };
    Some(((h * 60 + m) * 60 + sec) * 1000 + ms)
}

/// Removes tags (`<i>`, `<c.yellow>`, `<00:01.000>`), ASS overrides (`{\an8}`),
/// speaker dashes and repeated whitespace.
fn clean_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut depth_angle = false;
    let mut depth_brace = false;
    for c in text.chars() {
        match c {
            '<' => depth_angle = true,
            '>' if depth_angle => depth_angle = false,
            '{' => depth_brace = true,
            '}' if depth_brace => depth_brace = false,
            _ if depth_angle || depth_brace => {}
            _ => out.push(c),
        }
    }
    let out = out
        .replace("\\N", " ")
        .replace("\\n", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ");
    out.split_whitespace()
        .filter(|w| *w != "-")
        .map(|w| w.trim_start_matches('-'))
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Groups cues into windows (see [`ChunkOptions`]). Identical consecutive cue texts
/// (karaoke-style repeats) are kept once.
pub fn chunk_cues(cues: &[Cue], options: ChunkOptions) -> Vec<TextChunk> {
    let mut chunks = Vec::new();
    let mut current: Option<TextChunk> = None;
    let mut last_text: Option<&str> = None;
    for cue in cues {
        if let Some(chunk) = &current {
            let span = chunk.end_ms.max(cue.end_ms) - chunk.start_ms;
            let gap = cue.start_ms - chunk.end_ms;
            let long_enough = chunk.end_ms - chunk.start_ms >= options.target_ms;
            let scene_break =
                gap > options.gap_ms && chunk.end_ms - chunk.start_ms >= options.min_ms;
            if long_enough || scene_break || span > options.max_ms {
                chunks.extend(current.take());
                last_text = None;
            }
        }
        if last_text == Some(cue.text.as_str()) {
            if let Some(chunk) = &mut current {
                chunk.end_ms = chunk.end_ms.max(cue.end_ms);
            }
            continue;
        }
        match &mut current {
            Some(chunk) => {
                chunk.end_ms = chunk.end_ms.max(cue.end_ms);
                chunk.text.push(' ');
                chunk.text.push_str(&cue.text);
            }
            None => {
                current = Some(TextChunk {
                    start_ms: cue.start_ms,
                    end_ms: cue.end_ms,
                    text: cue.text.clone(),
                });
            }
        }
        last_text = Some(cue.text.as_str());
    }
    chunks.extend(current);
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cue(start_s: i64, end_s: i64, text: &str) -> Cue {
        Cue {
            start_ms: start_s * 1000,
            end_ms: end_s * 1000,
            text: text.to_owned(),
        }
    }

    #[test]
    fn webvtt_and_srt_are_parsed_and_cleaned() {
        let vtt = "WEBVTT\n\nNOTE a comment\n\nintro\n00:00:01.500 --> 00:00:03.000 align:start\n<i>Hallo</i> {\\an8}Welt\n\n00:04.250 --> 00:05.000\n- Wer?\n- Ich &amp; du.\n\n00:06.000 --> 00:07.000\n\n";
        assert_eq!(
            parse_cues(vtt),
            vec![
                Cue {
                    start_ms: 1500,
                    end_ms: 3000,
                    text: "Hallo Welt".into()
                },
                Cue {
                    start_ms: 4250,
                    end_ms: 5000,
                    text: "Wer? Ich & du.".into()
                },
            ]
        );
        let srt = "\u{feff}1\r\n00:00:10,000 --> 00:00:12,5\r\nZeile eins\r\nZeile zwei\r\n\r\n2\r\n01:00:00,000 --> 01:00:01,000\r\n<font color=\"red\">Ende</font>\r\n";
        let cues = parse_cues(srt);
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].start_ms, 10_000);
        assert_eq!(cues[0].end_ms, 12_500);
        assert_eq!(cues[0].text, "Zeile eins Zeile zwei");
        assert_eq!(cues[1].start_ms, 3_600_000);
        assert_eq!(cues[1].text, "Ende");
    }

    #[test]
    fn broken_blocks_are_skipped() {
        let text = "1\nkein Zeitstempel\n\n2\n00:00:xx,000 --> 00:00:02,000\nKaputt\n\n3\n00:00:03,000 --> 00:00:04,000\nGut";
        let cues = parse_cues(text);
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].text, "Gut");
    }

    #[test]
    fn cues_become_windows_of_30_to_60_seconds() {
        // A cue every 5 s for 3 minutes.
        let cues: Vec<Cue> = (0..36)
            .map(|i| cue(i * 5, i * 5 + 4, &format!("Satz {i}")))
            .collect();
        let chunks = chunk_cues(&cues, ChunkOptions::default());
        assert!(chunks.len() >= 3 && chunks.len() <= 6, "{}", chunks.len());
        for chunk in &chunks[..chunks.len() - 1] {
            let span = chunk.end_ms - chunk.start_ms;
            assert!((30_000..=60_000).contains(&span), "span {span}");
        }
        // Nothing is lost and the order is kept.
        let all: Vec<String> = chunks.iter().map(|c| c.text.clone()).collect();
        assert!(all.join(" ").starts_with("Satz 0 Satz 1 Satz 2"));
        assert!(all.last().unwrap().ends_with("Satz 35"));
        for pair in chunks.windows(2) {
            assert!(pair[0].end_ms <= pair[1].start_ms);
        }
    }

    #[test]
    fn long_silences_start_a_new_window() {
        let cues = vec![
            cue(0, 10, "Am Strand."),
            cue(20, 32, "Streit."),
            cue(60, 62, "Später im Haus."),
        ];
        let chunks = chunk_cues(&cues, ChunkOptions::default());
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].text, "Am Strand. Streit.");
        assert_eq!((chunks[0].start_ms, chunks[0].end_ms), (0, 32_000));
        assert_eq!(chunks[1].start_ms, 60_000);
    }

    #[test]
    fn windows_never_exceed_the_maximum_and_repeats_are_dropped() {
        let cues = vec![
            cue(0, 20, "A"),
            cue(20, 25, "A"),
            cue(25, 55, "B"),
            cue(55, 70, "C"),
        ];
        let chunks = chunk_cues(&cues, ChunkOptions::default());
        assert_eq!(chunks[0].text, "A B");
        assert_eq!(chunks[1].text, "C");
        assert!(chunks.iter().all(|c| c.end_ms - c.start_ms <= 60_000));
        assert!(chunk_cues(&[], ChunkOptions::default()).is_empty());
    }
}
