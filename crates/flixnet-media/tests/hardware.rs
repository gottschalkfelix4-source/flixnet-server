//! Hardware transcoding against the real ffmpeg. Tests that need a GPU print a note
//! and pass when there is none (CI: software only).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use flixnet_media::decision::{
    AudioPlan, PlayMethod, PlaybackPlan, SourceVideo, StreamAction, VideoPlan,
};
use flixnet_media::hardware::{
    Accel, EncoderKind, HardwareOptions, HardwareReport, HwEncoder, Tonemap, VideoEncoder,
};
use flixnet_media::hls::{HlsJob, Segments, hls_args};
use flixnet_media::transcode::{SessionSpec, TranscodeManager, TranscodeSettings};

fn ffmpeg(args: &[&str], output: &Path) {
    let status = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(args)
        .arg(output)
        .status()
        .expect("ffmpeg on PATH");
    assert!(status.success(), "ffmpeg {args:?}");
}

/// 30 s, 25 fps: H.264 (SDR) or HEVC Main 10 with HDR10 metadata, plus MP3.
fn clip(dir: &Path, hdr: bool, size: &str, seconds: u32) -> (PathBuf, SourceVideo) {
    let path = dir.join(if hdr { "hdr.mkv" } else { "sdr.mkv" });
    let picture = format!("testsrc2=s={size}:r=25:d={seconds}");
    let sound = format!("sine=frequency=440:duration={seconds}");
    let mut args = vec!["-f", "lavfi", "-i", &picture, "-f", "lavfi", "-i", &sound];
    if hdr {
        args.extend([
            "-vf",
            "format=yuv420p10le",
            "-c:v",
            "libx265",
            "-preset",
            "ultrafast",
            "-x265-params",
            "log-level=error:hdr10=1:colorprim=bt2020:transfer=smpte2084:colormatrix=bt2020nc:\
             master-display=G(13250,34500)B(7500,3000)R(34000,16000)WP(15635,16450)L(10000000,1):\
             max-cll=1000,400",
        ]);
    } else {
        args.extend(["-c:v", "libx264", "-preset", "ultrafast"]);
    }
    args.extend(["-c:a", "libmp3lame"]);
    ffmpeg(&args, &path);
    let (width, height) = size.split_once('x').unwrap();
    let source = SourceVideo {
        index: 0,
        codec: if hdr { "hevc" } else { "h264" }.into(),
        width: width.parse().unwrap(),
        height: height.parse().unwrap(),
        bit_depth: if hdr { 10 } else { 8 },
        hdr: if hdr { "hdr10" } else { "sdr" }.into(),
    };
    (path, source)
}

fn plan(width: i64, height: i64, tonemap: bool) -> PlaybackPlan {
    PlaybackPlan {
        method: PlayMethod::Transcode,
        video: Some(VideoPlan {
            stream_index: 0,
            action: StreamAction::Transcode,
            codec: "h264".into(),
            width,
            height,
            bitrate: None,
            tonemap,
        }),
        audio: Some(AudioPlan {
            stream_index: 1,
            action: StreamAction::Transcode,
            codec: "aac".into(),
            channels: 2,
        }),
        subtitle: None,
        reasons: vec![],
    }
}

fn spec(input: PathBuf, source: SourceVideo, tonemap: bool) -> SessionSpec {
    SessionSpec {
        user_id: 1,
        item_id: 1,
        file_id: 1,
        input,
        segments: Segments::fixed(30_000),
        plan: plan(320, 240, tonemap),
        source_video: Some(source),
    }
}

async fn detect(dir: &Path) -> HardwareReport {
    flixnet_media::hardware::detect(
        Path::new("ffmpeg"),
        &dir.join("hwtest"),
        &HardwareOptions::default(),
    )
    .await
}

/// Start, length and keyframe count of a segment's video as a player sees it.
fn inspect(init: &Path, segment: &Path, scratch: &Path) -> (f64, f64, usize) {
    let joined = scratch.join("joined.mp4");
    let mut bytes = std::fs::read(init).unwrap();
    bytes.extend(std::fs::read(segment).unwrap());
    std::fs::write(&joined, bytes).unwrap();
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0"])
        .args(["-show_entries", "packet=pts_time,flags", "-of", "csv=p=0"])
        .arg(&joined)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    let packets: Vec<(f64, bool)> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let (pts, flags) = l.split_once(',').unwrap();
            (pts.parse().unwrap(), flags.contains('K'))
        })
        .collect();
    let start = packets.iter().map(|p| p.0).fold(f64::INFINITY, f64::min);
    let keyframes = packets.iter().filter(|p| p.1).count();
    // 25 fps: one packet per 40 ms.
    (start, packets.len() as f64 * 0.04, keyframes)
}

/// Seeks to the fourth segment, then goes back to the first: every segment starts on
/// the 6 s grid, lasts 6 s and has exactly one keyframe.
async fn check_alignment(manager: &TranscodeManager, id: &str, scratch: &Path) {
    for index in [3u32, 0, 1, 4] {
        let segment = manager.segment(id, index).await.unwrap();
        let init = manager.init(id).await.unwrap();
        let (start, length, keyframes) = inspect(&init, &segment, scratch);
        let expected = f64::from(index) * 6.0;
        assert!(
            (start - expected).abs() < 0.05,
            "segment {index} starts at {start}, expected {expected}"
        );
        assert!((length - 6.0).abs() < 0.1, "segment {index} lasts {length}");
        assert_eq!(keyframes, 1, "segment {index} has {keyframes} keyframes");
    }
}

#[tokio::test]
async fn detection_reports_and_never_fails() {
    let dir = tempfile::tempdir().unwrap();
    let report = detect(dir.path()).await;
    let version = report.ffmpeg_version.clone().unwrap_or_default();
    assert!(version.starts_with("ffmpeg version"), "{version}");
    for probe in &report.probes {
        eprintln!("{probe:?}");
        assert_eq!(probe.available, probe.error.is_none(), "{probe:?}");
    }
    assert!(!dir.path().join("hwtest").exists(), "test clip removed");
    // Whatever was found, there is always an encoder.
    let best = report.best();
    assert_eq!(
        best == VideoEncoder::Software,
        report.available().is_empty()
    );
}

#[tokio::test]
async fn hardware_segments_fit_together() {
    let dir = tempfile::tempdir().unwrap();
    let report = detect(dir.path()).await;
    if report.available().is_empty() {
        eprintln!("skipped: no hardware encoder ({:?})", report.probes);
        return;
    }
    let (sdr, sdr_source) = clip(dir.path(), false, "320x240", 30);
    let (hdr, hdr_source) = clip(dir.path(), true, "320x240", 30);
    for accel in report.available() {
        let encoder = report.encoder(accel).unwrap();
        eprintln!("testing {encoder:?}");
        let manager = TranscodeManager::new(
            "ffmpeg",
            dir.path().join(format!("transcode-{accel:?}")),
            TranscodeSettings::default(),
        );
        manager.set_encoder(encoder);
        for (input, source, tonemap) in [(&sdr, &sdr_source, false), (&hdr, &hdr_source, true)] {
            let id = manager.create(spec(input.clone(), source.clone(), tonemap));
            check_alignment(&manager, &id, dir.path()).await;
            let info = &manager.sessions().await[0];
            assert_eq!(info.encoder, EncoderKind::from(accel), "no fallback needed");
            manager.stop(&id).await;
        }
        assert_eq!(manager.transcoding_info().fallbacks, 0);
    }
}

#[tokio::test]
async fn failing_hardware_falls_back_to_software() {
    let dir = tempfile::tempdir().unwrap();
    let (input, source) = clip(dir.path(), false, "320x240", 30);
    let manager = TranscodeManager::new(
        "ffmpeg",
        dir.path().join("transcode"),
        TranscodeSettings::default(),
    );
    // A render node that does not exist: GPU decoding fails, then GPU encoding.
    manager.set_encoder(VideoEncoder::Hardware(HwEncoder {
        accel: Accel::Vaapi,
        device: dir.path().join("renderD128"),
        low_power: false,
        hw_decode: true,
        tonemap: Tonemap::Vaapi,
    }));
    let id = manager.create(spec(input, source, false));
    check_alignment(&manager, &id, dir.path()).await;
    assert_eq!(manager.sessions().await[0].encoder, EncoderKind::Software);
    assert_eq!(manager.transcoding_info().fallbacks, 2);
    manager.stop_all().await;
}

/// 4K HEVC Main 10 HDR → 1080p H.264 SDR, software against every hardware method.
/// `cargo test -p flixnet-media --test hardware -- --ignored --nocapture`
#[tokio::test]
#[ignore = "benchmark; takes minutes"]
async fn benchmark_4k_hdr_to_1080p() {
    let dir = tempfile::tempdir().unwrap();
    let (input, source) = clip(dir.path(), true, "3840x2160", 30);
    let report = detect(dir.path()).await;
    let mut encoders = vec![VideoEncoder::Software];
    encoders.extend(report.available().iter().filter_map(|a| report.encoder(*a)));
    let plan = plan(1920, 1080, true);
    let segments = Segments::fixed(30_000);
    for encoder in encoders {
        let out = dir.path().join(format!("bench-{:?}", encoder.kind()));
        std::fs::create_dir_all(&out).unwrap();
        let args = hls_args(&HlsJob {
            input: &input,
            out_dir: &out,
            start_segment: 0,
            segments: &segments,
            plan: &plan,
            encoder: &encoder,
            source: Some(&source),
        });
        let started = Instant::now();
        let status = Command::new("ffmpeg")
            .current_dir(&out)
            .args(&args)
            .status()
            .unwrap();
        let elapsed = started.elapsed();
        assert!(status.success(), "{encoder:?}");
        let fps = 750.0 / elapsed.as_secs_f64();
        eprintln!(
            "{:?}: {:.1} s for 30 s ({fps:.0} fps, {:.1}x realtime) — {encoder:?}",
            encoder.kind(),
            elapsed.as_secs_f64(),
            30.0 / elapsed.as_secs_f64()
        );
    }
}
