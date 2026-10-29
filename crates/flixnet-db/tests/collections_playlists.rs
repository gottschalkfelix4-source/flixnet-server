//! Milestone 5.3: collections, playlists and smart-playlist filters at the query level.

use flixnet_db::browse::{self, ItemFilter, Sort};
use flixnet_db::collections::{self, ProviderCollection};
use flixnet_db::users::{self, NewUser};
use flixnet_db::{Db, libraries, metadata, now_ms, playlists, userdata};
use tempfile::TempDir;

async fn open() -> (TempDir, Db) {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("flixnet.db")).await.unwrap();
    (dir, db)
}

/// Inserts an item; `identity` doubles as content key.
async fn item(
    db: &Db,
    library: i64,
    kind: &str,
    title: &str,
    premiere: Option<&str>,
    age: Option<i64>,
    identity: &str,
) -> i64 {
    let (id,): (i64,) = sqlx::query_as(
        "INSERT INTO items (library_id, kind, title, sort_title, year, premiere_date, age_rating,
                            content_key, identity_key, date_added, date_modified)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(library)
    .bind(kind)
    .bind(title)
    .bind(title.to_lowercase())
    .bind(premiere.and_then(|p| p[..4].parse::<i64>().ok()))
    .bind(premiere)
    .bind(age)
    .bind(identity)
    .bind(identity)
    .bind(now_ms())
    .bind(now_ms())
    .fetch_one(db.writer())
    .await
    .unwrap();
    id
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

fn viewer(libraries: Vec<i64>, max_age: Option<i64>, user: Option<i64>) -> ItemFilter {
    ItemFilter {
        library_ids: libraries,
        max_age,
        user_id: user,
        ..Default::default()
    }
}

fn trilogy(parts: &[&str]) -> ProviderCollection {
    ProviderCollection {
        provider: "tmdb".into(),
        provider_id: "2344".into(),
        name: "Matrix Filmreihe".into(),
        sort_name: "matrix filmreihe".into(),
        overview: Some("Neo und die Maschinen.".into()),
        poster_url: Some("https://image.tmdb.org/t/p/original/matrix.jpg".into()),
        backdrop_url: None,
        parts: parts.iter().map(|p| (*p).to_owned()).collect(),
    }
}

#[tokio::test]
async fn automatic_collections_resolve_visible_members_in_release_order() {
    let (_dir, db) = open().await;
    let movies = libraries::create(&db, "Filme", "movies", &[])
        .await
        .unwrap()
        .id;
    let private = libraries::create(&db, "Privat", "movies", &[])
        .await
        .unwrap()
        .id;
    // Added out of order; one part is FSK 16, one lives in another library.
    let reloaded = item(
        &db,
        movies,
        "movie",
        "Matrix Reloaded",
        Some("2003-05-15"),
        Some(12),
        "tmdb:movie:604",
    )
    .await;
    let matrix = item(
        &db,
        movies,
        "movie",
        "Matrix",
        Some("1999-03-31"),
        Some(16),
        "tmdb:movie:603",
    )
    .await;
    let revolutions = item(
        &db,
        private,
        "movie",
        "Matrix Revolutions",
        Some("2003-11-05"),
        Some(12),
        "tmdb:movie:605",
    )
    .await;
    // A second copy of the first movie (other library) counts once.
    item(
        &db,
        private,
        "movie",
        "Matrix (4K)",
        Some("1999-03-31"),
        Some(16),
        "tmdb:movie:603",
    )
    .await;

    let parts = [
        "tmdb:movie:603",
        "tmdb:movie:604",
        "tmdb:movie:605",
        "tmdb:movie:624860",
    ];
    let id = collections::sync_provider(&db, &trilogy(&parts), "tmdb:movie:604")
        .await
        .unwrap();
    // Syncing again (from another member) keeps one collection.
    assert_eq!(
        collections::sync_provider(&db, &trilogy(&parts), "tmdb:movie:603")
            .await
            .unwrap(),
        id
    );
    assert_eq!(collections::list(&db).await.unwrap().len(), 1);
    assert_eq!(
        collections::member_identities(&db, id).await.unwrap().len(),
        4,
        "parts not in the library are kept for later"
    );

    let everything = viewer(vec![movies, private], None, None);
    assert_eq!(
        collections::member_ids(&db, id, &everything).await.unwrap(),
        [matrix, reloaded, revolutions],
        "release order, one item per identity"
    );
    let stats = collections::visible_stats(&db, &everything, 1, None)
        .await
        .unwrap();
    assert_eq!(stats.len(), 1);
    assert_eq!(stats[0].item_count, 3);

    // Library access and age limits narrow the members.
    let kid = viewer(vec![movies], Some(12), None);
    assert_eq!(
        collections::member_ids(&db, id, &kid).await.unwrap(),
        [reloaded]
    );
    let nothing = viewer(vec![movies], Some(6), None);
    assert!(
        collections::visible_stats(&db, &nothing, 1, None)
            .await
            .unwrap()
            .is_empty(),
        "a collection without visible members is absent"
    );

    // Blurhash survives a re-sync with the same artwork, and is dropped when it changes.
    collections::set_blurhash(&db, id, "poster", "LEHV6nWB2yk8")
        .await
        .unwrap();
    collections::sync_provider(&db, &trilogy(&parts), "tmdb:movie:603")
        .await
        .unwrap();
    let c = collections::get(&db, id).await.unwrap().unwrap();
    assert_eq!(c.poster_blurhash.as_deref(), Some("LEHV6nWB2yk8"));
    let mut changed = trilogy(&parts);
    changed.poster_url = Some("https://image.tmdb.org/t/p/original/other.jpg".into());
    collections::sync_provider(&db, &changed, "tmdb:movie:603")
        .await
        .unwrap();
    assert_eq!(
        collections::get(&db, id)
            .await
            .unwrap()
            .unwrap()
            .poster_blurhash,
        None
    );

    // A movie moving to another TMDB collection leaves the old one.
    let other = ProviderCollection {
        provider_id: "999".into(),
        name: "Andere Reihe".into(),
        sort_name: "andere reihe".into(),
        parts: vec![],
        ..trilogy(&[])
    };
    collections::sync_provider(&db, &other, "tmdb:movie:605")
        .await
        .unwrap();
    assert!(
        !collections::member_identities(&db, id)
            .await
            .unwrap()
            .contains(&"tmdb:movie:605".to_owned())
    );
    collections::leave_provider_collections(&db, "tmdb", "tmdb:movie:605")
        .await
        .unwrap();
    assert!(
        collections::of_item(&db, revolutions)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(collections::of_item(&db, reloaded).await.unwrap(), [id]);
}

#[tokio::test]
async fn manual_collections_and_playlist_entries_follow_identity_changes() {
    let (_dir, db) = open().await;
    let movies = libraries::create(&db, "Filme", "movies", &[])
        .await
        .unwrap()
        .id;
    let felix = user(&db, "Felix").await;
    let local = item(
        &db,
        movies,
        "movie",
        "Hausfilm",
        Some("2020-01-01"),
        Some(0),
        "local:movie:hausfilm:2020",
    )
    .await;
    let manual = collections::create_manual(&db, "Lieblinge", "lieblinge", None)
        .await
        .unwrap();
    assert_eq!(manual.kind, "manual");
    let identities = playlists::item_identities(&db, &[local, 999_999])
        .await
        .unwrap();
    assert_eq!(
        identities,
        [(local, "local:movie:hausfilm:2020".to_owned())],
        "unknown ids are skipped"
    );
    let keys: Vec<String> = identities.into_iter().map(|(_, k)| k).collect();
    assert_eq!(
        collections::add_members(&db, manual.id, &keys)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        collections::add_members(&db, manual.id, &keys)
            .await
            .unwrap(),
        0,
        "already a member"
    );
    let list = playlists::create(&db, felix, "Abend", false, None)
        .await
        .unwrap();
    playlists::add_entries(&db, list.id, &keys).await.unwrap();

    // The movie gets matched: its identity becomes provider based.
    metadata::set_provider_id(&db, local, "tmdb", "4242")
        .await
        .unwrap();
    userdata::update_identity(&db, local).await.unwrap();
    assert_eq!(
        collections::member_identities(&db, manual.id)
            .await
            .unwrap(),
        ["tmdb:movie:4242"]
    );
    let everything = viewer(vec![movies], None, Some(felix));
    assert_eq!(
        playlists::resolve(&db, list.id, &everything)
            .await
            .unwrap()
            .len(),
        1,
        "the entry still resolves after the identity change"
    );

    assert!(
        collections::remove_member(&db, manual.id, "tmdb:movie:4242")
            .await
            .unwrap()
    );
    assert!(collections::delete(&db, manual.id).await.unwrap());
    assert!(collections::get(&db, manual.id).await.unwrap().is_none());
}

#[tokio::test]
async fn playlist_entries_are_ordered_deduplicated_and_survive_removal_of_items() {
    let (_dir, db) = open().await;
    let movies = libraries::create(&db, "Filme", "movies", &[])
        .await
        .unwrap()
        .id;
    let felix = user(&db, "Felix").await;
    let anna = user(&db, "Anna").await;
    let a = item(&db, movies, "movie", "A", None, Some(0), "x:a").await;
    let b = item(&db, movies, "movie", "B", None, Some(0), "x:b").await;
    let c = item(&db, movies, "movie", "C", None, Some(18), "x:c").await;

    let list = playlists::create(&db, felix, "Mix", false, None)
        .await
        .unwrap();
    let keys = |k: &[&str]| k.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    assert_eq!(
        playlists::add_entries(&db, list.id, &keys(&["x:a", "x:b", "x:a", "x:c"]))
            .await
            .unwrap(),
        3
    );
    let all = viewer(vec![movies], None, Some(felix));
    let items_of = |rows: Vec<(i64, i64)>| rows.into_iter().map(|(_, i)| i).collect::<Vec<_>>();
    assert_eq!(
        items_of(playlists::resolve(&db, list.id, &all).await.unwrap()),
        [a, b, c]
    );
    assert_eq!(
        items_of(
            playlists::resolve(&db, list.id, &viewer(vec![movies], Some(12), Some(felix)))
                .await
                .unwrap()
        ),
        [a, b],
        "age limit applies to entries"
    );

    let entries = playlists::entry_ids(&db, list.id).await.unwrap();
    playlists::set_order(&db, list.id, &[entries[2], entries[0], entries[1]])
        .await
        .unwrap();
    assert_eq!(
        items_of(playlists::resolve(&db, list.id, &all).await.unwrap()),
        [c, a, b]
    );

    // An item that disappears (e.g. file deleted) is skipped, its entry kept.
    sqlx::query("DELETE FROM items WHERE id = ?")
        .bind(a)
        .execute(db.writer())
        .await
        .unwrap();
    assert_eq!(
        items_of(playlists::resolve(&db, list.id, &all).await.unwrap()),
        [c, b]
    );
    assert_eq!(playlists::entry_ids(&db, list.id).await.unwrap().len(), 3);
    // ... and comes back when the movie is scanned again.
    let a_again = item(&db, movies, "movie", "A", None, Some(0), "x:a").await;
    assert_eq!(
        items_of(playlists::resolve(&db, list.id, &all).await.unwrap()),
        [c, a_again, b]
    );

    assert!(
        playlists::remove_entry(&db, list.id, entries[1])
            .await
            .unwrap()
    );
    assert!(!playlists::remove_entry(&db, list.id, 0).await.unwrap());

    // Sharing: Anna sees shared playlists of others, never private ones.
    playlists::create(&db, anna, "Annas", false, None)
        .await
        .unwrap();
    assert_eq!(playlists::list_for_user(&db, felix).await.unwrap().len(), 1);
    playlists::update(&db, list.id, Some("Mix 2"), Some(true), None)
        .await
        .unwrap();
    let annas: Vec<String> = playlists::list_for_user(&db, anna)
        .await
        .unwrap()
        .into_iter()
        .map(|p| p.name)
        .collect();
    assert_eq!(annas, ["Annas", "Mix 2"], "own first, then shared");
    let shared = playlists::get(&db, list.id).await.unwrap().unwrap();
    assert_eq!(shared.owner_name, "Felix");
    assert_eq!(shared.entry_count, 2);
    assert!(playlists::delete(&db, list.id).await.unwrap());
}

#[tokio::test]
async fn smart_filter_fields_genres_and_rating() {
    let (_dir, db) = open().await;
    let movies = libraries::create(&db, "Filme", "movies", &[])
        .await
        .unwrap()
        .id;
    let horror = item(&db, movies, "movie", "Grusel", None, Some(0), "x:g").await;
    let comedy = item(&db, movies, "movie", "Lachen", None, Some(0), "x:l").await;
    let drama = item(&db, movies, "movie", "Tränen", None, Some(0), "x:t").await;
    metadata::set_genres(&db, horror, &["Horror".into()])
        .await
        .unwrap();
    metadata::set_genres(&db, comedy, &["Komödie".into(), "Horror".into()])
        .await
        .unwrap();
    metadata::set_genres(&db, drama, &["Drama".into()])
        .await
        .unwrap();
    for (id, rating) in [(horror, 5.5), (comedy, 8.0), (drama, 7.0)] {
        sqlx::query("UPDATE items SET community_rating = ? WHERE id = ?")
            .bind(rating)
            .bind(id)
            .execute(db.writer())
            .await
            .unwrap();
    }
    let filter = ItemFilter {
        library_ids: vec![movies],
        genres: vec!["Horror".into(), "Drama".into()],
        sort: Sort::Rating,
        descending: true,
        limit: 10,
        ..Default::default()
    };
    let ids = |rows: Vec<browse::ItemSummary>| rows.into_iter().map(|r| r.id).collect::<Vec<_>>();
    let (rows, total) = browse::list(&db, &filter).await.unwrap();
    assert_eq!(total, 3, "any of the genres, each item once");
    assert_eq!(ids(rows), [comedy, drama, horror]);
    let rated = ItemFilter {
        min_rating: Some(7.0),
        ..filter
    };
    assert_eq!(
        ids(browse::list(&db, &rated).await.unwrap().0),
        [comedy, drama]
    );
}
