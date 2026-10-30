//! Which guide programs a recording rule wants (pure functions).
//!
//! * A **program** rule records one broadcast: its channel and start time (program ids
//!   change with every guide refresh, the slot does not).
//! * A **series** rule records every broadcast whose title (normalized: case, accents
//!   of ASCII letters, punctuation and spacing do not matter) — or, when set, whose
//!   guide series id — matches; optionally only on one channel and only within a time
//!   window of the day (local time of the rule's UTC offset; windows may cross
//!   midnight).
//! * "Nur neue Folgen" skips episodes that were already recorded or are scheduled
//!   (same season/episode, else same episode title), and programs without episode
//!   information unless the guide marks them as new.

use std::collections::HashSet;

/// What a rule records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleKind {
    Program,
    Series,
}

impl RuleKind {
    pub fn as_str(self) -> &'static str {
        match self {
            RuleKind::Program => "program",
            RuleKind::Series => "series",
        }
    }

    pub fn parse(kind: &str) -> Option<Self> {
        match kind {
            "program" => Some(RuleKind::Program),
            "series" => Some(RuleKind::Series),
            _ => None,
        }
    }
}

/// The matching part of a rule.
#[derive(Debug, Clone, PartialEq)]
pub struct RuleFilter {
    pub kind: RuleKind,
    pub title: String,
    pub series_id: Option<String>,
    /// Program rules: the channel; series rules: an optional channel filter.
    pub channel_id: Option<i64>,
    /// Program rules: the broadcast's start.
    pub program_start_ms: Option<i64>,
    pub new_only: bool,
    /// Minutes after local midnight `[start, end)`; `None` = any time.
    pub window: Option<(i64, i64)>,
    /// Local time = UTC + this many minutes.
    pub utc_offset_min: i64,
}

/// A guide program as the matcher sees it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Candidate {
    pub channel_id: i64,
    pub start_ms: i64,
    pub end_ms: i64,
    pub title: String,
    pub subtitle: Option<String>,
    pub season: Option<i64>,
    pub episode: Option<i64>,
    pub series_id: Option<String>,
    pub is_new: bool,
}

/// Title identity: lower-case letters and digits, single spaces (umlauts kept).
pub fn normalize_title(title: &str) -> String {
    let mut out = String::with_capacity(title.len());
    let mut space = false;
    for c in title.chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            if space && !out.is_empty() {
                out.push(' ');
            }
            space = false;
            out.push(c);
        } else {
            space = true;
        }
    }
    out
}

/// Minutes after local midnight of a time.
pub fn local_minute(ms: i64, utc_offset_min: i64) -> i64 {
    (ms.div_euclid(60_000) + utc_offset_min).rem_euclid(1440)
}

fn in_window(minute: i64, (start, end): (i64, i64)) -> bool {
    if start <= end {
        (start..end).contains(&minute)
    } else {
        // Across midnight, e.g. 22:00–02:00.
        minute >= start || minute < end
    }
}

/// Whether the rule wants this program (before "only new episodes").
pub fn matches(rule: &RuleFilter, program: &Candidate) -> bool {
    match rule.kind {
        RuleKind::Program => {
            rule.channel_id == Some(program.channel_id)
                && rule.program_start_ms == Some(program.start_ms)
        }
        RuleKind::Series => {
            if rule.channel_id.is_some_and(|c| c != program.channel_id) {
                return false;
            }
            let identity = match (&rule.series_id, &program.series_id) {
                (Some(wanted), Some(id)) => wanted == id,
                // A series id rule never matches by title alone.
                (Some(_), None) => false,
                _ => normalize_title(&rule.title) == normalize_title(&program.title),
            };
            identity
                && rule.window.is_none_or(|window| {
                    in_window(local_minute(program.start_ms, rule.utc_offset_min), window)
                })
        }
    }
}

/// Which episode a program is, for "only new episodes" (`None`: unknown).
pub fn episode_key(
    season: Option<i64>,
    episode: Option<i64>,
    subtitle: Option<&str>,
) -> Option<String> {
    match (season, episode) {
        (Some(s), Some(e)) => Some(format!("s{s}e{e}")),
        (None, Some(e)) => Some(format!("e{e}")),
        _ => subtitle
            .map(normalize_title)
            .filter(|s| !s.is_empty())
            .map(|s| format!("t:{s}")),
    }
}

/// The programs a rule records, in start order: matching ones, and with "only new
/// episodes" each episode once and none in `known` (episodes recorded or scheduled
/// already).
pub fn select<'a>(
    rule: &RuleFilter,
    programs: &'a [Candidate],
    known: &HashSet<String>,
) -> Vec<&'a Candidate> {
    let mut wanted: Vec<&Candidate> = programs.iter().filter(|p| matches(rule, p)).collect();
    wanted.sort_by_key(|p| (p.start_ms, p.channel_id));
    if !rule.new_only || rule.kind == RuleKind::Program {
        return wanted;
    }
    let mut seen = known.clone();
    wanted
        .into_iter()
        .filter(
            |p| match episode_key(p.season, p.episode, p.subtitle.as_deref()) {
                Some(key) => seen.insert(key),
                None => p.is_new,
            },
        )
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: i64 = 3_600_000;
    /// 2026-10-30 00:00 UTC.
    const DAY: i64 = 1_793_318_400_000;

    fn series(title: &str) -> RuleFilter {
        RuleFilter {
            kind: RuleKind::Series,
            title: title.into(),
            series_id: None,
            channel_id: None,
            program_start_ms: None,
            new_only: false,
            window: None,
            utc_offset_min: 60,
        }
    }

    fn program(channel: i64, start: i64, title: &str) -> Candidate {
        Candidate {
            channel_id: channel,
            start_ms: start,
            end_ms: start + HOUR,
            title: title.into(),
            ..Candidate::default()
        }
    }

    #[test]
    fn titles_match_regardless_of_case_and_punctuation() {
        assert_eq!(normalize_title("  Tatort:  Münster!"), "tatort münster");
        let rule = series("tatort - münster");
        assert!(matches(&rule, &program(1, DAY, "TATORT: Münster")));
        assert!(!matches(&rule, &program(1, DAY, "Tatort")));
    }

    #[test]
    fn program_rules_match_their_slot_only() {
        let rule = RuleFilter {
            kind: RuleKind::Program,
            channel_id: Some(3),
            program_start_ms: Some(DAY + 20 * HOUR),
            ..series("egal")
        };
        assert!(matches(
            &rule,
            &program(3, DAY + 20 * HOUR, "Anderer Titel")
        ));
        assert!(!matches(&rule, &program(4, DAY + 20 * HOUR, "egal")));
        assert!(!matches(&rule, &program(3, DAY + 21 * HOUR, "egal")));
    }

    #[test]
    fn channel_filter_series_id_and_time_window() {
        let mut rule = series("Die Sendung mit der Maus");
        rule.channel_id = Some(2);
        assert!(matches(&rule, &program(2, DAY, "Die Sendung mit der Maus")));
        assert!(!matches(
            &rule,
            &program(5, DAY, "Die Sendung mit der Maus")
        ));

        let mut by_id = series("Maus");
        by_id.series_id = Some("SH0001".into());
        let mut p = program(1, DAY, "Die Sendung mit der Maus (Wdh.)");
        assert!(!matches(&by_id, &p), "no series id on the program");
        p.series_id = Some("SH0001".into());
        assert!(matches(&by_id, &p), "the id wins over the title");

        // 20:00–22:00 local (UTC+1): 19:00 UTC is 20:00 local.
        let mut evening = series("Tatort");
        evening.window = Some((20 * 60, 22 * 60));
        assert!(matches(&evening, &program(1, DAY + 19 * HOUR, "Tatort")));
        assert!(!matches(&evening, &program(1, DAY + 21 * HOUR, "Tatort")));
        assert!(!matches(&evening, &program(1, DAY + 18 * HOUR, "Tatort")));
        // Across midnight: 23:00–01:00 local.
        let mut night = series("Tatort");
        night.window = Some((23 * 60, 60));
        assert!(matches(&night, &program(1, DAY + 22 * HOUR, "Tatort")));
        assert!(matches(
            &night,
            &program(1, DAY + 23 * HOUR + 30 * 60_000, "Tatort")
        ));
        assert!(!matches(&night, &program(1, DAY, "Tatort")), "01:00 local");
    }

    #[test]
    fn only_new_episodes_skips_known_and_repeated_ones() {
        let mut rule = series("Serie");
        rule.new_only = true;
        let episode = |start: i64, season, episode, is_new| Candidate {
            season,
            episode,
            is_new,
            ..program(1, start, "Serie")
        };
        let programs = [
            episode(DAY + 4 * HOUR, Some(1), Some(3), false), // first airing of 3
            episode(DAY + HOUR, Some(1), Some(1), false),     // recorded already
            episode(DAY + 2 * HOUR, Some(1), Some(2), false),
            episode(DAY + 3 * HOUR, None, None, false), // unknown, not new
            episode(DAY + 5 * HOUR, None, None, true),  // unknown, marked new
            Candidate {
                subtitle: Some("Der Fall".into()),
                ..episode(DAY + 6 * HOUR, None, None, false)
            },
            Candidate {
                subtitle: Some("der fall!".into()),
                ..episode(DAY + 7 * HOUR, None, None, true)
            },
        ];
        let known: HashSet<String> = ["s1e1".to_owned()].into();
        let starts: Vec<i64> = select(&rule, &programs, &known)
            .iter()
            .map(|p| (p.start_ms - DAY) / HOUR)
            .collect();
        assert_eq!(starts, [2, 4, 5, 6]);

        rule.new_only = false;
        assert_eq!(select(&rule, &programs, &known).len(), programs.len());
    }

    #[test]
    fn episode_keys() {
        assert_eq!(episode_key(Some(2), Some(5), None).as_deref(), Some("s2e5"));
        assert_eq!(episode_key(None, Some(5), Some("x")).as_deref(), Some("e5"));
        assert_eq!(
            episode_key(None, None, Some("Folge: Eins")).as_deref(),
            Some("t:folge eins")
        );
        assert_eq!(episode_key(None, None, Some(" - ")), None);
    }
}
