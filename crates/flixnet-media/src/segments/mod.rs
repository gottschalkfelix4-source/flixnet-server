//! Intro and credits detection for the episodes of a season.
//!
//! Episodes of one season share their intro music and their end credits. Each
//! episode's audio is fingerprinted with Chromaprint (ffmpeg's `chromaprint` muxer,
//! available in jellyfin-ffmpeg, Debian/Ubuntu's ffmpeg and the gyan.dev builds) in
//! two windows: the opening (intro) and the end (credits). The fingerprints of
//! neighbouring episodes are compared to find the longest shared stretch, and the
//! edges are snapped to black frames/silence close by (`blackdetect`,
//! `silencedetect`). Without Chromaprint the detection is switched off, not faked.
//!
//! Why not an own fingerprint from PCM: Chromaprint is in every ffmpeg Flixnet runs
//! with, is tuned for exactly this (robust to re-encoding and loudness changes) and
//! costs one ffmpeg run per window; a home-grown spectral hash was tried on the
//! synthetic seasons and was far less discriminative.

mod analyzer;
pub mod matching;

pub use analyzer::{AnalysisError, SegmentAnalyzer, fingerprint_args, gap_args};
pub use matching::{
    Detection, Fingerprint, Gap, ITEM_MS, MatchConfig, PairMatch, Span, detect_common, match_pair,
    refine,
};

/// Which part of an episode a fingerprint covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Region {
    /// The opening, where intros are: the first 10 min, at most half the episode.
    Head,
    /// The end, where credits are: the last 5 min, at most a third of the episode.
    Tail,
}

impl Region {
    pub fn as_str(self) -> &'static str {
        match self {
            Region::Head => "head",
            Region::Tail => "tail",
        }
    }

    /// `(start_ms, duration_ms)` of the window in a file of `duration_ms`.
    pub fn window(self, duration_ms: i64) -> (i64, i64) {
        match self {
            Region::Head => (0, (duration_ms / 2).min(600_000)),
            Region::Tail => {
                let length = (duration_ms / 3).min(300_000);
                (duration_ms - length, length)
            }
        }
    }
}

/// How close (ms) a black/silent gap must be to a fingerprint edge to move it.
pub const SNAP_TOLERANCE_MS: i64 = 2_000;

/// Credits ending this close (ms) to the end of the file run to the end.
pub const CREDITS_END_SLACK_MS: i64 = 5_000;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows() {
        assert_eq!(Region::Head.window(2_700_000), (0, 600_000));
        assert_eq!(Region::Head.window(150_000), (0, 75_000));
        assert_eq!(Region::Tail.window(2_700_000), (2_400_000, 300_000));
        assert_eq!(Region::Tail.window(150_000), (100_000, 50_000));
    }
}
