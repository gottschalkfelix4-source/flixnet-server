//! Dashboard statistics: plays, watch time per kind, history buckets, top lists.

use flixnet_db::users::{self, NewUser};
use flixnet_db::{Db, libraries, music, now_ms, stats};
use tempfile::TempDir;

const MIN: i64 = 60_000;
const DAY: i64 = 24 * 60 * MIN;

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

/// An item of `kind`; tracks and episodes hang off `series` (album artist / series).
async fn item(
    db: &Db,
    library: i64,
    kind: &str,
    title: &str,
    series: Option<i64>,
    runtime: i64,
) -> i64 {
    let key = format!("test:{kind}:{title}");
    let (id,): (i64,) = sqlx::query_as(
        "INSERT INTO items (library_id, kind, series_id, title, sort_title, runtime_ms,
                            content_key, identity_key, date_added, date_modified)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(library)
    .bind(kind)
    .bind(series)
    .bind(title)
    .bind(title.to_lowercase())
    .bind(runtime)
    .bind(&key)
    .bind(&key)
    .bind(now_ms())
    .bind(now_ms())
    .fetch_one(db.writer())
    .await
    .unwrap();
    if kind == "series" || kind == "artist" {
        sqlx::query("UPDATE items SET series_id = id WHERE id = ?")
            .bind(id)
            .execute(db.writer())
            .await
            .unwrap();
    }
    id
}

/// One playback session: a start event at `from` and a stop event at `to` (positions in ms).
async fn session(db: &Db, user: i64, item: i64, session: &str, at: i64, from: i64, to: i64) {
    for (event, position, offset) in [("start", from, 0), ("stop", to, to - from)] {
        sqlx::query(
            "INSERT INTO watch_events (user_id, identity_key, item_id, event, position_ms,
                                       completed, session_id, created_at)
             SELECT ?, identity_key, id, ?, ?, 0, ?, ? FROM items WHERE id = ?",
        )
        .bind(user)
        .bind(event)
        .bind(position)
        .bind(session)
        .bind(at + offset)
        .bind(item)
        .execute(db.writer())
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn plays_watch_time_history_and_top_lists() {
    let (_dir, db) = open().await;
    let movies = libraries::create(&db, "Filme", "movies", &[])
        .await
        .unwrap()
        .id;
    let shows = libraries::create(&db, "Serien", "shows", &[])
        .await
        .unwrap()
        .id;
    let tunes = libraries::create(&db, "Musik", "music", &[])
        .await
        .unwrap()
        .id;
    let felix = user(&db, "Felix").await;
    let anna = user(&db, "Anna").await;

    let matrix = item(&db, movies, "movie", "Matrix", None, 120 * MIN).await;
    let alien = item(&db, movies, "movie", "Alien", None, 110 * MIN).await;
    let dark = item(&db, shows, "series", "Dark", None, 0).await;
    let e1 = item(&db, shows, "episode", "Dark 1", Some(dark), 50 * MIN).await;
    let e2 = item(&db, shows, "episode", "Dark 2", Some(dark), 50 * MIN).await;
    let artist = item(&db, tunes, "artist", "Daft Punk", None, 0).await;
    let track = item(&db, tunes, "track", "One More Time", Some(artist), 5 * MIN).await;

    let now = now_ms();
    let since = now - 7 * DAY;
    // Felix: Matrix fully, both Dark episodes; Anna: Matrix halfway, Alien a bit.
    session(&db, felix, matrix, "a", now - 6 * DAY, 0, 120 * MIN).await;
    session(&db, felix, e1, "b", now - 2 * DAY, 0, 50 * MIN).await;
    session(
        &db,
        felix,
        e2,
        "c",
        now - 2 * DAY + 60 * MIN,
        10 * MIN,
        50 * MIN,
    )
    .await;
    session(&db, anna, matrix, "d", now - DAY, 30 * MIN, 90 * MIN).await;
    session(&db, anna, alien, "e", now - DAY, 0, 20 * MIN).await;
    // Anna listens to a track twice (music is logged as single completed events).
    assert!(music::record_play(&db, anna, track).await.unwrap());
    assert!(music::record_play(&db, anna, track).await.unwrap());
    // Something older than the period does not count.
    session(&db, anna, alien, "old", now - 30 * DAY, 0, 100 * MIN).await;
    // A position jump beyond the runtime (seek past the end) is capped at the runtime.
    session(&db, felix, alien, "f", now - 3 * DAY, 0, 500 * MIN).await;

    let users = stats::top_users(&db, since, 10).await.unwrap();
    assert_eq!(users.len(), 2);
    let f = &users[0];
    assert_eq!((f.name.as_str(), f.plays), ("Felix", 4));
    assert_eq!(
        f.movies_ms,
        120 * MIN + 110 * MIN,
        "Alien capped at its runtime"
    );
    assert_eq!(f.tv_ms, 50 * MIN + 40 * MIN);
    assert_eq!(f.watch_ms, f.movies_ms + f.tv_ms);
    let a = &users[1];
    assert_eq!((a.name.as_str(), a.plays), ("Anna", 4));
    assert_eq!(a.movies_ms, 60 * MIN + 20 * MIN);
    assert_eq!(a.music_ms, 10 * MIN);

    let movies_top = stats::top_movies(&db, since, 10).await.unwrap();
    assert_eq!(
        movies_top
            .iter()
            .map(|t| (t.id, t.plays, t.users))
            .collect::<Vec<_>>(),
        [(matrix, 2, 2), (alien, 2, 2)]
    );
    let series_top = stats::top_series(&db, since, 10).await.unwrap();
    assert_eq!(
        series_top
            .iter()
            .map(|t| (t.id, t.plays, t.users))
            .collect::<Vec<_>>(),
        [(dark, 2, 1)]
    );
    let artists_top = stats::top_artists(&db, since, 10).await.unwrap();
    assert_eq!(
        artists_top
            .iter()
            .map(|t| (t.id, t.plays, t.users))
            .collect::<Vec<_>>(),
        [(artist, 2, 1)]
    );

    // Daily buckets over the week: continuous, totals add up.
    let history = stats::history(&db, since, now, DAY).await.unwrap();
    assert_eq!(
        history.len(),
        8,
        "seven full days plus the bucket containing now"
    );
    assert!(history.windows(2).all(|w| w[1].start - w[0].start == DAY));
    let total: i64 = history
        .iter()
        .map(|b| b.movies_ms + b.tv_ms + b.music_ms + b.other_ms)
        .sum();
    assert_eq!(total, f.watch_ms + a.watch_ms);
    assert_eq!(
        history[1].movies_ms,
        120 * MIN,
        "Matrix six days ago: second day of the week"
    );
}
