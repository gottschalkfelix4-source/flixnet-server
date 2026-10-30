//! Intro and credits detection on a synthetic season (ffmpeg with Chromaprint on PATH;
//! skipped, not failed, when the build lacks the muxer).
//!
//! Every episode has its own cold open and body (random tone sequences) but the same
//! 36 s intro, at a different, non-round offset, and the same 20 s credits after a
//! 1 s black, silent fade. The done criterion of milestone 5.1: the intro is found to
//! within ±1 s.

use std::path::{Path, PathBuf};
use std::process::Command;

use flixnet_media::segments::{
    CREDITS_END_SLACK_MS, Fingerprint, MatchConfig, Region, SegmentAnalyzer, Span, detect_common,
};
use flixnet_media::{Ffprobe, StreamKind};

const INTRO_S: f64 = 36.0;
const BODY_S: f64 = 80.0;
const FADE_S: f64 = 1.0;
const CREDITS_S: f64 = 20.0;

/// The intro tune: two voices stepping through a scale.
const INTRO_AUDIO: &str = "aevalsrc=0.3*sin(2*PI*t*220*pow(2\\,mod(floor(t/0.75)*7\\,12)/12))\
    +0.2*sin(2*PI*t*330*pow(2\\,mod(floor(t/0.5)*5\\,12)/12))";
const CREDITS_AUDIO: &str = "aevalsrc=0.3*sin(2*PI*t*196*pow(2\\,mod(floor(t/0.6)*3\\,12)/12))";

/// Random tones (three per second) that differ per `seed`.
fn own_audio(seed: u32) -> String {
    let hash = format!("sin(floor(t*3)*12.9898+{seed}*78.233)*43758.5453");
    format!("aevalsrc=0.25*sin(2*PI*t*(150+700*({hash}-floor({hash}))))")
}

struct Episode {
    path: PathBuf,
    intro_start_s: f64,
    /// Where the fade before the credits begins.
    credits_start_s: f64,
}

fn episode(dir: &Path, number: u32, cold_open_s: f64) -> Episode {
    let path = dir.join(format!("Serie.S01E0{number}.mkv"));
    let seed = 1000 + number * 37;
    let parts = [
        ("testsrc2", own_audio(seed), cold_open_s),
        ("smptehdbars", INTRO_AUDIO.to_owned(), INTRO_S),
        ("testsrc", own_audio(seed + 500), BODY_S),
        (
            "color=c=black",
            "anullsrc=r=48000:cl=mono".to_owned(),
            FADE_S,
        ),
        ("rgbtestsrc", CREDITS_AUDIO.to_owned(), CREDITS_S),
    ];
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-hide_banner", "-loglevel", "error", "-y"]);
    let mut inputs = String::new();
    for (i, (video, audio, seconds)) in parts.iter().enumerate() {
        let sep = if video.contains('=') { ':' } else { '=' };
        cmd.args([
            "-f",
            "lavfi",
            "-i",
            &format!("{video}{sep}s=320x180:r=10:d={seconds}"),
        ]);
        let sep = if audio.starts_with("anullsrc") {
            ":"
        } else {
            ":s=48000:"
        };
        cmd.args(["-f", "lavfi", "-i", &format!("{audio}{sep}d={seconds}")]);
        inputs.push_str(&format!("[{}:v][{}:a]", 2 * i, 2 * i + 1));
    }
    let filter = format!("{inputs}concat=n={}:v=1:a=1[v][a]", parts.len());
    cmd.args(["-filter_complex", &filter, "-map", "[v]", "-map", "[a]"]);
    cmd.args([
        "-c:v",
        "libx264",
        "-preset",
        "ultrafast",
        "-c:a",
        "aac",
        "-b:a",
        "128k",
    ]);
    let status = cmd.arg(&path).status().expect("ffmpeg on PATH");
    assert!(status.success(), "generating episode {number}");
    Episode {
        path,
        intro_start_s: cold_open_s,
        credits_start_s: cold_open_s + INTRO_S + BODY_S,
    }
}

struct Probed {
    duration_ms: i64,
    video: Option<i64>,
    audio: i64,
}

async fn probe(path: &Path) -> Probed {
    let info = Ffprobe::new("ffprobe").probe(path).await.unwrap();
    let index = |kind| {
        info.streams
            .iter()
            .find(|s| s.kind == kind)
            .map(|s| s.index)
    };
    Probed {
        duration_ms: info.duration_ms.unwrap(),
        video: index(StreamKind::Video),
        audio: index(StreamKind::Audio).unwrap(),
    }
}

async fn fingerprint(
    analyzer: &SegmentAnalyzer,
    episode: &Episode,
    probed: &Probed,
    region: Region,
) -> Fingerprint {
    let (start, length) = region.window(probed.duration_ms);
    analyzer
        .fingerprint(&episode.path, probed.audio, start, length)
        .await
        .unwrap()
}

fn error_s(found_ms: i64, expected_s: f64) -> f64 {
    found_ms as f64 / 1000.0 - expected_s
}

#[tokio::test]
async fn intro_and_credits_of_a_synthetic_season_are_found_to_the_second() {
    let analyzer = SegmentAnalyzer::new("ffmpeg");
    if !analyzer.chromaprint_available().await {
        eprintln!("skipped: ffmpeg was built without Chromaprint");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    // Cold opens of different, non-round lengths: the intro moves around.
    let episodes: Vec<Episode> = [12.03, 27.46, 41.81, 6.37]
        .iter()
        .enumerate()
        .map(|(i, cold_open)| episode(dir.path(), i as u32 + 1, *cold_open))
        .collect();
    let mut probed = Vec::new();
    let mut heads = Vec::new();
    let mut tails = Vec::new();
    for episode in &episodes {
        let p = probe(&episode.path).await;
        heads.push(fingerprint(&analyzer, episode, &p, Region::Head).await);
        tails.push(fingerprint(&analyzer, episode, &p, Region::Tail).await);
        probed.push(p);
    }

    let intros = detect_common(
        &heads.iter().map(Some).collect::<Vec<_>>(),
        &MatchConfig::INTRO,
    );
    let credits = detect_common(
        &tails.iter().map(Some).collect::<Vec<_>>(),
        &MatchConfig::CREDITS,
    );
    let mut worst: f64 = 0.0;
    for (i, episode) in episodes.iter().enumerate() {
        let p = &probed[i];
        let intro = intros[i].unwrap_or_else(|| panic!("no intro in episode {}", i + 1));
        let refined = analyzer
            .refine_edges(&episode.path, p.video, Some(p.audio), intro.span)
            .await
            .unwrap();
        let (start, end) = (
            error_s(refined.start_ms, episode.intro_start_s),
            error_s(refined.end_ms, episode.intro_start_s + INTRO_S),
        );
        println!(
            "E{:02} intro {:.2}–{:.2} s (expected {:.2}–{:.2}): start {start:+.2} s, end {end:+.2} s, confidence {:.2}",
            i + 1,
            refined.start_ms as f64 / 1000.0,
            refined.end_ms as f64 / 1000.0,
            episode.intro_start_s,
            episode.intro_start_s + INTRO_S,
            intro.confidence,
        );
        worst = worst.max(start.abs()).max(end.abs());
        assert_eq!(intro.confidence, 1.0);

        let found = credits[i].unwrap_or_else(|| panic!("no credits in episode {}", i + 1));
        let mut span = analyzer
            .refine_edges(&episode.path, p.video, Some(p.audio), found.span)
            .await
            .unwrap();
        if p.duration_ms - span.end_ms <= CREDITS_END_SLACK_MS {
            span = Span {
                end_ms: p.duration_ms,
                ..span
            };
        }
        let start = error_s(span.start_ms, episode.credits_start_s);
        println!(
            "E{:02} credits from {:.2} s (fade starts at {:.2} s): {start:+.2} s",
            i + 1,
            span.start_ms as f64 / 1000.0,
            episode.credits_start_s,
        );
        assert!(start.abs() <= 1.0, "credits start off by {start:.2} s");
        assert_eq!(span.end_ms, p.duration_ms);
    }
    println!("largest intro edge error: {worst:.2} s");
    assert!(worst <= 1.0, "intro edges off by up to {worst:.2} s");
}

#[tokio::test]
async fn files_without_shared_audio_have_no_intro() {
    let analyzer = SegmentAnalyzer::new("ffmpeg");
    if !analyzer.chromaprint_available().await {
        eprintln!("skipped: ffmpeg was built without Chromaprint");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let mut prints = Vec::new();
    for seed in [1u32, 2] {
        let path = dir.path().join(format!("{seed}.mka"));
        let status = Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
            ])
            .arg(format!("{}:s=48000:d=90", own_audio(seed)))
            .args(["-c:a", "aac"])
            .arg(&path)
            .status()
            .unwrap();
        assert!(status.success());
        prints.push(analyzer.fingerprint(&path, 0, 0, 90_000).await.unwrap());
    }
    assert!(
        prints[0].items.len() > 600,
        "{} items",
        prints[0].items.len()
    );
    let found = detect_common(&[Some(&prints[0]), Some(&prints[1])], &MatchConfig::INTRO);
    assert_eq!(found, vec![None, None]);
}
