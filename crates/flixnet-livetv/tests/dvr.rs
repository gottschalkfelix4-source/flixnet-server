//! The recorder against the fake source's real-time streams: recordings scheduled in
//! the database run, survive a server restart in a new part file, take a tuner from
//! live viewers, and missed ones are reported.

mod fake;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use flixnet_db::dvr::{self, NewRecording, NewRule, Recording};
use flixnet_db::livetv::{self, NewChannel, NewTunerHost};
use flixnet_db::{Db, now_ms};
use flixnet_livetv::dvr::{Dvr, DvrSettings};
use flixnet_livetv::stream::WatchRequest;
use flixnet_livetv::{LiveSettings, LiveTvManager, StreamSource};
use flixnet_media::decision::{AudioPlan, PlayMethod, PlaybackPlan, StreamAction, VideoPlan};

fn live_settings() -> LiveSettings {
    LiveSettings {
        segment_secs: 1,
        window: 4,
        min_segments: 1,
        viewer_timeout: Duration::from_secs(5),
        buffer: Duration::from_secs(30),
        ..LiveSettings::default()
    }
}

fn dvr_settings() -> DvrSettings {
    DvrSettings {
        tick: Duration::from_millis(200),
        plan_every: Duration::from_secs(60),
        retry: Duration::from_millis(500),
    }
}

/// A database with one playlist tuner (`tuners` at once) and two channels of the
/// fake source. Returns (db, channel ids).
async fn setup(fake: &fake::FakeSource, dir: &std::path::Path, tuners: i64) -> (Db, Vec<i64>) {
    let db = Db::open(&dir.join("flixnet.db")).await.unwrap();
    let host = livetv::create_host(
        &db,
        &NewTunerHost {
            kind: "m3u",
            name: "Fake",
            url: &fake.url("/playlist.m3u"),
            tuner_count: Some(tuners),
            ..NewTunerHost::default()
        },
    )
    .await
    .unwrap();
    let channels: Vec<NewChannel> = [("1", "Das Erste HD"), ("2", "ZDF HD")]
        .iter()
        .map(|(n, name)| NewChannel {
            remote_id: (*n).into(),
            number: Some((*n).into()),
            sort_order: n.parse().unwrap(),
            name: (*name).into(),
            stream_url: fake.url(&format!("/stream/h264/{n}.ts")),
            ..NewChannel::default()
        })
        .collect();
    livetv::sync_channels(&db, host, &channels).await.unwrap();
    let ids = livetv::list_channels(&db, false)
        .await
        .unwrap()
        .iter()
        .map(|c| c.id)
        .collect();
    (db, ids)
}

/// A one-off recording of `channel` from `start` to `end` (no padding).
async fn schedule(db: &Db, channel: i64, title: &str, start: i64, end: i64) -> i64 {
    let rule = dvr::create_rule(
        db,
        &NewRule {
            kind: "program",
            title,
            channel_id: Some(channel),
            program_start_ms: Some(start),
            utc_offset_min: 60,
            ..NewRule::default()
        },
    )
    .await
    .unwrap();
    dvr::upsert_scheduled(
        db,
        &[NewRecording {
            rule_id: rule,
            channel_id: channel,
            channel_name: "Das Erste HD".into(),
            title: title.into(),
            description: Some("Eine Aufnahme.".into()),
            rating: Some("FSK 12".into()),
            categories: vec!["Spielfilm".into()],
            year: Some(2026),
            start_ms: start,
            end_ms: end,
            utc_offset_min: 60,
            ..NewRecording::default()
        }],
    )
    .await
    .unwrap();
    dvr::list(db, &[])
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.title == title)
        .unwrap()
        .id
}

async fn wait_for(db: &Db, id: i64, what: &str, check: impl Fn(&Recording) -> bool) -> Recording {
    let deadline = Instant::now() + Duration::from_secs(40);
    loop {
        let r = dvr::get(db, id).await.unwrap().unwrap();
        if check(&r) {
            return r;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}: {r:?}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[tokio::test]
async fn a_recording_runs_and_resumes_into_a_new_part_after_a_restart() {
    let fake = fake::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (db, channels) = setup(&fake, dir.path(), 2).await;
    let root = dir.path().join("recordings");
    let now = now_ms();
    let id = schedule(&db, channels[0], "Testfilm", now + 1_000, now + 14_000).await;
    // Missed while the server was down: reported, not recorded.
    let missed = schedule(&db, channels[1], "Verpasst", now - 60_000, now - 30_000).await;

    let finished: Arc<Mutex<Vec<i64>>> = Arc::default();
    let start = |live: LiveTvManager| {
        let dvr = Dvr::start(db.clone(), live, &root, dvr_settings());
        let finished = finished.clone();
        dvr.set_hook(Arc::new(move |r| {
            let finished = finished.clone();
            Box::pin(async move {
                if let Some(r) = r {
                    finished.lock().unwrap().push(r.id);
                }
            })
        }));
        dvr
    };

    // First server run: the recording starts at its time and writes part 1.
    let live = LiveTvManager::new(
        "ffmpeg",
        "ffprobe",
        dir.path().join("live1"),
        live_settings(),
    );
    let dvr = start(live.clone());
    let running = wait_for(&db, id, "the recording to write", |r| {
        r.status == "recording" && r.bytes > 100_000
    })
    .await;
    assert_eq!(running.parts, 1);
    let first = std::path::PathBuf::from(running.path.clone().unwrap());
    assert!(
        first.ends_with("Filme/Testfilm (2026)/Testfilm (2026).ts"),
        "{first:?}"
    );
    assert_eq!(live.streams()[0].recordings, 1);
    let gone = wait_for(&db, missed, "the missed one", |r| r.status == "failed").await;
    assert!(gone.error.unwrap().contains("Server lief"));

    // The server stops mid-recording: the recording stays "recording".
    dvr.shutdown().await;
    live.stop_all().await;
    let interrupted = dvr::get(&db, id).await.unwrap().unwrap();
    assert_eq!(interrupted.status, "recording");
    let bytes_before = interrupted.bytes;

    // Next server run: it resumes into part 2 (part 1 is renamed) and completes.
    let live = LiveTvManager::new(
        "ffmpeg",
        "ffprobe",
        dir.path().join("live2"),
        live_settings(),
    );
    let _dvr = start(live);
    let done = wait_for(&db, id, "completion", |r| r.status == "completed").await;
    assert_eq!(done.parts, 2);
    assert!(done.bytes > bytes_before);
    let part1 = first.with_file_name("Testfilm (2026) - part1.ts");
    let part2 = first.with_file_name("Testfilm (2026) - part2.ts");
    assert_eq!(done.path.as_deref(), Some(part1.to_string_lossy().as_ref()));
    assert!(!first.exists());
    for part in [&part1, &part2] {
        assert!(std::fs::metadata(part).unwrap().len() > 50_000, "{part:?}");
    }
    let nfo = std::fs::read_to_string(first.with_file_name("movie.nfo")).unwrap();
    assert!(nfo.contains("<mpaa>FSK 12</mpaa>"), "{nfo}");
    assert!(nfo.contains("<title>Testfilm</title>"), "{nfo}");
    assert_eq!(*finished.lock().unwrap(), [id]);

    // The recorded file is a valid MPEG-TS with the broadcast's streams.
    let probe = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=codec_name",
            "-of",
            "csv=p=0",
        ])
        .arg(&part2)
        .output()
        .unwrap();
    let codecs = String::from_utf8_lossy(&probe.stdout);
    assert!(
        codecs.contains("h264") && codecs.contains("aac"),
        "{codecs}"
    );
}

#[tokio::test]
async fn a_recording_takes_the_tuner_from_live_viewers() {
    let fake = fake::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (db, channels) = setup(&fake, dir.path(), 1).await;
    let host = livetv::list_hosts(&db).await.unwrap()[0].id;
    let live = LiveTvManager::new(
        "ffmpeg",
        "ffprobe",
        dir.path().join("live"),
        live_settings(),
    );
    let copy = PlaybackPlan {
        method: PlayMethod::Remux,
        video: Some(VideoPlan {
            stream_index: 0,
            action: StreamAction::Copy,
            codec: "h264".into(),
            width: 320,
            height: 180,
            bitrate: None,
            tonemap: false,
        }),
        audio: Some(AudioPlan {
            stream_index: 1,
            action: StreamAction::Copy,
            codec: "aac".into(),
            channels: 2,
        }),
        subtitle: None,
        reasons: vec![],
    };
    // Someone watches the second channel on the only tuner.
    let viewer = live
        .watch(WatchRequest {
            user_id: 1,
            channel_id: channels[1],
            tuner_host_id: host,
            tuner_limit: 1,
            source: StreamSource {
                url: fake.url("/stream/h264/2.ts"),
                user_agent: None,
            },
            plan: copy,
        })
        .await
        .unwrap();
    let now = now_ms();
    let id = schedule(&db, channels[0], "Wichtig", now, now + 6_000).await;
    let _dvr = Dvr::start(
        db.clone(),
        live.clone(),
        dir.path().join("rec"),
        dvr_settings(),
    );
    wait_for(&db, id, "the recording to start", |r| r.bytes > 0).await;
    assert!(
        live.playlist(&viewer.session_id).is_err(),
        "the live viewer lost the tuner"
    );
    assert_eq!(live.tuners_in_use(host), 1);
    wait_for(&db, id, "completion", |r| r.status == "completed").await;
}
