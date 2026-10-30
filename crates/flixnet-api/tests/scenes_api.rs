//! Milestone 6.5 with real ffmpeg: a generated clip with an embedded English SubRip
//! track and a German sidecar `.srt` is indexed into time-stamped chunks; scene
//! search finds a line and links to the player at its time.

mod common;

use std::path::Path;
use std::process::Command;

use axum::http::StatusCode;
use common::{TestApp, app};
use flixnet_core::events::EventBus;
use flixnet_core::jobs::JobRuntime;
use flixnet_db::{libraries, media, transcripts};
use flixnet_library::scan::scan_library;
use flixnet_library::scenes::index_file;
use flixnet_media::subtitles::SubtitleCache;

fn ffmpeg(args: &[&str]) {
    let status = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(args)
        .status()
        .expect("ffmpeg on PATH");
    assert!(status.success(), "ffmpeg {args:?}");
}

/// Two minutes of black video with sound; English SubRip inside, German sidecar.
fn make_movie(folder: &Path) {
    std::fs::create_dir_all(folder).unwrap();
    let srt = folder.join("embedded.tmp.srt");
    std::fs::write(
        &srt,
        "1\n00:00:05,000 --> 00:00:08,000\nGood morning.\n\n\
         2\n00:01:30,000 --> 00:01:34,000\nWe should talk about the lighthouse.\n",
    )
    .unwrap();
    let movie = folder.join("Strandtag.2022.mkv");
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "color=c=black:s=160x120:r=10:d=120",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=440:duration=120",
        "-i",
        srt.to_str().unwrap(),
        "-map",
        "0",
        "-map",
        "1",
        "-map",
        "2",
        "-c:v",
        "libx264",
        "-preset",
        "ultrafast",
        "-g",
        "50",
        "-c:a",
        "aac",
        "-c:s",
        "srt",
        "-metadata:s:s:0",
        "language=eng",
        movie.to_str().unwrap(),
    ]);
    std::fs::remove_file(srt).unwrap();
    std::fs::write(
        folder.join("Strandtag.2022.de.srt"),
        "1\r\n00:00:04,000 --> 00:00:07,000\r\nGuten Morgen!\r\n\r\n\
         2\r\n00:00:20,000 --> 00:00:24,000\r\nDas Wetter ist herrlich.\r\n\r\n\
         3\r\n00:01:12,500 --> 00:01:16,000\r\n<i>Hör auf, mit mir am Strand zu streiten!</i>\r\n\r\n\
         4\r\n00:01:17,000 --> 00:01:20,000\r\nIch streite doch gar nicht.\r\n",
    )
    .unwrap();
}

struct Fixture {
    token: String,
    movie: i64,
    file_id: i64,
    folder: std::path::PathBuf,
}

async fn library(app: &TestApp, root: &Path) -> Fixture {
    let folder = root.join("Filme/Strandtag (2022)");
    make_movie(&folder);
    let library = libraries::create(
        &app.db,
        "Filme",
        "movies",
        &[root.join("Filme").to_string_lossy().into_owned()],
    )
    .await
    .unwrap()
    .id;
    let queue = JobRuntime::new(app.db.clone(), EventBus::new())
        .handle()
        .queue()
        .clone();
    scan_library(&app.db, &queue, library).await.unwrap();
    let file_id = media::file_states(&app.db, library).await.unwrap()[0].id;
    flixnet_library::jobs::probe_file(&app.db, &flixnet_media::Ffprobe::new("ffprobe"), file_id)
        .await
        .unwrap();
    let token = app.setup_admin().await;
    let movie = app.get("/api/v1/items?kind=movie", Some(&token)).await.json["items"][0]["id"]
        .as_i64()
        .unwrap();
    Fixture {
        token,
        movie,
        file_id,
        folder,
    }
}

fn cache(app: &TestApp) -> SubtitleCache {
    SubtitleCache::new(
        "ffmpeg",
        app.dir.path().join("cache/subtitles"),
        std::time::Duration::from_secs(60),
        2,
    )
}

#[tokio::test]
async fn scene_search_finds_a_line_and_links_to_its_time() {
    let app = app().await;
    let root = tempfile::tempdir().unwrap();
    let f = library(&app, root.path()).await;
    let cache = cache(&app);

    // Nothing indexed yet.
    let r = app
        .get("/api/v1/scenes/search?q=Strand", Some(&f.token))
        .await;
    assert_eq!(r.json["hits"], serde_json::json!([]));
    assert_eq!(
        transcripts::files_to_index(&app.db, 10).await.unwrap(),
        vec![f.file_id]
    );

    let outcome = index_file(&app.db, &cache, f.file_id).await.unwrap();
    assert_eq!(outcome.indexed, 2, "embedded English and sidecar German");
    assert!(
        transcripts::files_to_index(&app.db, 10)
            .await
            .unwrap()
            .is_empty()
    );
    let stored = transcripts::for_file(&app.db, f.file_id).await.unwrap();
    let german = stored.iter().find(|t| t.stream_index >= 1000).unwrap();
    // 4–24 s and 72.5–80 s are two scenes (long silence in between).
    assert_eq!(german.chunk_count, 2, "{stored:?}");

    let r = app
        .get(
            "/api/v1/scenes/search?q=die%20Szene%20wo%20sie%20am%20Strand%20streiten",
            Some(&f.token),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let hits = r.json["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 1, "{}", r.json);
    let hit = &hits[0];
    assert_eq!(hit["item"]["id"], f.movie);
    assert_eq!(hit["start_ms"], 72_500);
    assert_eq!(hit["end_ms"], 80_000);
    assert_eq!(hit["play_url"], format!("/play/{}?start=72500", f.movie));
    let snippet = hit["snippet"].as_str().unwrap();
    assert!(snippet.contains("am Strand zu streiten"), "{snippet}");
    assert!(!snippet.contains("<i>"), "markup removed: {snippet}");

    // English dialogue from the embedded track, and a search within the movie.
    let r = app
        .get(
            &format!("/api/v1/scenes/search?q=lighthouse&item_id={}", f.movie),
            Some(&f.token),
        )
        .await;
    assert_eq!(r.json["hits"][0]["start_ms"], 90_000, "{}", r.json);

    // Unchanged sources are skipped; a changed sidecar is indexed again.
    let again = index_file(&app.db, &cache, f.file_id).await.unwrap();
    assert_eq!((again.indexed, again.unchanged), (0, 2));
    std::thread::sleep(std::time::Duration::from_millis(1100));
    std::fs::write(
        f.folder.join("Strandtag.2022.de.srt"),
        "1\n00:00:10,000 --> 00:00:12,000\nNur noch eine Zeile über Möwen.\n",
    )
    .unwrap();
    let changed = index_file(&app.db, &cache, f.file_id).await.unwrap();
    assert_eq!((changed.indexed, changed.unchanged), (1, 1));
    let r = app
        .get("/api/v1/scenes/search?q=Strand", Some(&f.token))
        .await;
    assert_eq!(r.json["hits"], serde_json::json!([]), "old text is gone");
    let r = app
        .get("/api/v1/scenes/search?q=M%C3%B6wen", Some(&f.token))
        .await;
    assert_eq!(r.json["hits"][0]["start_ms"], 10_000);

    // A deleted sidecar takes its chunks along (after the next probe).
    std::fs::remove_file(f.folder.join("Strandtag.2022.de.srt")).unwrap();
    flixnet_library::jobs::probe_file(&app.db, &flixnet_media::Ffprobe::new("ffprobe"), f.file_id)
        .await
        .unwrap();
    index_file(&app.db, &cache, f.file_id).await.unwrap();
    let r = app
        .get("/api/v1/scenes/search?q=M%C3%B6wen", Some(&f.token))
        .await;
    assert_eq!(r.json["hits"], serde_json::json!([]));
    let status = app.get("/api/v1/assistant/status", Some(&f.token)).await;
    assert_eq!(status.json["scene_search"], true);
}
