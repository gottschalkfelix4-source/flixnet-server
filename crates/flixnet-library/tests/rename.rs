//! Milestone 2.7 gate: watch state survives renames, folder renames and matching;
//! the file watcher queues a scan when something changes.

use std::path::Path;
use std::time::Duration;

use flixnet_core::events::EventBus;
use flixnet_core::jobs::JobRuntime;
use flixnet_db::metadata::set_provider_id;
use flixnet_db::{Db, items, jobs, libraries, media, userdata, users};
use flixnet_library::scan::scan_library;
use flixnet_library::watch::LibraryWatcher;

fn write(path: &Path, content: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

#[tokio::test]
async fn watch_state_survives_renames_moves_and_matching() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("flixnet.db")).await.unwrap();
    let queue = JobRuntime::new(db.clone(), EventBus::new())
        .handle()
        .queue()
        .clone();
    let user = users::create_first_admin(&db, "Felix", "phc")
        .await
        .unwrap()
        .unwrap();

    let root = dir.path().join("Filme");
    let original = root.join("Alpha (2020)/Alpha.2020.1080p.mkv");
    write(&original, b"some video bytes");
    write(
        &root.join("Beta (2021)/Beta.2021.mkv"),
        b"other video bytes, longer",
    );
    let library = libraries::create(
        &db,
        "Filme",
        "movies",
        &[root.to_string_lossy().into_owned()],
    )
    .await
    .unwrap();
    scan_library(&db, &queue, library.id).await.unwrap();

    let alpha = items::list_by_kind(&db, library.id, "movie")
        .await
        .unwrap()
        .into_iter()
        .find(|m| m.title == "Alpha")
        .unwrap();
    userdata::set_played(&db, user.id, alpha.id, true)
        .await
        .unwrap();
    userdata::set_favorite(&db, user.id, alpha.id, true)
        .await
        .unwrap();
    let file_id = media::files_for_item(&db, alpha.id).await.unwrap()[0].id;

    // 1. Rename the file inside its folder.
    let renamed = root.join("Alpha (2020)/Alpha (2020) - Bluray.mkv");
    std::fs::rename(&original, &renamed).unwrap();
    let report = scan_library(&db, &queue, library.id).await.unwrap();
    assert_eq!(
        (report.moved, report.added, report.missing),
        (1, 0, 0),
        "{report:?}"
    );
    let file = media::get_file(&db, file_id).await.unwrap().unwrap();
    assert!(file.path.ends_with("Alpha (2020) - Bluray.mkv"));
    assert_eq!(file.item_id, Some(alpha.id), "same item");
    let data = userdata::get(&db, user.id, alpha.id).await.unwrap();
    assert!(data.played && data.favorite, "watch state kept: {data:?}");

    // 2. Rename the whole folder.
    let new_folder = root.join("Alpha - Director's Cut (2020)");
    std::fs::rename(root.join("Alpha (2020)"), &new_folder).unwrap();
    let report = scan_library(&db, &queue, library.id).await.unwrap();
    assert_eq!((report.moved, report.added), (1, 0), "{report:?}");
    assert!(userdata::get(&db, user.id, alpha.id).await.unwrap().played);
    assert_eq!(
        items::list_by_kind(&db, library.id, "movie")
            .await
            .unwrap()
            .len(),
        2,
        "no duplicate item"
    );

    // 3. Matching changes the identity to a provider key; data moves along.
    set_provider_id(&db, alpha.id, "tmdb", "4242")
        .await
        .unwrap();
    userdata::update_identity(&db, alpha.id).await.unwrap();
    assert_eq!(
        userdata::identity_key(&db, alpha.id)
            .await
            .unwrap()
            .as_deref(),
        Some("tmdb:movie:4242")
    );
    assert!(userdata::get(&db, user.id, alpha.id).await.unwrap().played);

    // 4. Replacing the file with a different version (new size/mtime) under a new
    //    folder name creates a new local item — but once matched to the same TMDB id,
    //    it shares the watch state.
    std::fs::remove_dir_all(&new_folder).unwrap();
    write(
        &root.join("Alpha Die Rueckkehr (2020)/Alpha.Die.Rueckkehr.2020.2160p.mkv"),
        b"a completely different 4k file",
    );
    scan_library(&db, &queue, library.id).await.unwrap();
    let remastered = items::list_by_kind(&db, library.id, "movie")
        .await
        .unwrap()
        .into_iter()
        .find(|m| m.title == "Alpha Die Rueckkehr")
        .unwrap();
    assert!(
        !userdata::get(&db, user.id, remastered.id)
            .await
            .unwrap()
            .played,
        "not matched yet"
    );
    set_provider_id(&db, remastered.id, "tmdb", "4242")
        .await
        .unwrap();
    userdata::update_identity(&db, remastered.id).await.unwrap();
    assert!(
        userdata::get(&db, user.id, remastered.id)
            .await
            .unwrap()
            .played,
        "same movie, same history"
    );
}

#[tokio::test]
async fn episodes_follow_the_series_identity() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("flixnet.db")).await.unwrap();
    let queue = JobRuntime::new(db.clone(), EventBus::new())
        .handle()
        .queue()
        .clone();
    let user = users::create_first_admin(&db, "Felix", "phc")
        .await
        .unwrap()
        .unwrap();
    let root = dir.path().join("Serien");
    write(&root.join("Dark (2017)/Staffel 1/Dark.S01E01.mkv"), b"e1");
    write(
        &root.join("Dark (2017)/Staffel 1/Dark.S01E02.mkv"),
        b"e2-longer",
    );
    let library = libraries::create(
        &db,
        "Serien",
        "shows",
        &[root.to_string_lossy().into_owned()],
    )
    .await
    .unwrap();
    scan_library(&db, &queue, library.id).await.unwrap();

    let series = &items::list_by_kind(&db, library.id, "series")
        .await
        .unwrap()[0];
    userdata::set_played(&db, user.id, series.id, true)
        .await
        .unwrap();
    let episodes = items::list_by_kind(&db, library.id, "episode")
        .await
        .unwrap();
    assert!(
        userdata::get(&db, user.id, episodes[0].id)
            .await
            .unwrap()
            .played,
        "marking a series marks its episodes"
    );

    set_provider_id(&db, series.id, "tmdb", "70523")
        .await
        .unwrap();
    userdata::update_identity(&db, series.id).await.unwrap();
    assert_eq!(
        userdata::identity_key(&db, episodes[1].id)
            .await
            .unwrap()
            .as_deref(),
        Some("tmdb:tv:70523:s1e2")
    );
    assert!(
        userdata::get(&db, user.id, episodes[1].id)
            .await
            .unwrap()
            .played
    );
}

#[tokio::test]
async fn watcher_queues_a_scan_after_changes() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("flixnet.db")).await.unwrap();
    let queue = JobRuntime::new(db.clone(), EventBus::new())
        .handle()
        .queue()
        .clone();
    let root = dir.path().join("Filme");
    std::fs::create_dir_all(&root).unwrap();
    libraries::create(
        &db,
        "Filme",
        "movies",
        &[root.to_string_lossy().into_owned()],
    )
    .await
    .unwrap();

    let watcher = LibraryWatcher::start(queue, Duration::from_millis(300));
    watcher.sync(&db).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    write(&root.join("Neu (2024)/Neu.2024.mkv"), b"new");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let queued = jobs::list(&db, Some("queued"), 10).await.unwrap();
        if queued.iter().any(|j| j.kind == "library.scan") {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no scan was queued after the change"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
