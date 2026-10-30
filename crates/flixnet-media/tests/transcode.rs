//! TranscodeManager against the real ffmpeg (must be on PATH, as in CI).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use flixnet_media::decision::{AudioPlan, PlayMethod, PlaybackPlan, StreamAction, VideoPlan};
use flixnet_media::hls::Segments;
use flixnet_media::transcode::{SessionSpec, TranscodeManager, TranscodeSettings};

/// 30 s of test picture and tone in Matroska (MPEG-4 Part 2 + MP3: needs a transcode).
fn clip(dir: &Path) -> PathBuf {
    let path = dir.join("clip.mkv");
    let status = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "lavfi", "-i", "testsrc=s=320x240:r=25:d=30"])
        .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=30"])
        .args(["-c:v", "mpeg4", "-c:a", "libmp3lame"])
        .arg(&path)
        .status()
        .expect("ffmpeg on PATH");
    assert!(status.success());
    path
}

fn spec(input: PathBuf) -> SessionSpec {
    SessionSpec {
        user_id: 1,
        item_id: 1,
        file_id: 1,
        input,
        segments: Segments::fixed(30_000),
        plan: PlaybackPlan {
            method: PlayMethod::Transcode,
            video: Some(VideoPlan {
                stream_index: 0,
                action: StreamAction::Transcode,
                codec: "h264".into(),
                width: 320,
                height: 240,
                bitrate: None,
                tonemap: false,
            }),
            audio: Some(AudioPlan {
                stream_index: 1,
                action: StreamAction::Transcode,
                codec: "aac".into(),
                channels: 2,
            }),
            subtitle: None,
            reasons: vec![],
        },
        source_video: None,
    }
}

/// Start time of a segment as a player sees it (init + segment).
fn segment_start(init: &Path, segment: &Path, scratch: &Path) -> f64 {
    let joined = scratch.join("joined.mp4");
    let mut bytes = std::fs::read(init).unwrap();
    bytes.extend(std::fs::read(segment).unwrap());
    std::fs::write(&joined, bytes).unwrap();
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=start_time",
            "-of",
            "csv=p=0",
        ])
        .arg(&joined)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().parse().unwrap()
}

async fn wait_until(mut done: impl AsyncFnMut() -> bool, limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if done().await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

#[tokio::test]
async fn seeking_restarts_on_the_absolute_timeline() {
    let dir = tempfile::tempdir().unwrap();
    let input = clip(dir.path());
    let manager = TranscodeManager::new(
        "ffmpeg",
        dir.path().join("transcode"),
        TranscodeSettings::default(),
    );
    let id = manager.create(spec(input));

    let playlist = manager.playlist(&id).unwrap();
    assert_eq!(playlist.matches("#EXTINF").count(), 5);

    // Jump straight to the fourth segment (18 s): ffmpeg starts there.
    let started = Instant::now();
    let segment = manager.segment(&id, 3).await.unwrap();
    let init = manager.init(&id).await.unwrap();
    assert!(started.elapsed() < Duration::from_secs(10));
    let start = segment_start(&init, &segment, dir.path());
    assert!((start - 18.0).abs() < 0.1, "segment 3 starts at {start}");

    // Earlier segments come from a new run; the finished one stays available.
    let first = manager.segment(&id, 0).await.unwrap();
    assert!(segment_start(&init, &first, dir.path()).abs() < 0.1);
    assert_eq!(manager.segment(&id, 3).await.unwrap(), segment);
    assert!(manager.segment(&id, 5).await.is_err(), "past the end");

    // Stopping kills ffmpeg and removes the files.
    let session_dir = segment.parent().unwrap().to_owned();
    manager.stop(&id).await;
    assert!(manager.sessions().await.is_empty());
    assert!(!session_dir.exists());
    assert!(manager.segment(&id, 0).await.is_err());
}

#[tokio::test]
async fn idle_sessions_are_reaped() {
    let dir = tempfile::tempdir().unwrap();
    let input = clip(dir.path());
    let settings = TranscodeSettings {
        idle_timeout: Duration::from_secs(1),
        ..Default::default()
    };
    let manager = TranscodeManager::new("ffmpeg", dir.path().join("transcode"), settings);
    let id = manager.create(spec(input));
    let segment = manager.segment(&id, 0).await.unwrap();
    assert_eq!(manager.sessions().await.len(), 1);

    let manager2 = manager.clone();
    // The reaper forgets the session first and removes its files right after.
    let session_dir = segment.parent().unwrap().to_owned();
    assert!(
        wait_until(
            async || manager2.sessions().await.is_empty() && !session_dir.exists(),
            Duration::from_secs(15)
        )
        .await,
        "idle session reaped"
    );
}

#[tokio::test]
async fn ffmpeg_pauses_when_far_ahead_and_resumes_on_demand() {
    let dir = tempfile::tempdir().unwrap();
    let input = clip(dir.path());
    let settings = TranscodeSettings {
        max_ahead: 1,
        ..Default::default()
    };
    let manager = TranscodeManager::new("ffmpeg", dir.path().join("transcode"), settings);
    let id = manager.create(spec(input));
    manager.segment(&id, 0).await.unwrap();

    // The tiny clip encodes faster than real time, so ffmpeg soon runs ahead and is stopped
    // (or it already finished everything).
    let m = manager.clone();
    assert!(
        wait_until(
            async || !m.sessions().await[0].transcoding,
            Duration::from_secs(10)
        )
        .await,
        "ffmpeg stopped"
    );
    // Asking for the end still works.
    let last = manager.segment(&id, 4).await.unwrap();
    assert!(last.exists());
    manager.stop_all().await;
}

/// Start and duration of a segment as a player sees it (init + segment).
fn segment_span(init: &Path, segment: &Path, scratch: &Path) -> (f64, f64) {
    let joined = scratch.join("span.mp4");
    let mut bytes = std::fs::read(init).unwrap();
    bytes.extend(std::fs::read(segment).unwrap());
    std::fs::write(&joined, bytes).unwrap();
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0"])
        .args([
            "-count_packets",
            "-show_entries",
            "stream=start_time,nb_read_packets",
        ])
        .args(["-of", "csv=p=0"])
        .arg(&joined)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    let (start, packets) = text.trim().split_once(',').unwrap();
    // 25 fps: one packet per 40 ms.
    let packets: f64 = packets.parse().unwrap();
    (start.parse().unwrap(), packets * 0.04)
}

#[tokio::test]
async fn remux_segments_follow_the_keyframe_index() {
    let dir = tempfile::tempdir().unwrap();
    // H.264 with irregular GOPs (forced keyframes only) and MP3 audio.
    let input = dir.path().join("remux.mkv");
    let status = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "lavfi", "-i", "testsrc=s=320x240:r=25:d=20"])
        .args(["-f", "lavfi", "-i", "sine=duration=20"])
        .args([
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-g",
            "1000",
            "-sc_threshold",
            "0",
        ])
        .args(["-force_key_frames", "0,1.3,4.1,5,9.7,13,17.5"])
        .args(["-c:a", "libmp3lame"])
        .arg(&input)
        .status()
        .unwrap();
    assert!(status.success());

    let keyframes = flixnet_media::Ffprobe::new("ffprobe")
        .keyframes(&input, 0)
        .await
        .unwrap();
    assert_eq!(keyframes.len(), 7, "{keyframes:?}");
    let segments = Segments::keyframes(&keyframes, 20_000).unwrap();

    let mut session = spec(input);
    session.segments = segments.clone();
    let video = session.plan.video.as_mut().unwrap();
    video.action = StreamAction::Copy;
    let manager = TranscodeManager::new(
        "ffmpeg",
        dir.path().join("transcode"),
        TranscodeSettings::default(),
    );
    let id = manager.create(session);

    // Seek into the middle first, then go back: every cut sits on its keyframe.
    for index in [4, 1, 5] {
        let segment = manager.segment(&id, index).await.unwrap();
        let init = manager.init(&id).await.unwrap();
        let (start, duration) = segment_span(&init, &segment, dir.path());
        let expected_start = segments.start_ms(index) as f64 / 1000.0;
        let expected_length = segments.length_ms(index) as f64 / 1000.0;
        assert!(
            (start - expected_start).abs() < 0.05,
            "segment {index} starts at {start}, expected {expected_start}"
        );
        assert!(
            (duration - expected_length).abs() < 0.1,
            "segment {index} lasts {duration}, expected {expected_length}"
        );
    }
    manager.stop_all().await;
}

/// ffprobe's view of the audio in `files` joined (init + segments).
fn audio_packets(files: &[PathBuf], scratch: &Path) -> (String, Vec<f64>) {
    let joined = scratch.join("audio.mp4");
    let mut bytes = Vec::new();
    for file in files {
        bytes.extend(std::fs::read(file).unwrap());
    }
    std::fs::write(&joined, bytes).unwrap();
    let probe = |entries: &str| {
        let out = Command::new("ffprobe")
            .args(["-v", "error", "-select_streams", "a:0"])
            .args(["-show_entries", entries, "-of", "csv=p=0"])
            .arg(&joined)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let codec = probe("stream=codec_name").trim().to_owned();
    let pts = probe("packet=pts_time")
        .lines()
        .filter_map(|l| l.trim().trim_end_matches(',').parse().ok())
        .collect();
    (codec, pts)
}

/// Regression (found in 4.7): Chrome plays MP3 files, so its profile lists MP3 — but
/// MP3 copied into the fMP4 segments made Media Source fail with
/// CHUNK_DEMUXER_ERROR_APPEND_FAILED. The decision converts it to AAC, and the AAC
/// track runs on without gaps across segments and restarts.
#[tokio::test]
async fn mp3_audio_reaches_hls_as_aac() {
    use flixnet_media::decision::{
        AudioCodecSupport, DeviceProfile, PlaybackChoice, SourceAudio, SourceMedia, SourceVideo,
        VideoCodecSupport, decide,
    };
    let dir = tempfile::tempdir().unwrap();
    // H.264 (keyframe every 2 s) with MP3 in Matroska: a remux for Chrome.
    let input = dir.path().join("mp3.mkv");
    let status = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "lavfi", "-i", "testsrc=s=320x240:r=25:d=12"])
        .args(["-f", "lavfi", "-i", "sine=duration=12"])
        .args(["-c:v", "libx264", "-preset", "ultrafast", "-g", "50"])
        .args(["-sc_threshold", "0", "-c:a", "libmp3lame"])
        .arg(&input)
        .status()
        .unwrap();
    assert!(status.success());

    let chrome = DeviceProfile {
        containers: vec!["mp4".into(), "webm".into()],
        video_codecs: vec![VideoCodecSupport {
            codec: "h264".into(),
            ..Default::default()
        }],
        audio_codecs: ["aac", "mp3", "opus", "flac"]
            .map(|codec| AudioCodecSupport {
                codec: codec.into(),
                max_channels: Some(6),
            })
            .to_vec(),
        ..Default::default()
    };
    let source = SourceMedia {
        container: "mkv".into(),
        bitrate: None,
        video: Some(SourceVideo {
            index: 0,
            codec: "h264".into(),
            width: 320,
            height: 240,
            bit_depth: 8,
            hdr: "sdr".into(),
        }),
        audio: vec![SourceAudio {
            index: 1,
            codec: "mp3".into(),
            channels: 1,
            is_default: true,
        }],
        subtitles: vec![],
    };
    let plan = decide(&source, &chrome, PlaybackChoice::default());
    assert_eq!(plan.method, PlayMethod::Remux);
    let audio = plan.audio.as_ref().unwrap();
    assert_eq!(
        (audio.action, audio.codec.as_str()),
        (StreamAction::Transcode, "aac")
    );

    let keyframes = flixnet_media::Ffprobe::new("ffprobe")
        .keyframes(&input, 0)
        .await
        .unwrap();
    let mut session = spec(input);
    session.segments = Segments::keyframes(&keyframes, 12_000).unwrap();
    session.plan = plan;
    let manager = TranscodeManager::new(
        "ffmpeg",
        dir.path().join("transcode"),
        TranscodeSettings::default(),
    );
    let id = manager.create(session);

    // A later segment first (its own run), then the start: as after a resume.
    let seg4 = manager.segment(&id, 4).await.unwrap();
    let init = manager.init(&id).await.unwrap();
    let seg0 = manager.segment(&id, 0).await.unwrap();
    let seg1 = manager.segment(&id, 1).await.unwrap();
    let seg2 = manager.segment(&id, 2).await.unwrap();

    let (codec, pts) = audio_packets(&[init.clone(), seg0, seg1, seg2], dir.path());
    assert_eq!(codec, "aac", "MP3 never reaches the fMP4 segments");
    assert!(pts.len() > 100, "{} audio packets", pts.len());
    // AAC priming may start a frame (23 ms) early, never more.
    assert!(
        pts[0] > -0.05 && pts[0] < 0.05,
        "audio starts at {}",
        pts[0]
    );
    for pair in pts.windows(2) {
        let step = pair[1] - pair[0];
        assert!(
            step > 0.0 && step < 0.05,
            "audio runs on without jumps: {} → {}",
            pair[0],
            pair[1]
        );
    }
    let (codec, pts) = audio_packets(&[init, seg4], dir.path());
    assert_eq!(codec, "aac");
    assert!(
        (pts[0] - 8.0).abs() < 0.1,
        "the restarted run's audio starts at its segment: {}",
        pts[0]
    );
    manager.stop_all().await;
}

#[tokio::test]
async fn a_start_hint_begins_the_first_run_at_the_resume_point() {
    let dir = tempfile::tempdir().unwrap();
    let input = clip(dir.path());
    let manager = TranscodeManager::new(
        "ffmpeg",
        dir.path().join("transcode"),
        TranscodeSettings::default(),
    );
    let id = manager.create(spec(input));
    // Resume at 19 s: inside segment 3 (18–24 s).
    manager.hint_start(&id, 19_000);

    // Players fetch the init segment first; ffmpeg must not start at 0 for it.
    let init = manager.init(&id).await.unwrap();
    let segment = manager.segment(&id, 3).await.unwrap();
    let start = segment_start(&init, &segment, dir.path());
    assert!((start - 18.0).abs() < 0.1, "segment 3 starts at {start}");
    let session_dir = segment.parent().unwrap().to_owned();
    assert!(
        !session_dir.join("0.m4s").exists(),
        "nothing was transcoded from the beginning"
    );
    manager.stop(&id).await;
}
