//! Live streams against the fake source's real-time ffmpeg streams: shared
//! upstream, tuner counting, segments, stop and reaping.

mod fake;

use std::time::{Duration, Instant};

use flixnet_livetv::stream::WatchRequest;
use flixnet_livetv::{LiveError, LiveSettings, LiveTvManager, StreamSource};
use flixnet_media::decision::{AudioPlan, PlayMethod, PlaybackPlan, StreamAction, VideoPlan};

fn settings() -> LiveSettings {
    LiveSettings {
        segment_secs: 1,
        window: 4,
        min_segments: 1,
        start_timeout: Duration::from_secs(30),
        viewer_timeout: Duration::from_secs(3),
        probe_timeout: Duration::from_secs(15),
    }
}

fn plan(video: StreamAction, audio: StreamAction) -> PlaybackPlan {
    PlaybackPlan {
        method: PlayMethod::Remux,
        video: Some(VideoPlan {
            stream_index: 0,
            action: video,
            codec: "h264".into(),
            width: 320,
            height: 180,
            bitrate: None,
            tonemap: false,
        }),
        audio: Some(AudioPlan {
            stream_index: 1,
            action: audio,
            codec: "aac".into(),
            channels: 2,
        }),
        subtitle: None,
        reasons: vec![],
    }
}

fn request(channel_id: i64, url: String, limit: i64, plan: PlaybackPlan) -> WatchRequest {
    WatchRequest {
        user_id: 1,
        channel_id,
        tuner_host_id: 7,
        tuner_limit: limit,
        source: StreamSource {
            url,
            user_agent: None,
        },
        plan,
    }
}

async fn wait_for(what: &str, mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !check() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn probe_reads_codecs_of_a_live_stream() {
    let fake = fake::start().await;
    let dir = tempfile::tempdir().unwrap();
    let live = LiveTvManager::new("ffmpeg", "ffprobe", dir.path().join("live"), settings());
    let info = live
        .probe(&StreamSource {
            url: fake.url("/stream/mpeg2/3.ts"),
            user_agent: Some("Flixnet-Test".into()),
        })
        .await
        .unwrap();
    let codecs: Vec<_> = info
        .streams
        .iter()
        .filter_map(|s| s.codec.as_deref())
        .collect();
    assert!(
        codecs.contains(&"mpeg2video") && codecs.contains(&"mp2"),
        "{codecs:?}"
    );
    assert!(
        live.probe(&StreamSource {
            url: fake.url("/missing.ts"),
            user_agent: None
        })
        .await
        .is_err()
    );
}

#[tokio::test]
async fn viewers_share_one_upstream_and_tuners_are_counted() {
    let fake = fake::start().await;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("live");
    let live = LiveTvManager::new("ffmpeg", "ffprobe", &root, settings());
    let copy = plan(StreamAction::Copy, StreamAction::Copy);

    let first = live
        .watch(request(1, fake.url("/stream/h264/1.ts"), 1, copy.clone()))
        .await
        .unwrap();
    assert!(!first.shared);
    let second = live
        .watch(request(1, fake.url("/stream/h264/1.ts"), 1, copy.clone()))
        .await
        .unwrap();
    assert!(second.shared, "the second viewer joins the running stream");
    assert_eq!(first.stream_id, second.stream_id);
    assert_ne!(first.session_id, second.session_id);
    assert_eq!(
        fake.open_streams(),
        1,
        "one upstream connection for both viewers"
    );
    assert_eq!(live.tuners_in_use(7), 1);

    // The only tuner is taken: another channel of the same host must wait.
    let busy = live
        .watch(request(2, fake.url("/stream/h264/2.ts"), 1, copy.clone()))
        .await;
    assert!(matches!(busy, Err(LiveError::TunersBusy)), "{busy:?}");

    // Playlist and segments, through either session.
    let playlist = live.playlist(&first.session_id).unwrap();
    assert!(playlist.starts_with("#EXTM3U"), "{playlist}");
    let segment = playlist
        .lines()
        .find(|l| l.ends_with(".ts"))
        .unwrap()
        .to_owned();
    let path = live.segment(&second.session_id, &segment).unwrap();
    assert!(std::fs::metadata(&path).unwrap().len() > 0);
    assert!(live.segment(&first.session_id, "../ffmpeg.log").is_err());
    assert!(live.segment(&first.session_id, "seg999999.ts").is_err());

    // The live window moves on.
    let first_segment = segment.clone();
    wait_for("the window to move", || {
        live.playlist(&first.session_id)
            .is_ok_and(|p| !p.contains(&format!("\n{first_segment}\n")))
    })
    .await;

    // The first viewer leaves: the stream keeps running for the second.
    live.stop(&first.session_id).await;
    assert!(live.playlist(&first.session_id).is_err());
    assert!(live.playlist(&second.session_id).is_ok());
    assert_eq!(live.streams().len(), 1);
    assert_eq!(live.streams()[0].viewers, [1]);

    // The last viewer leaves: ffmpeg stops, the upstream closes, files are removed.
    live.stop(&second.session_id).await;
    assert!(live.streams().is_empty());
    assert_eq!(live.tuners_in_use(7), 0);
    wait_for("the upstream to close", || fake.open_streams() == 0).await;
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);

    // With the tuner free again the other channel starts.
    let other = live
        .watch(request(2, fake.url("/stream/h264/2.ts"), 1, copy))
        .await
        .unwrap();
    live.stop(&other.session_id).await;
}

#[tokio::test]
async fn silent_viewers_are_reaped_and_transcodes_work() {
    let fake = fake::start().await;
    let dir = tempfile::tempdir().unwrap();
    let live = LiveTvManager::new("ffmpeg", "ffprobe", dir.path().join("live"), settings());

    // MPEG-2 video and MP2 audio: transcoded to H.264/AAC.
    let ticket = live
        .watch(request(
            3,
            fake.url("/stream/mpeg2/3.ts"),
            0,
            plan(StreamAction::Transcode, StreamAction::Transcode),
        ))
        .await
        .unwrap();
    let streams = live.streams();
    assert_eq!(streams.len(), 1);
    assert!(!streams[0].video_copied && !streams[0].audio_copied);

    let playlist = live.playlist(&ticket.session_id).unwrap();
    let segment = playlist.lines().find(|l| l.ends_with(".ts")).unwrap();
    let path = live.segment(&ticket.session_id, segment).unwrap();
    let probe = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=codec_name",
            "-of",
            "csv=p=0",
        ])
        .arg(&path)
        .output()
        .unwrap();
    let codecs = String::from_utf8_lossy(&probe.stdout);
    assert!(
        codecs.contains("h264") && codecs.contains("aac"),
        "{codecs}"
    );

    // Nobody asks for anything: the viewer times out and the stream stops.
    wait_for("the reaper", || live.streams().is_empty()).await;
    assert!(matches!(
        live.playlist(&ticket.session_id),
        Err(LiveError::UnknownSession)
    ));
    wait_for("the upstream to close", || fake.open_streams() == 0).await;
}

#[tokio::test]
async fn a_broken_source_fails_cleanly() {
    let fake = fake::start().await;
    let dir = tempfile::tempdir().unwrap();
    let live = LiveTvManager::new("ffmpeg", "ffprobe", dir.path().join("live"), settings());
    let failed = live
        .watch(request(
            4,
            fake.url("/missing.ts"),
            0,
            plan(StreamAction::Copy, StreamAction::Copy),
        ))
        .await;
    match failed {
        Err(LiveError::Failed(reason)) => assert!(reason.contains("404"), "{reason}"),
        other => panic!("expected a failure, got {other:?}"),
    }
    assert!(live.streams().is_empty());
}
