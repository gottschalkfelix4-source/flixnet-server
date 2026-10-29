//! Finding the audio that episodes of a season share (pure functions, no I/O).
//!
//! Two fingerprints are compared at every relative shift. At each shift the items of
//! both sides that differ in at most a few bits "match"; the longest run of matches
//! (short gaps allowed) is the candidate. Runs over stationary audio (silence, a
//! constant hum) are rejected because their items barely vary. Each episode is
//! compared with up to four neighbours; the candidates that agree with each other
//! decide its segment (median edges), so one bad pairing does not move the result.

use std::collections::HashSet;

/// Duration of one Chromaprint item: a hop of 1365 samples at 11 025 Hz (≈ 123.8 ms).
pub const ITEM_MS: f64 = 1365.0 * 1000.0 / 11025.0;

/// Each item summarizes the audio from its own timestamp to roughly 1.3 s later
/// (4096-sample frames plus Chromaprint's filters over up to 16 frames). The first
/// matching item therefore sits slightly before the shared audio starts, and the
/// last one about a window's length before it ends. Measured on synthetic seasons
/// with tonal, noise and chord intros (see `tests/segments.rs`).
const START_CORRECTION_MS: f64 = 300.0;
const END_CORRECTION_MS: f64 = 1300.0;

/// Chromaprint items of one stretch of audio: item `i` starts at
/// `start_ms + i * ITEM_MS`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Fingerprint {
    pub start_ms: i64,
    pub items: Vec<u32>,
}

impl Fingerprint {
    pub fn new(start_ms: i64, items: Vec<u32>) -> Self {
        Self { start_ms, items }
    }

    fn time_ms(&self, index: usize, correction: f64) -> i64 {
        (self.start_ms as f64 + index as f64 * ITEM_MS + correction).round() as i64
    }
}

/// A stretch of time within a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start_ms: i64,
    pub end_ms: i64,
}

impl Span {
    pub fn duration_ms(&self) -> i64 {
        self.end_ms - self.start_ms
    }
}

/// Tuning of one detection (intro or credits).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MatchConfig {
    /// Items match when they differ in at most this many of their 32 bits.
    pub max_bit_errors: u32,
    /// Non-matching items tolerated inside a run.
    pub max_gap: usize,
    /// Edges are trimmed until `edge_hits` of `edge_window` items match.
    pub edge_window: usize,
    pub edge_hits: usize,
    pub min_ms: i64,
    pub max_ms: i64,
}

impl MatchConfig {
    /// Intros: 15 s to 2.5 min of shared audio near the start.
    pub const INTRO: Self = Self {
        max_bit_errors: 6,
        max_gap: 8,
        edge_window: 8,
        edge_hits: 6,
        min_ms: 15_000,
        max_ms: 150_000,
    };
    /// Credits: at least 10 s of shared audio near the end.
    pub const CREDITS: Self = Self {
        min_ms: 10_000,
        max_ms: 300_000,
        ..Self::INTRO
    };

    fn min_items(&self) -> usize {
        (self.min_ms as f64 / ITEM_MS).ceil() as usize
    }

    fn max_items(&self) -> usize {
        (self.max_ms as f64 / ITEM_MS).floor() as usize
    }
}

/// The audio two fingerprints share: where it is in each of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PairMatch {
    pub a: Span,
    pub b: Span,
}

/// Longest stretch of shared audio of two fingerprints within the configured length.
pub fn match_pair(a: &Fingerprint, b: &Fingerprint, config: &MatchConfig) -> Option<PairMatch> {
    let (n, m) = (a.items.len(), b.items.len());
    let min_items = config.min_items().max(config.edge_window);
    let max_items = config.max_items();
    if n < min_items || m < min_items {
        return None;
    }
    // (length, start in a, end in a (inclusive), shift)
    let mut best: Option<(usize, usize, usize, isize)> = None;
    let mut hits = Vec::with_capacity(n.max(m));
    for shift in -(n as isize - 1)..m as isize {
        let first = (-shift).max(0) as usize;
        let last = n.min((m as isize - shift) as usize);
        if last < first + min_items {
            continue;
        }
        hits.clear();
        hits.extend((first..last).map(|i| {
            let j = (i as isize + shift) as usize;
            (a.items[i] ^ b.items[j]).count_ones() <= config.max_bit_errors
        }));
        if hits.iter().filter(|h| **h).count() < min_items * config.edge_hits / config.edge_window {
            continue;
        }
        for (start, end) in runs(&hits, config.max_gap) {
            let Some((start, end)) = trim(&hits, start, end, config) else {
                continue;
            };
            let len = end - start + 1;
            if len < min_items || len > max_items || best.is_some_and(|b| b.0 >= len) {
                continue;
            }
            if !varied(&a.items[first + start..=first + end]) {
                continue;
            }
            best = Some((len, first + start, first + end, shift));
        }
    }
    let (_, start, end, shift) = best?;
    let other = |i: usize| (i as isize + shift) as usize;
    Some(PairMatch {
        a: Span {
            start_ms: a.time_ms(start, START_CORRECTION_MS),
            end_ms: a.time_ms(end + 1, END_CORRECTION_MS),
        },
        b: Span {
            start_ms: b.time_ms(other(start), START_CORRECTION_MS),
            end_ms: b.time_ms(other(end) + 1, END_CORRECTION_MS),
        },
    })
}

/// Runs of `true` (inclusive index ranges), bridging gaps of up to `max_gap` misses.
fn runs(hits: &[bool], max_gap: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut current: Option<(usize, usize)> = None;
    for (i, hit) in hits.iter().enumerate() {
        if !hit {
            continue;
        }
        current = match current {
            Some((start, end)) if i - end <= max_gap + 1 => Some((start, i)),
            Some(run) => {
                out.push(run);
                Some((i, i))
            }
            None => Some((i, i)),
        };
    }
    out.extend(current);
    out
}

/// Moves both edges inwards until the items next to them match densely, so chance
/// matches bridged by the gap tolerance do not stretch the run.
fn trim(
    hits: &[bool],
    mut start: usize,
    mut end: usize,
    config: &MatchConfig,
) -> Option<(usize, usize)> {
    let window = config.edge_window;
    let dense =
        |from: usize| hits[from..from + window].iter().filter(|h| **h).count() >= config.edge_hits;
    while end + 1 >= start + window && !(hits[start] && dense(start)) {
        start += 1;
    }
    while end + 1 >= start + window && !(hits[end] && dense(end + 1 - window)) {
        end -= 1;
    }
    (end + 1 >= start + window).then_some((start, end))
}

/// Whether a run carries information: silence and stationary noise produce nearly
/// constant items that "match" anything of the same kind.
fn varied(items: &[u32]) -> bool {
    let distinct: HashSet<u32> = items.iter().copied().collect();
    distinct.len() * 4 >= items.len()
}

/// The segment found for one file, with the share of comparisons that agree.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Detection {
    pub span: Span,
    pub confidence: f64,
}

/// Candidates whose starts lie this close together describe the same segment.
const AGREEMENT_MS: i64 = 3_000;

/// Shared audio of the files of one season, in episode order (`None`: no usable
/// fingerprint). Each file is compared with the next two; returns one detection per
/// input, `None` where nothing was found.
pub fn detect_common(
    prints: &[Option<&Fingerprint>],
    config: &MatchConfig,
) -> Vec<Option<Detection>> {
    let mut candidates: Vec<Vec<Span>> = vec![Vec::new(); prints.len()];
    let mut comparisons = vec![0usize; prints.len()];
    for i in 0..prints.len() {
        for j in (i + 1)..prints.len().min(i + 3) {
            let (Some(a), Some(b)) = (prints[i], prints[j]) else {
                continue;
            };
            comparisons[i] += 1;
            comparisons[j] += 1;
            if let Some(found) = match_pair(a, b, config) {
                candidates[i].push(found.a);
                candidates[j].push(found.b);
            }
        }
    }
    candidates
        .iter()
        .zip(comparisons)
        .map(|(spans, compared)| consensus(spans, compared))
        .collect()
}

/// The largest group of agreeing candidates (ties: the longer segment), as medians.
fn consensus(spans: &[Span], compared: usize) -> Option<Detection> {
    let group = spans
        .iter()
        .map(|anchor| {
            spans
                .iter()
                .copied()
                .filter(|s| (s.start_ms - anchor.start_ms).abs() <= AGREEMENT_MS)
                .collect::<Vec<_>>()
        })
        .max_by_key(|group| (group.len(), median(group.iter().map(Span::duration_ms))))?;
    Some(Detection {
        span: Span {
            start_ms: median(group.iter().map(|s| s.start_ms)),
            end_ms: median(group.iter().map(|s| s.end_ms)),
        },
        confidence: group.len() as f64 / compared.max(1) as f64,
    })
}

fn median(values: impl Iterator<Item = i64>) -> i64 {
    let mut values: Vec<i64> = values.collect();
    values.sort_unstable();
    match values.len() {
        0 => 0,
        n if n % 2 == 1 => values[n / 2],
        n => (values[n / 2 - 1] + values[n / 2]) / 2,
    }
}

/// A stretch of black picture and/or silence (from `blackdetect`/`silencedetect`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gap {
    pub start_ms: i64,
    pub end_ms: i64,
}

/// Snaps fingerprint edges to nearby black/silent gaps: a segment starts where the
/// gap before it starts and ends where the gap after it ends, so skipping also skips
/// the fade. Edges without a gap within `tolerance_ms` stay where they are.
pub fn refine(span: Span, gaps: &[Gap], tolerance_ms: i64) -> Span {
    let nearest = |edge: i64, pick: fn(&Gap) -> i64| {
        gaps.iter()
            .filter(|g| g.start_ms - tolerance_ms <= edge && edge <= g.end_ms + tolerance_ms)
            .min_by_key(|g| (pick(g) - edge).abs())
            .map(pick)
    };
    let start_ms = nearest(span.start_ms, |g| g.start_ms).unwrap_or(span.start_ms);
    let end_ms = nearest(span.end_ms, |g| g.end_ms).unwrap_or(span.end_ms);
    if end_ms > start_ms {
        Span { start_ms, end_ms }
    } else {
        span
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic pseudo-random items (xorshift), different per seed.
    fn noise(seed: u32, len: usize) -> Vec<u32> {
        let mut state = seed.wrapping_mul(2_654_435_761).max(1);
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                state
            })
            .collect()
    }

    fn items(seconds: f64) -> usize {
        (seconds * 1000.0 / ITEM_MS).round() as usize
    }

    /// Episode: `pre` s of own audio, the shared intro, then `post` s of own audio.
    fn episode(seed: u32, pre: f64, intro: &[u32], post: f64) -> Fingerprint {
        let mut out = noise(seed, items(pre));
        out.extend_from_slice(intro);
        out.extend(noise(seed + 1000, items(post)));
        Fingerprint::new(0, out)
    }

    fn intro_at(pre: f64) -> f64 {
        items(pre) as f64 * ITEM_MS
    }

    #[test]
    fn finds_a_shared_run_at_different_offsets() {
        let intro = noise(7, items(40.0));
        let a = episode(1, 30.0, &intro, 100.0);
        let b = episode(2, 75.0, &intro, 60.0);
        let found = match_pair(&a, &b, &MatchConfig::INTRO).expect("match");
        let expect = |pre: f64, span: Span| {
            let start = intro_at(pre);
            assert!(
                (span.start_ms as f64 - start - START_CORRECTION_MS).abs() < 1.0,
                "{span:?}"
            );
            let end = start + intro.len() as f64 * ITEM_MS + END_CORRECTION_MS;
            assert!((span.end_ms as f64 - end).abs() < 1.0, "{span:?}");
        };
        expect(30.0, found.a);
        expect(75.0, found.b);
    }

    #[test]
    fn tolerates_bit_errors_and_short_dropouts() {
        let intro = noise(7, items(40.0));
        let a = episode(1, 20.0, &intro, 60.0);
        let mut noisy = intro.clone();
        for (i, item) in noisy.iter_mut().enumerate() {
            *item ^= 0b101 << (i % 20); // two flipped bits everywhere
        }
        for item in &mut noisy[100..104] {
            *item = !*item; // a short dropout
        }
        let b = episode(2, 45.0, &noisy, 60.0);
        let found = match_pair(&a, &b, &MatchConfig::INTRO).expect("match");
        assert!((found.a.duration_ms() - 41_000).abs() < 300, "{found:?}");
    }

    #[test]
    fn short_or_missing_overlaps_are_ignored() {
        let short = noise(7, items(10.0));
        let a = episode(1, 20.0, &short, 60.0);
        let b = episode(2, 45.0, &short, 60.0);
        assert_eq!(match_pair(&a, &b, &MatchConfig::INTRO), None);
        let unrelated = Fingerprint::new(0, noise(99, items(120.0)));
        assert_eq!(match_pair(&a, &unrelated, &MatchConfig::INTRO), None);
        assert_eq!(
            match_pair(&a, &Fingerprint::default(), &MatchConfig::INTRO),
            None
        );
    }

    #[test]
    fn constant_audio_is_not_an_intro() {
        // Long silence (constant items) in both files.
        let silence = vec![0x5555_0000; items(40.0)];
        let a = episode(1, 20.0, &silence, 60.0);
        let b = episode(2, 45.0, &silence, 60.0);
        assert_eq!(match_pair(&a, &b, &MatchConfig::INTRO), None);
    }

    #[test]
    fn longer_than_allowed_is_rejected() {
        let long = noise(7, items(200.0));
        let a = episode(1, 20.0, &long, 30.0);
        let b = episode(2, 45.0, &long, 30.0);
        assert_eq!(match_pair(&a, &b, &MatchConfig::INTRO), None);
        assert!(match_pair(&a, &b, &MatchConfig::CREDITS).is_some());
    }

    #[test]
    fn tail_fingerprints_report_absolute_times() {
        let credits = noise(5, items(30.0));
        let mut a = episode(1, 50.0, &credits, 2.0);
        a.start_ms = 1_200_000;
        let mut b = episode(2, 80.0, &credits, 2.0);
        b.start_ms = 1_300_000;
        let found = match_pair(&a, &b, &MatchConfig::CREDITS).expect("match");
        assert!((found.a.start_ms - (1_200_000 + intro_at(50.0) as i64 + 300)).abs() <= 1);
        assert!((found.b.start_ms - (1_300_000 + intro_at(80.0) as i64 + 300)).abs() <= 1);
    }

    #[test]
    fn season_consensus_outvotes_a_bad_pair() {
        let intro = noise(7, items(35.0));
        let pres = [10.0, 25.0, 40.0, 5.0];
        let prints: Vec<Fingerprint> = pres
            .iter()
            .enumerate()
            .map(|(i, pre)| episode(i as u32 + 1, *pre, &intro, 80.0))
            .collect();
        let refs: Vec<Option<&Fingerprint>> = prints.iter().map(Some).collect();
        let found = detect_common(&refs, &MatchConfig::INTRO);
        for (detection, pre) in found.iter().zip(pres) {
            let detection = detection.expect("intro found");
            assert!(
                (detection.span.start_ms as f64 - intro_at(pre) - START_CORRECTION_MS).abs() < 1.0
            );
            assert_eq!(detection.confidence, 1.0);
        }

        // An episode without the intro gets nothing; the others still agree.
        let other = Fingerprint::new(0, noise(77, items(120.0)));
        let refs = vec![Some(&prints[0]), Some(&other), Some(&prints[1]), None];
        let found = detect_common(&refs, &MatchConfig::INTRO);
        assert!(found[0].is_some() && found[2].is_some());
        assert_eq!(found[1], None);
        assert_eq!(found[3], None);
        assert!(found[0].unwrap().confidence < 1.0);
    }

    #[test]
    fn consensus_prefers_the_majority() {
        let s = |start_ms, end_ms| Span { start_ms, end_ms };
        let found = consensus(
            &[s(10_000, 50_000), s(10_400, 50_200), s(80_000, 120_000)],
            3,
        )
        .unwrap();
        assert_eq!(found.span, s(10_200, 50_100));
        assert!((found.confidence - 2.0 / 3.0).abs() < 1e-9);
        assert_eq!(consensus(&[], 2), None);
    }

    #[test]
    fn edges_snap_to_nearby_gaps() {
        let span = Span {
            start_ms: 60_400,
            end_ms: 100_300,
        };
        let gaps = [
            Gap {
                start_ms: 59_000,
                end_ms: 60_000,
            }, // black before the intro
            Gap {
                start_ms: 100_800,
                end_ms: 101_500,
            }, // fade after it
            Gap {
                start_ms: 30_000,
                end_ms: 31_000,
            }, // far away
        ];
        assert_eq!(
            refine(span, &gaps, 2_000),
            Span {
                start_ms: 59_000,
                end_ms: 101_500
            }
        );
        assert_eq!(refine(span, &gaps[2..], 2_000), span);
        assert_eq!(refine(span, &[], 2_000), span);
    }

    #[test]
    fn run_detection_bridges_small_gaps() {
        let hits: Vec<bool> = "11110011111000000000111"
            .chars()
            .map(|c| c == '1')
            .collect();
        assert_eq!(runs(&hits, 2), vec![(0, 10), (20, 22)]);
        assert_eq!(runs(&hits, 1), vec![(0, 3), (6, 10), (20, 22)]);
        assert_eq!(runs(&[], 2), vec![]);
    }
}
