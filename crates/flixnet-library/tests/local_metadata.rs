//! Milestone 2.4: NFO files and local artwork become item metadata, and NFO values
//! are locked against provider updates.

use std::path::Path;

use flixnet_core::events::EventBus;
use flixnet_core::jobs::JobRuntime;
use flixnet_db::metadata::{self, Authority, ItemFields};
use flixnet_db::{Db, items, jobs, libraries};
use flixnet_library::jobs::RefreshPayload;
use flixnet_library::refresh::refresh_local;
use flixnet_library::scan::scan_library;

fn write(root: &Path, relative: &str, content: &[u8]) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

/// Runs all queued metadata refresh jobs.
async fn run_refreshes(db: &Db) -> usize {
    let mut ran = 0;
    while let Some(job) = jobs::claim(db, "io").await.unwrap() {
        if job.kind == "metadata.refresh" {
            let payload: RefreshPayload = serde_json::from_str(&job.payload).unwrap();
            refresh_local(db, payload.item_id).await.unwrap();
            ran += 1;
        }
        jobs::complete(db, job.id).await.unwrap();
    }
    ran
}

const MOVIE_NFO: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<movie>
  <title>Der Pate</title>
  <originaltitle>The Godfather</originaltitle>
  <year>1972</year>
  <plot>Die Geschichte der Corleones.</plot>
  <runtime>175</runtime>
  <mpaa>FSK 16</mpaa>
  <rating>9.2</rating>
  <uniqueid type="tmdb">238</uniqueid>
  <uniqueid type="imdb">tt0068646</uniqueid>
  <genre>Drama</genre><genre>Krimi</genre>
  <studio>Paramount</studio>
  <director>Francis Ford Coppola</director>
  <actor><name>Marlon Brando</name><role>Vito Corleone</role><order>0</order></actor>
  <actor><name>Al Pacino</name><role>Michael Corleone</role><order>1</order></actor>
</movie>"#;

#[tokio::test]
async fn nfo_and_artwork_fill_items_and_lock_fields() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("flixnet.db")).await.unwrap();
    let queue = JobRuntime::new(db.clone(), EventBus::new())
        .handle()
        .queue()
        .clone();

    let movies_root = dir.path().join("Filme");
    // The video content does not matter for metadata.
    write(
        &movies_root,
        "The Godfather (1972)/The.Godfather.1972.1080p.mkv",
        b"video",
    );
    write(
        &movies_root,
        "The Godfather (1972)/movie.nfo",
        MOVIE_NFO.as_bytes(),
    );
    write(&movies_root, "The Godfather (1972)/poster.jpg", b"img");
    write(&movies_root, "The Godfather (1972)/fanart.jpg", b"img");
    write(&movies_root, "The Godfather (1972)/clearlogo.png", b"img");
    // A loose movie in the root must not take the library's own folder.jpg.
    write(&movies_root, "Loose.Movie.2010.mkv", b"video");
    write(&movies_root, "folder.jpg", b"img");

    let shows_root = dir.path().join("Serien");
    write(&shows_root, "Dark (2017)/tvshow.nfo",
        b"<tvshow><title>Dark</title><plot>Winden.</plot><uniqueid type=\"tvdb\">334824</uniqueid><genre>Mystery</genre></tvshow>");
    write(&shows_root, "Dark (2017)/poster.jpg", b"img");
    write(&shows_root, "Dark (2017)/season01-poster.jpg", b"img");
    write(
        &shows_root,
        "Dark (2017)/Staffel 1/Dark.S01E01.mkv",
        b"video",
    );
    write(&shows_root, "Dark (2017)/Staffel 1/Dark.S01E01.nfo",
        b"<episodedetails><title>Geheimnisse</title><plot>Ein Junge verschwindet.</plot><aired>2017-12-01</aired></episodedetails>");
    write(
        &shows_root,
        "Dark (2017)/Staffel 1/Dark.S01E01-thumb.jpg",
        b"img",
    );

    let movies = libraries::create(
        &db,
        "Filme",
        "movies",
        &[movies_root.to_string_lossy().into_owned()],
    )
    .await
    .unwrap();
    let shows = libraries::create(
        &db,
        "Serien",
        "shows",
        &[shows_root.to_string_lossy().into_owned()],
    )
    .await
    .unwrap();
    let report = scan_library(&db, &queue, movies.id).await.unwrap();
    assert_eq!(report.refreshes_queued, 2);
    let report = scan_library(&db, &queue, shows.id).await.unwrap();
    assert_eq!(report.refreshes_queued, 3, "series, season and episode");
    assert_eq!(run_refreshes(&db).await, 5);

    // --- Movie from NFO ---------------------------------------------------------
    let movie_items = items::list_by_kind(&db, movies.id, "movie").await.unwrap();
    let pate = movie_items
        .iter()
        .find(|m| m.title == "Der Pate")
        .expect("NFO title applied");
    assert_eq!(pate.year, Some(1972));
    assert_eq!(pate.sort_title, "pate");
    let ids = items::provider_ids(&db, pate.id).await.unwrap();
    assert_eq!(
        ids,
        [
            ("imdb".into(), "tt0068646".into()),
            ("tmdb".into(), "238".into())
        ]
    );
    assert_eq!(
        metadata::genres(&db, pate.id).await.unwrap(),
        ["Drama", "Krimi"]
    );
    assert_eq!(
        metadata::studios(&db, pate.id).await.unwrap(),
        ["Paramount"]
    );
    let credits = metadata::credits(&db, pate.id).await.unwrap();
    let cast: Vec<_> = credits
        .iter()
        .map(|c| (c.role.as_str(), c.name.as_str(), c.character.as_deref()))
        .collect();
    assert_eq!(
        cast,
        [
            ("actor", "Marlon Brando", Some("Vito Corleone")),
            ("actor", "Al Pacino", Some("Michael Corleone")),
            ("director", "Francis Ford Coppola", None),
        ]
    );
    let images = metadata::images(&db, pate.id).await.unwrap();
    let kinds: Vec<_> = images.iter().map(|i| i.kind.as_str()).collect();
    assert_eq!(kinds, ["backdrop", "logo", "poster"]);

    let loose = movie_items
        .iter()
        .find(|m| m.title == "Loose Movie")
        .unwrap();
    assert!(
        metadata::images(&db, loose.id).await.unwrap().is_empty(),
        "library folder.jpg is not its poster"
    );

    // --- NFO values are locked against providers --------------------------------
    let locked = metadata::locked_fields(&db, pate.id).await.unwrap();
    assert!(locked.contains(&"title".to_owned()) && locked.contains(&"overview".to_owned()));
    let written = metadata::apply_fields(
        &db,
        pate.id,
        &ItemFields {
            title: Some("The Godfather".into()),
            tagline: Some("An offer you can't refuse.".into()),
            ..Default::default()
        },
        Authority::Provider,
    )
    .await
    .unwrap();
    assert_eq!(written, ["tagline"], "only the unlocked field is filled");
    assert_eq!(
        items::get(&db, pate.id).await.unwrap().unwrap().title,
        "Der Pate"
    );

    // --- Series, season, episode ------------------------------------------------
    let series = &items::list_by_kind(&db, shows.id, "series").await.unwrap()[0];
    assert_eq!(metadata::genres(&db, series.id).await.unwrap(), ["Mystery"]);
    assert_eq!(
        items::provider_ids(&db, series.id).await.unwrap(),
        [("tvdb".into(), "334824".into())]
    );
    assert_eq!(
        metadata::images(&db, series.id).await.unwrap()[0].kind,
        "poster"
    );
    let season = &items::list_by_kind(&db, shows.id, "season").await.unwrap()[0];
    assert_eq!(metadata::images(&db, season.id).await.unwrap().len(), 1);
    let episode = &items::list_by_kind(&db, shows.id, "episode").await.unwrap()[0];
    let episode_row = items::get(&db, episode.id).await.unwrap().unwrap();
    assert_eq!(episode_row.title, "Geheimnisse");
    assert_eq!(episode_row.premiere_date.as_deref(), Some("2017-12-01"));
    assert_eq!(
        metadata::images(&db, episode.id).await.unwrap()[0].kind,
        "thumb"
    );

    // Refreshing again is stable (no duplicate images or credits).
    refresh_local(&db, pate.id).await.unwrap();
    assert_eq!(metadata::images(&db, pate.id).await.unwrap().len(), 3);
    assert_eq!(metadata::credits(&db, pate.id).await.unwrap().len(), 3);
}
