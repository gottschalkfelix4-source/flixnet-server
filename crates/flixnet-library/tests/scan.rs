//! Milestone 2.3 gate: a library tree scans into the right items, probes run, and a
//! rescan without changes writes nothing and queues no probes. Needs ffmpeg/ffprobe.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use flixnet_core::events::EventBus;
use flixnet_core::jobs::{JobQueue, JobRuntime};
use flixnet_db::{Db, items, jobs, libraries, media};
use flixnet_library::jobs::{ProbePayload, probe_file};
use flixnet_library::scan::scan_library;
use flixnet_media::Ffprobe;
use tempfile::TempDir;

/// One tiny real video, generated once and copied around.
fn sample_video() -> &'static Path {
    static SAMPLE: OnceLock<(TempDir, PathBuf)> = OnceLock::new();
    &SAMPLE
        .get_or_init(|| {
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("sample.mkv");
            let status = Command::new("ffmpeg")
                .args(["-hide_banner", "-loglevel", "error", "-y"])
                .args(["-f", "lavfi", "-i", "testsrc=s=320x240:r=25:d=1"])
                .args(["-f", "lavfi", "-i", "sine=d=1"])
                .args(["-c:v", "mpeg4", "-c:a", "mp2"])
                .arg(&file)
                .status()
                .expect("ffmpeg must be installed");
            assert!(status.success());
            (dir, file)
        })
        .1
}

fn place(root: &Path, relative: &str) {
    let target = root.join(relative);
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::fs::copy(sample_video(), &target).unwrap();
}

struct Fixture {
    _dir: TempDir,
    db: Db,
    queue: JobQueue,
    movies_root: PathBuf,
    shows_root: PathBuf,
    movies: i64,
    shows: i64,
}

async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("flixnet.db")).await.unwrap();
    let queue = JobRuntime::new(db.clone(), EventBus::new())
        .handle()
        .queue()
        .clone();

    let movies_root = dir.path().join("Filme");
    for file in [
        "Testfilm Alpha (2021)/Testfilm.Alpha.2021.German.DL.1080p.WEB.H264-FLX.mkv",
        "Testfilm Alpha (2021)/Testfilm Alpha (2021)-trailer.mkv",
        "Testfilm Alpha (2021)/Sample/sample.mkv",
        "Testfilm Beta (2019) {tmdb-990001}/Testfilm Beta (2019) {tmdb-990001}.mkv",
        "Testfilm Gamma (2023)/Testfilm Gamma (2023) - cd1.mkv",
        "Testfilm Gamma (2023)/Testfilm Gamma (2023) - cd2.mkv",
    ] {
        place(&movies_root, file);
    }
    std::fs::write(
        movies_root
            .join("Testfilm Alpha (2021)/Testfilm.Alpha.2021.German.DL.1080p.WEB.H264-FLX.de.srt"),
        "1\n00:00:00,000 --> 00:00:01,000\nHallo\n",
    )
    .unwrap();
    std::fs::write(movies_root.join("notes.txt"), "not media").unwrap();

    let shows_root = dir.path().join("Serien");
    for file in [
        "Testserie (2022) [tvdbid-990100]/Staffel 01/Testserie.S01E01.mkv",
        "Testserie (2022) [tvdbid-990100]/Staffel 01/Testserie.S01E02.mkv",
        "Testserie (2022) [tvdbid-990100]/Staffel 01/Testserie.S01E03E04.mkv",
        "Testserie (2022) [tvdbid-990100]/Staffel 02/Testserie.S02E01.mkv",
    ] {
        place(&shows_root, file);
    }

    let movies = libraries::create(
        &db,
        "Filme",
        "movies",
        &[movies_root.to_string_lossy().into_owned()],
    )
    .await
    .unwrap()
    .id;
    let shows = libraries::create(
        &db,
        "Serien",
        "shows",
        &[shows_root.to_string_lossy().into_owned()],
    )
    .await
    .unwrap()
    .id;
    Fixture {
        _dir: dir,
        db,
        queue,
        movies_root,
        shows_root,
        movies,
        shows,
    }
}

/// A minimal worker: claims queued probe jobs and runs them to completion.
async fn run_probes(db: &Db) -> usize {
    let ffprobe = Ffprobe::new("ffprobe");
    let mut ran = 0;
    while let Some(job) = jobs::claim(db, "probe").await.unwrap() {
        let payload: ProbePayload = serde_json::from_str(&job.payload).unwrap();
        probe_file(db, &ffprobe, payload.file_id).await.unwrap();
        jobs::complete(db, job.id).await.unwrap();
        ran += 1;
    }
    ran
}

#[tokio::test]
async fn scan_builds_items_and_rescan_is_a_no_op() {
    let f = fixture().await;

    // --- Movies ---------------------------------------------------------------
    let report = scan_library(&f.db, &f.queue, f.movies).await.unwrap();
    assert_eq!(report.added, 5, "{report:?}");
    assert_eq!(report.skipped, 1, "the sample is not imported");
    assert_eq!(report.probes_queued, 5);

    let movies = items::list_by_kind(&f.db, f.movies, "movie").await.unwrap();
    let titles: Vec<_> = movies.iter().map(|m| (m.title.as_str(), m.year)).collect();
    assert_eq!(
        titles,
        [
            ("Testfilm Alpha", Some(2021)),
            ("Testfilm Beta", Some(2019)),
            ("Testfilm Gamma", Some(2023))
        ]
    );
    assert_eq!(movies[1].content_key, "tmdb:movie:990001");
    assert_eq!(
        items::provider_ids(&f.db, movies[1].id).await.unwrap(),
        [("tmdb".to_owned(), "990001".to_owned())]
    );

    let alpha_files = media::files_for_item(&f.db, movies[0].id).await.unwrap();
    let roles: Vec<_> = alpha_files
        .iter()
        .map(|f| (f.role.as_str(), f.extra_kind.as_deref()))
        .collect();
    assert_eq!(roles, [("extra", Some("trailer")), ("main", None)]);
    let gamma_parts: Vec<_> = media::files_for_item(&f.db, movies[2].id)
        .await
        .unwrap()
        .iter()
        .map(|f| f.part)
        .collect();
    assert_eq!(
        gamma_parts,
        [Some(1), Some(2)],
        "multi-part files share one item"
    );

    // --- Shows ----------------------------------------------------------------
    let report = scan_library(&f.db, &f.queue, f.shows).await.unwrap();
    assert_eq!((report.added, report.probes_queued), (4, 4));
    let series = items::list_by_kind(&f.db, f.shows, "series").await.unwrap();
    assert_eq!(series.len(), 1);
    assert_eq!(
        (series[0].title.as_str(), series[0].year),
        ("Testserie", Some(2022))
    );
    assert_eq!(series[0].content_key, "tvdb:series:990100");
    let seasons = items::list_by_kind(&f.db, f.shows, "season").await.unwrap();
    assert_eq!(
        seasons.iter().map(|s| s.index_number).collect::<Vec<_>>(),
        [Some(1), Some(2)]
    );
    let episodes = items::list_by_kind(&f.db, f.shows, "episode")
        .await
        .unwrap();
    let numbers: Vec<_> = episodes
        .iter()
        .map(|e| (e.parent_index_number, e.index_number, e.index_end))
        .collect();
    assert_eq!(
        numbers,
        [
            (Some(1), Some(1), None),
            (Some(1), Some(2), None),
            (Some(1), Some(3), Some(4)),
            (Some(2), Some(1), None)
        ]
    );
    assert!(episodes.iter().all(|e| e.series_id == Some(series[0].id)));

    // --- Probes ---------------------------------------------------------------
    assert_eq!(run_probes(&f.db).await, 9);
    let main = alpha_files.iter().find(|f| f.role == "main").unwrap();
    let probed = media::get_file(&f.db, main.id).await.unwrap().unwrap();
    assert_eq!(probed.container.as_deref(), Some("mkv"));
    assert!(probed.duration_ms.unwrap() > 900);
    let streams = media::streams(&f.db, main.id).await.unwrap();
    let kinds: Vec<_> = streams
        .iter()
        .map(|s| (s.kind.as_str(), s.is_external))
        .collect();
    assert_eq!(
        kinds,
        [("video", false), ("audio", false), ("subtitle", true)]
    );
    assert_eq!(streams[2].language.as_deref(), Some("deu"));
    assert_eq!(streams[2].idx, 1000);

    // --- Gate: rescan without changes is a no-op --------------------------------
    for library in [f.movies, f.shows] {
        let report = scan_library(&f.db, &f.queue, library).await.unwrap();
        assert_eq!(
            (
                report.added,
                report.changed,
                report.missing,
                report.probes_queued
            ),
            (0, 0, 0, 0),
            "{report:?}"
        );
    }
    assert_eq!(run_probes(&f.db).await, 0);

    // --- A changed file is re-probed; a deleted one is marked missing -------------
    let e1 = f
        .shows_root
        .join("Testserie (2022) [tvdbid-990100]/Staffel 01/Testserie.S01E01.mkv");
    let mut bytes = std::fs::read(&e1).unwrap();
    bytes.extend_from_slice(&[0; 16]);
    std::fs::write(&e1, bytes).unwrap();
    std::fs::remove_file(
        f.shows_root
            .join("Testserie (2022) [tvdbid-990100]/Staffel 02/Testserie.S02E01.mkv"),
    )
    .unwrap();

    let report = scan_library(&f.db, &f.queue, f.shows).await.unwrap();
    assert_eq!(
        (report.changed, report.missing, report.probes_queued),
        (1, 1, 1),
        "{report:?}"
    );
    assert_eq!(run_probes(&f.db).await, 1);
    // Missing files keep their items during the grace period (an unmounted disk
    // must not wipe watch history).
    assert_eq!(
        items::list_by_kind(&f.db, f.shows, "episode")
            .await
            .unwrap()
            .len(),
        4
    );
}

#[tokio::test]
async fn unreachable_folders_never_mark_files_missing() {
    let f = fixture().await;
    scan_library(&f.db, &f.queue, f.movies).await.unwrap();

    // The disk goes away (renamed folder = unmounted share).
    let unmounted = f.movies_root.with_file_name("Filme-offline");
    std::fs::rename(&f.movies_root, &unmounted).unwrap();

    let report = scan_library(&f.db, &f.queue, f.movies).await.unwrap();
    assert_eq!(report.unreachable.len(), 1);
    assert_eq!(report.missing, 0, "{report:?}");
    let counts = libraries::counts(&f.db, f.movies).await.unwrap();
    assert_eq!(counts.files, 5);
}

/// The developer fixture library (`cargo xtask fixtures`), if it has been generated.
#[tokio::test]
async fn scans_the_generated_fixture_library() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/generated");
    if !root.join("movies").exists() {
        eprintln!("fixtures not generated; skipping (run `cargo xtask fixtures`)");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("flixnet.db")).await.unwrap();
    let queue = JobRuntime::new(db.clone(), EventBus::new())
        .handle()
        .queue()
        .clone();
    let movies = libraries::create(
        &db,
        "Filme",
        "movies",
        &[root.join("movies").to_string_lossy().into_owned()],
    )
    .await
    .unwrap();
    let shows = libraries::create(
        &db,
        "Serien",
        "shows",
        &[root.join("shows").to_string_lossy().into_owned()],
    )
    .await
    .unwrap();

    scan_library(&db, &queue, movies.id).await.unwrap();
    scan_library(&db, &queue, shows.id).await.unwrap();
    assert_eq!(
        items::list_by_kind(&db, movies.id, "movie")
            .await
            .unwrap()
            .len(),
        5
    );
    assert_eq!(
        items::list_by_kind(&db, shows.id, "episode")
            .await
            .unwrap()
            .len(),
        3
    );
    assert_eq!(run_probes(&db).await, 8);

    // HDR10 + two audio tracks + embedded ASS + two sidecar subtitles on "Beta".
    let beta = items::list_by_kind(&db, movies.id, "movie")
        .await
        .unwrap()
        .into_iter()
        .find(|m| m.title == "Testfilm Beta")
        .unwrap();
    let file = &media::files_for_item(&db, beta.id).await.unwrap()[0];
    let streams = media::streams(&db, file.id).await.unwrap();
    assert_eq!(streams[0].hdr.as_deref(), Some("hdr10"));
    assert_eq!(streams.iter().filter(|s| s.kind == "audio").count(), 2);
    assert_eq!(streams.iter().filter(|s| s.is_external).count(), 2);
}
