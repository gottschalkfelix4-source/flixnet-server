//! Resume points, watched thresholds, watch history, "Weiterschauen" and "Nächste Folge".

use flixnet_db::playstate::{self, NewWatchEvent, Progress, SeriesNext, Visibility};
use flixnet_db::users::{self, NewUser};
use flixnet_db::{Db, libraries, now_ms, userdata};
use tempfile::TempDir;

const MIN: i64 = 60_000;

async fn open() -> (TempDir, Db) {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("flixnet.db")).await.unwrap();
    (dir, db)
}

async fn user(db: &Db, name: &str) -> i64 {
    users::create(
        db,
        &NewUser {
            name,
            password_hash: None,
            pin_hash: None,
            is_admin: false,
            is_hidden: false,
            max_age_rating: None,
        },
    )
    .await
    .unwrap()
    .id
}

#[derive(Default)]
struct Seed<'a> {
    kind: &'a str,
    title: &'a str,
    parent: Option<i64>,
    series: Option<i64>,
    season: Option<i64>,
    index: Option<i64>,
    index_end: Option<i64>,
    runtime_ms: Option<i64>,
    age: Option<i64>,
}

async fn insert(db: &Db, library: i64, s: Seed<'_>) -> i64 {
    let key = format!("test:{}:{}:{:?}:{:?}", s.kind, s.title, s.season, s.index);
    let (id,): (i64,) = sqlx::query_as(
        "INSERT INTO items (library_id, kind, parent_id, series_id, title, sort_title, index_number,
                            index_end, parent_index_number, runtime_ms, age_rating, content_key,
                            identity_key, date_added, date_modified)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(library)
    .bind(s.kind)
    .bind(s.parent)
    .bind(s.series)
    .bind(s.title)
    .bind(s.title.to_lowercase())
    .bind(s.index)
    .bind(s.index_end)
    .bind(s.season)
    .bind(s.runtime_ms)
    .bind(s.age)
    .bind(&key)
    .bind(&key)
    .bind(now_ms())
    .bind(now_ms())
    .fetch_one(db.writer())
    .await
    .unwrap();
    if s.kind == "series" {
        sqlx::query("UPDATE items SET series_id = id WHERE id = ?")
            .bind(id)
            .execute(db.writer())
            .await
            .unwrap();
    }
    id
}

async fn movie(db: &Db, library: i64, title: &str, runtime_ms: i64) -> i64 {
    insert(
        db,
        library,
        Seed {
            kind: "movie",
            title,
            runtime_ms: Some(runtime_ms),
            ..Default::default()
        },
    )
    .await
}

/// A series with seasons; `episodes` per season as (season, first, last) — a range
/// means one multi-episode file.
struct Show {
    id: i64,
    /// (season, episode) → item id (multi-episode files under their first number).
    episodes: Vec<((i64, i64), i64)>,
}

impl Show {
    fn ep(&self, season: i64, episode: i64) -> i64 {
        self.episodes
            .iter()
            .find(|(k, _)| *k == (season, episode))
            .map(|(_, id)| *id)
            .unwrap_or_else(|| panic!("no S{season}E{episode}"))
    }
}

async fn show(
    db: &Db,
    library: i64,
    title: &str,
    age: Option<i64>,
    files: &[(i64, i64, i64)],
) -> Show {
    let series = insert(
        db,
        library,
        Seed {
            kind: "series",
            title,
            age,
            ..Default::default()
        },
    )
    .await;
    let mut seasons = std::collections::HashMap::new();
    let mut episodes = Vec::new();
    for &(season, first, last) in files {
        let season_id = match seasons.get(&season) {
            Some(id) => *id,
            None => {
                let id = insert(
                    db,
                    library,
                    Seed {
                        kind: "season",
                        title: &format!("{title} Staffel {season}"),
                        parent: Some(series),
                        series: Some(series),
                        index: Some(season),
                        ..Default::default()
                    },
                )
                .await;
                seasons.insert(season, id);
                id
            }
        };
        let id = insert(
            db,
            library,
            Seed {
                kind: "episode",
                title: &format!("{title} S{season}E{first}"),
                parent: Some(season_id),
                series: Some(series),
                season: Some(season),
                index: Some(first),
                index_end: (last > first).then_some(last),
                runtime_ms: Some(40 * MIN),
                ..Default::default()
            },
        )
        .await;
        episodes.push(((season, first), id));
    }
    Show {
        id: series,
        episodes,
    }
}

async fn watch(db: &Db, user: i64, item: i64) {
    let progress = playstate::save_stop(db, user, item, 39 * MIN, Some(40 * MIN))
        .await
        .unwrap();
    assert_eq!(progress, Progress::Played);
}

#[tokio::test]
async fn stops_set_resume_points_or_mark_watched() {
    let (_dir, db) = open().await;
    let lib = libraries::create(&db, "Filme", "movies", &[])
        .await
        .unwrap()
        .id;
    let felix = user(&db, "Felix").await;
    let film = movie(&db, lib, "Alpha", 100 * MIN).await;
    let duration = playstate::item_duration(&db, film).await.unwrap();
    assert_eq!(duration, Some(100 * MIN), "runtime when no file is probed");

    // A very early stop leaves nothing behind.
    let early = playstate::save_stop(&db, felix, film, MIN, duration)
        .await
        .unwrap();
    assert_eq!(early, Progress::TooEarly);
    assert_eq!(
        userdata::get(&db, felix, film).await.unwrap(),
        Default::default()
    );

    // Progress in the middle becomes the resume point …
    playstate::save_progress(&db, felix, film, 30 * MIN, duration)
        .await
        .unwrap();
    let data = userdata::get(&db, felix, film).await.unwrap();
    assert_eq!(data.position_ms, 30 * MIN);
    assert!(!data.played);
    assert!(data.last_played_at.is_some());

    // … an early position afterwards (restart from the beginning, quit) keeps it …
    playstate::save_stop(&db, felix, film, 30_000, duration)
        .await
        .unwrap();
    assert_eq!(
        userdata::get(&db, felix, film).await.unwrap().position_ms,
        30 * MIN
    );
    // … progress inside the end zone does not mark anything yet …
    playstate::save_progress(&db, felix, film, 95 * MIN, duration)
        .await
        .unwrap();
    assert!(!userdata::get(&db, felix, film).await.unwrap().played);

    // … and a stop there counts as watched and clears the resume point.
    let end = playstate::save_stop(&db, felix, film, 92 * MIN, duration)
        .await
        .unwrap();
    assert_eq!(end, Progress::Played);
    let data = userdata::get(&db, felix, film).await.unwrap();
    assert!(data.played);
    assert_eq!(data.play_count, 1);
    assert_eq!(data.position_ms, 0);

    // Watching again counts another play.
    playstate::save_stop(&db, felix, film, 99 * MIN, duration)
        .await
        .unwrap();
    assert_eq!(userdata::get(&db, felix, film).await.unwrap().play_count, 2);

    // Others are unaffected.
    let anna = user(&db, "Anna").await;
    assert_eq!(
        userdata::get(&db, anna, film).await.unwrap(),
        Default::default()
    );
}

#[tokio::test]
async fn watch_events_are_appended_with_identity() {
    let (_dir, db) = open().await;
    let lib = libraries::create(&db, "Filme", "movies", &[])
        .await
        .unwrap()
        .id;
    let felix = user(&db, "Felix").await;
    let film = movie(&db, lib, "Alpha", 100 * MIN).await;
    for (event, position, completed) in [("start", 0, false), ("stop", 95 * MIN, true)] {
        playstate::record_event(
            &db,
            &NewWatchEvent {
                user_id: felix,
                item_id: film,
                event,
                position_ms: position,
                duration_ms: Some(100 * MIN),
                play_method: Some("direct_play"),
                completed,
                session_id: Some("abc"),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    }
    let events = playstate::events(&db, felix).await.unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].event, "start");
    assert_eq!(events[1].event, "stop");
    assert!(events[1].completed);
    assert_eq!(events[1].identity_key, "test:movie:Alpha:None:None");
    assert_eq!(events[1].item_id, Some(film));
    assert_eq!(events[1].play_method.as_deref(), Some("direct_play"));
}

#[tokio::test]
async fn resume_list_is_newest_first_and_respects_visibility() {
    let (_dir, db) = open().await;
    let lib = libraries::create(&db, "Filme", "movies", &[])
        .await
        .unwrap()
        .id;
    let private = libraries::create(&db, "Privat", "movies", &[])
        .await
        .unwrap()
        .id;
    let felix = user(&db, "Felix").await;
    let alpha = movie(&db, lib, "Alpha", 100 * MIN).await;
    let beta = movie(&db, lib, "Beta", 100 * MIN).await;
    let gamma = movie(&db, lib, "Gamma", 100 * MIN).await;
    let hidden = movie(&db, private, "Geheim", 100 * MIN).await;
    for (film, position) in [
        (alpha, 20 * MIN),
        (hidden, 20 * MIN),
        (beta, 30 * MIN),
        (gamma, 40 * MIN),
    ] {
        playstate::save_progress(&db, felix, film, position, Some(100 * MIN))
            .await
            .unwrap();
        // Distinct timestamps for a stable order.
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    // Finished: no longer in the list.
    watch(&db, felix, gamma).await;
    sqlx::query("UPDATE items SET age_rating = 18 WHERE id = ?")
        .bind(beta)
        .execute(db.writer())
        .await
        .unwrap();

    let all = Visibility {
        library_ids: vec![lib],
        restriction: None.into(),
    };
    assert_eq!(
        playstate::resume_items(&db, felix, &all, 20).await.unwrap(),
        [beta, alpha]
    );
    let kids = Visibility {
        library_ids: vec![lib],
        restriction: Some(12).into(),
    };
    assert!(
        playstate::resume_items(&db, felix, &kids, 20)
            .await
            .unwrap()
            .is_empty(),
        "age limit hides beta; alpha is unrated and hidden as well"
    );
    assert_eq!(
        playstate::resume_items(&db, felix, &all, 1).await.unwrap(),
        [beta]
    );
}

#[tokio::test]
async fn next_up_follows_the_furthest_watched_episode() {
    let (_dir, db) = open().await;
    let lib = libraries::create(&db, "Serien", "shows", &[])
        .await
        .unwrap()
        .id;
    let felix = user(&db, "Felix").await;
    // Season 0 (specials), season 1 with a double episode E3–E4, season 2.
    let dark = show(
        &db,
        lib,
        "Dark",
        None,
        &[
            (0, 1, 1),
            (1, 1, 1),
            (1, 2, 2),
            (1, 3, 4),
            (1, 5, 5),
            (2, 1, 1),
            (2, 2, 2),
        ],
    )
    .await;
    let all = Visibility {
        library_ids: vec![lib],
        restriction: None.into(),
    };
    let since = now_ms() - playstate::NEXT_UP_MAX_AGE_MS;

    // Nothing watched: the series' first regular episode, but no "Nächste Folge".
    assert_eq!(
        playstate::series_next(&db, felix, dark.id).await.unwrap(),
        Some(SeriesNext {
            episode_id: dark.ep(1, 1),
            position_ms: 0,
            after_watched: false,
        })
    );
    assert!(
        playstate::next_up(&db, felix, &all, since, 20)
            .await
            .unwrap()
            .is_empty()
    );

    // A special does not count as progress in the series.
    watch(&db, felix, dark.ep(0, 1)).await;
    assert!(
        playstate::next_up(&db, felix, &all, since, 20)
            .await
            .unwrap()
            .is_empty()
    );

    watch(&db, felix, dark.ep(1, 1)).await;
    assert_eq!(
        playstate::next_up(&db, felix, &all, since, 20)
            .await
            .unwrap(),
        [dark.ep(1, 2)]
    );

    // The double episode E3–E4 is next after E2; after it comes E5.
    watch(&db, felix, dark.ep(1, 2)).await;
    assert_eq!(
        playstate::next_up(&db, felix, &all, since, 20)
            .await
            .unwrap(),
        [dark.ep(1, 3)]
    );
    watch(&db, felix, dark.ep(1, 3)).await;
    assert_eq!(
        playstate::next_up(&db, felix, &all, since, 20)
            .await
            .unwrap(),
        [dark.ep(1, 5)]
    );

    // Season boundaries: after S1E5 comes S2E1.
    watch(&db, felix, dark.ep(1, 5)).await;
    assert_eq!(
        playstate::next_up(&db, felix, &all, since, 20)
            .await
            .unwrap(),
        [dark.ep(2, 1)]
    );

    // Started but not finished: it is in "Weiterschauen" instead.
    playstate::save_progress(&db, felix, dark.ep(2, 1), 10 * MIN, Some(40 * MIN))
        .await
        .unwrap();
    assert!(
        playstate::next_up(&db, felix, &all, since, 20)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        playstate::resume_items(&db, felix, &all, 20).await.unwrap(),
        [dark.ep(2, 1)]
    );
    let next = playstate::series_next(&db, felix, dark.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(next.episode_id, dark.ep(2, 1));
    assert_eq!(next.position_ms, 10 * MIN, "the play button resumes it");

    // Skipping ahead: the furthest watched episode counts, earlier gaps don't.
    watch(&db, felix, dark.ep(2, 2)).await;
    assert_eq!(
        playstate::series_next(&db, felix, dark.id).await.unwrap(),
        None,
        "everything after the furthest episode is watched"
    );
    assert_eq!(
        playstate::first_episode(&db, dark.id).await.unwrap(),
        Some(dark.ep(1, 1)),
        "starting over begins with the first regular episode"
    );
}

#[tokio::test]
async fn next_up_orders_series_by_activity_and_drops_stale_ones() {
    let (_dir, db) = open().await;
    let lib = libraries::create(&db, "Serien", "shows", &[])
        .await
        .unwrap()
        .id;
    let felix = user(&db, "Felix").await;
    let files = [(1, 1, 1), (1, 2, 2)];
    let a = show(&db, lib, "Alpha", Some(12), &files).await;
    let b = show(&db, lib, "Beta", Some(16), &files).await;
    let c = show(&db, lib, "Gamma", Some(6), &files).await;
    for s in [&a, &b, &c] {
        watch(&db, felix, s.ep(1, 1)).await;
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    // Gamma was last watched long ago.
    sqlx::query(
        "UPDATE user_item_data SET last_played_at = ? WHERE identity_key = (SELECT identity_key FROM items WHERE id = ?)",
    )
    .bind(now_ms() - playstate::NEXT_UP_MAX_AGE_MS - 1)
    .bind(c.ep(1, 1))
    .execute(db.writer())
    .await
    .unwrap();

    let since = now_ms() - playstate::NEXT_UP_MAX_AGE_MS;
    let all = Visibility {
        library_ids: vec![lib],
        restriction: None.into(),
    };
    assert_eq!(
        playstate::next_up(&db, felix, &all, since, 20)
            .await
            .unwrap(),
        [b.ep(1, 2), a.ep(1, 2)],
        "most recent first, stale series left out"
    );
    let teen = Visibility {
        library_ids: vec![lib],
        restriction: Some(12).into(),
    };
    assert_eq!(
        playstate::next_up(&db, felix, &teen, since, 20)
            .await
            .unwrap(),
        [a.ep(1, 2)],
        "episodes inherit the series' age rating"
    );
    let none = Visibility {
        library_ids: vec![],
        restriction: None.into(),
    };
    assert!(
        playstate::next_up(&db, felix, &none, since, 20)
            .await
            .unwrap()
            .is_empty()
    );
    // Another user has their own next-up.
    let anna = user(&db, "Anna").await;
    assert!(
        playstate::next_up(&db, anna, &all, since, 20)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn next_episode_for_autoplay() {
    let (_dir, db) = open().await;
    let lib = libraries::create(&db, "Serien", "shows", &[])
        .await
        .unwrap()
        .id;
    let s = show(
        &db,
        lib,
        "Dark",
        None,
        &[(0, 1, 1), (0, 2, 2), (1, 1, 2), (1, 3, 3), (2, 1, 1)],
    )
    .await;
    let next = |id| playstate::next_episode(&db, id);
    assert_eq!(
        next(s.ep(1, 1)).await.unwrap(),
        Some(s.ep(1, 3)),
        "after E1–E2"
    );
    assert_eq!(next(s.ep(1, 3)).await.unwrap(), Some(s.ep(2, 1)));
    assert_eq!(next(s.ep(2, 1)).await.unwrap(), None, "the last episode");
    assert_eq!(next(s.ep(0, 1)).await.unwrap(), Some(s.ep(0, 2)));
    assert_eq!(
        next(s.ep(0, 2)).await.unwrap(),
        None,
        "specials stay specials"
    );
    assert_eq!(next(s.id).await.unwrap(), None, "not an episode");
}
