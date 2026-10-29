//! Browsing queries: correctness on a small library and the milestone 2.6 gate
//! (p95 < 50 ms on 50 000 items).

use std::time::{Duration, Instant};

use flixnet_db::browse::{self, ItemFilter, Sort, fts_query};
use flixnet_db::{Db, libraries, now_ms};
use tempfile::TempDir;

async fn open() -> (TempDir, Db) {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("flixnet.db")).await.unwrap();
    (dir, db)
}

struct Seed<'a> {
    kind: &'a str,
    title: &'a str,
    sort: &'a str,
    year: Option<i64>,
    age: Option<i64>,
    parent: Option<i64>,
    series: Option<i64>,
    index: Option<i64>,
    overview: Option<&'a str>,
}

async fn insert(db: &Db, library: i64, s: Seed<'_>) -> i64 {
    let (id,): (i64,) = sqlx::query_as(
        "INSERT INTO items (library_id, kind, parent_id, series_id, title, sort_title, year, age_rating,
                            index_number, overview, content_key, date_added, date_modified)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(library)
    .bind(s.kind)
    .bind(s.parent)
    .bind(s.series)
    .bind(s.title)
    .bind(s.sort)
    .bind(s.year)
    .bind(s.age)
    .bind(s.index)
    .bind(s.overview)
    .bind(format!("test:{}:{}", s.kind, s.title))
    .bind(now_ms())
    .bind(now_ms())
    .fetch_one(db.writer())
    .await
    .unwrap();
    id
}

fn movie<'a>(title: &'a str, sort: &'a str, year: i64, age: Option<i64>) -> Seed<'a> {
    Seed {
        kind: "movie",
        title,
        sort,
        year: Some(year),
        age,
        parent: None,
        series: None,
        index: None,
        overview: None,
    }
}

#[tokio::test]
async fn filters_sorting_letters_search_and_age_limits() {
    let (_dir, db) = open().await;
    let lib = libraries::create(&db, "Filme", "movies", &[])
        .await
        .unwrap()
        .id;
    let other = libraries::create(&db, "Privat", "movies", &[])
        .await
        .unwrap()
        .id;

    insert(&db, lib, movie("The Matrix", "matrix", 1999, Some(16))).await;
    insert(
        &db,
        lib,
        movie("Ärger im Paradies", "arger im paradies", 2010, Some(6)),
    )
    .await;
    insert(&db, lib, movie("Zorro", "zorro", 1998, Some(12))).await;
    insert(&db, lib, movie("1917", "1917", 2019, Some(12))).await;
    let mut unrated = movie("Unbewertet", "unbewertet", 2000, None);
    unrated.overview = Some("Ein Film über Matrosen.");
    insert(&db, lib, unrated).await;
    insert(&db, other, movie("Geheim", "geheim", 2020, Some(0))).await;

    let base = ItemFilter {
        library_ids: vec![lib],
        kinds: vec!["movie".into()],
        limit: 50,
        ..Default::default()
    };
    let titles =
        |rows: &[browse::ItemSummary]| rows.iter().map(|r| r.title.clone()).collect::<Vec<_>>();

    let (rows, total) = browse::list(&db, &base).await.unwrap();
    assert_eq!(total, 5, "other libraries are not included");
    assert_eq!(
        titles(&rows),
        [
            "1917",
            "Ärger im Paradies",
            "The Matrix",
            "Unbewertet",
            "Zorro"
        ]
    );

    let by_year = ItemFilter {
        sort: Sort::Year,
        descending: true,
        ..base.clone()
    };
    assert_eq!(
        browse::list(&db, &by_year).await.unwrap().0[0].title,
        "1917"
    );

    let a = ItemFilter {
        letter: Some('a'),
        ..base.clone()
    };
    assert_eq!(
        titles(&browse::list(&db, &a).await.unwrap().0),
        ["Ärger im Paradies"],
        "umlauts sort under A"
    );
    let hash = ItemFilter {
        letter: Some('#'),
        ..base.clone()
    };
    assert_eq!(titles(&browse::list(&db, &hash).await.unwrap().0), ["1917"]);
    let letters = browse::letters(&db, &base).await.unwrap();
    assert_eq!(letters, [('#', 1), ('a', 1), ('m', 1), ('u', 1), ('z', 1)]);

    // Search: prefix, accent-insensitive, title before overview.
    let search = ItemFilter {
        search: fts_query("matr"),
        sort: Sort::Relevance,
        ..base.clone()
    };
    assert_eq!(
        titles(&browse::list(&db, &search).await.unwrap().0),
        ["The Matrix", "Unbewertet"]
    );
    let accent = ItemFilter {
        search: fts_query("arger"),
        ..base.clone()
    };
    assert_eq!(browse::list(&db, &accent).await.unwrap().1, 1);

    let range = ItemFilter {
        year_from: Some(1998),
        year_to: Some(2000),
        ..base.clone()
    };
    assert_eq!(browse::list(&db, &range).await.unwrap().1, 3);

    // Age 12 profile: no 16, no unrated.
    let kids = ItemFilter {
        max_age: Some(12),
        ..base.clone()
    };
    assert_eq!(
        titles(&browse::list(&db, &kids).await.unwrap().0),
        ["1917", "Ärger im Paradies", "Zorro"]
    );

    // Paging.
    let page = ItemFilter {
        limit: 2,
        offset: 2,
        ..base.clone()
    };
    let (rows, total) = browse::list(&db, &page).await.unwrap();
    assert_eq!(
        (titles(&rows), total),
        (vec!["The Matrix".to_owned(), "Unbewertet".to_owned()], 5)
    );
}

#[tokio::test]
async fn episodes_inherit_the_series_rating() {
    let (_dir, db) = open().await;
    let lib = libraries::create(&db, "Serien", "shows", &[])
        .await
        .unwrap()
        .id;
    let series = insert(
        &db,
        lib,
        Seed {
            kind: "series",
            title: "Dark",
            sort: "dark",
            year: Some(2017),
            age: Some(16),
            parent: None,
            series: None,
            index: None,
            overview: None,
        },
    )
    .await;
    let season = insert(
        &db,
        lib,
        Seed {
            kind: "season",
            title: "Staffel 1",
            sort: "staffel 1",
            year: None,
            age: None,
            parent: Some(series),
            series: Some(series),
            index: Some(1),
            overview: None,
        },
    )
    .await;
    for n in [2, 1] {
        insert(
            &db,
            lib,
            Seed {
                kind: "episode",
                title: if n == 1 { "Eins" } else { "Zwei" },
                sort: "x",
                year: None,
                age: None,
                parent: Some(season),
                series: Some(series),
                index: Some(n),
                overview: None,
            },
        )
        .await;
    }
    let episodes = ItemFilter {
        library_ids: vec![lib],
        parent_id: Some(season),
        limit: 50,
        ..Default::default()
    };
    let (rows, _) = browse::list(&db, &episodes).await.unwrap();
    assert_eq!(
        rows.iter().map(|r| r.index_number).collect::<Vec<_>>(),
        [Some(1), Some(2)]
    );

    let kid = ItemFilter {
        max_age: Some(12),
        ..episodes.clone()
    };
    assert_eq!(
        browse::list(&db, &kid).await.unwrap().1,
        0,
        "16+ series hides its episodes"
    );
    assert!(
        !browse::is_visible(&db, rows[0].id, &[lib], Some(12))
            .await
            .unwrap()
    );
    assert!(
        browse::is_visible(&db, rows[0].id, &[lib], Some(16))
            .await
            .unwrap()
    );

    let series_row = browse::list(
        &db,
        &ItemFilter {
            library_ids: vec![lib],
            kinds: vec!["series".into()],
            limit: 1,
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .0;
    assert_eq!(series_row[0].child_count, 1);
}

/// Inserts `n` movies with genres and posters in one transaction.
async fn seed_large(db: &Db, library: i64, n: usize) {
    let words = [
        "Nacht", "Stern", "Reise", "Haus", "Krieg", "Liebe", "Geist", "Stadt", "Meer", "Zeit",
        "Spiel", "Held",
    ];
    let mut tx = db.writer().begin().await.unwrap();
    for genre in ["Drama", "Komödie", "Action", "Thriller", "Dokumentarfilm"] {
        sqlx::query("INSERT INTO genres (name) VALUES (?)")
            .bind(genre)
            .execute(&mut *tx)
            .await
            .unwrap();
    }
    for i in 0..n {
        let title = format!("{} der {} {}", words[i % 12], words[(i / 12) % 12], i);
        let sort = title.to_lowercase();
        let (id,): (i64,) = sqlx::query_as(
            "INSERT INTO items (library_id, kind, title, sort_title, year, age_rating, community_rating,
                                overview, content_key, date_added, date_modified)
             VALUES (?, 'movie', ?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
        )
        .bind(library)
        .bind(&title)
        .bind(&sort)
        .bind(1950 + (i % 75) as i64)
        .bind([0, 6, 12, 16, 18][i % 5])
        .bind((i % 100) as f64 / 10.0)
        .bind(format!("Eine Geschichte über {} und {}.", words[(i * 7) % 12], words[(i * 5) % 12]))
        .bind(format!("perf:{i}"))
        .bind(i as i64)
        .bind(i as i64)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        sqlx::query("INSERT INTO item_genres (item_id, genre_id) VALUES (?, ?)")
            .bind(id)
            .bind((i % 5) as i64 + 1)
            .execute(&mut *tx)
            .await
            .unwrap();
        if i % 2 == 0 {
            sqlx::query("INSERT INTO images (item_id, kind, source, url, score, created_at) VALUES (?, 'poster', 'tmdb', 'u', 1, 0)")
                .bind(id)
                .execute(&mut *tx)
                .await
                .unwrap();
        }
    }
    tx.commit().await.unwrap();
    sqlx::query("ANALYZE").execute(db.writer()).await.unwrap();
    // A running server checkpoints continuously; a fresh 50k-row WAL is not representative.
    sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
        .execute(db.writer())
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "diagnostics"]
async fn explain_browse_plans() {
    let (_dir, db) = open().await;
    let lib = libraries::create(&db, "Groß", "movies", &[])
        .await
        .unwrap()
        .id;
    seed_large(&db, lib, 50_000).await;
    for (name, sql) in [
        (
            "page",
            "SELECT i.id FROM items i WHERE i.library_id IN (?) AND i.kind IN (?) ORDER BY i.sort_title ASC, i.parent_index_number, i.index_number, i.sort_title, i.id LIMIT 60 OFFSET 0",
        ),
        (
            "page-simple",
            "SELECT i.id FROM items i WHERE i.library_id IN (?) AND i.kind IN (?) ORDER BY i.sort_title ASC LIMIT 60 OFFSET 0",
        ),
        (
            "count",
            "SELECT COUNT(*) FROM items i WHERE i.library_id IN (?) AND i.kind IN (?)",
        ),
    ] {
        let plan: Vec<(i64, i64, i64, String)> =
            sqlx::query_as(sqlx::AssertSqlSafe(format!("EXPLAIN QUERY PLAN {sql}")))
                .bind(lib)
                .bind("movie")
                .fetch_all(db.reader())
                .await
                .unwrap();
        let start = Instant::now();
        let _: Vec<(i64,)> = sqlx::query_as(sqlx::AssertSqlSafe(sql.to_owned()))
            .bind(lib)
            .bind("movie")
            .fetch_all(db.reader())
            .await
            .unwrap();
        eprintln!("{name} ({:?}):", start.elapsed());
        for row in plan {
            eprintln!("    {}", row.3);
        }
    }
    let ids: Vec<i64> = (1..=60).collect();
    let start = Instant::now();
    let rows = browse::summaries(&db, &ids, None).await.unwrap();
    eprintln!("summaries x{} ({:?})", rows.len(), start.elapsed());
    let plan: Vec<(i64, i64, i64, String)> = sqlx::query_as(
        "EXPLAIN QUERY PLAN SELECT (SELECT im.id FROM images im WHERE im.item_id = i.id AND im.kind = 'poster'
           ORDER BY CASE im.source WHEN 'local' THEN 0 ELSE 2 END, im.score DESC, im.id LIMIT 1),
           (SELECT COUNT(*) FROM items c WHERE c.parent_id = i.id)
         FROM items i WHERE i.id IN (1, 2, 3)",
    )
    .fetch_all(db.reader())
    .await
    .unwrap();
    for row in plan {
        eprintln!("    {}", row.3);
    }
}

#[tokio::test]
async fn p95_under_50ms_on_50k_items() {
    let (_dir, db) = open().await;
    let lib = libraries::create(&db, "Groß", "movies", &[])
        .await
        .unwrap()
        .id;
    seed_large(&db, lib, 50_000).await;

    let base = ItemFilter {
        library_ids: vec![lib],
        kinds: vec!["movie".into()],
        limit: 60,
        ..Default::default()
    };
    let queries: Vec<(&str, ItemFilter)> = vec![
        ("title page 1", base.clone()),
        (
            "recently added",
            ItemFilter {
                sort: Sort::DateAdded,
                descending: true,
                ..base.clone()
            },
        ),
        (
            "deep page",
            ItemFilter {
                offset: 40_000,
                ..base.clone()
            },
        ),
        (
            "letter",
            ItemFilter {
                letter: Some('s'),
                ..base.clone()
            },
        ),
        (
            "genre",
            ItemFilter {
                genre: Some("Thriller".into()),
                sort: Sort::Rating,
                descending: true,
                ..base.clone()
            },
        ),
        (
            "search",
            ItemFilter {
                search: fts_query("stern rei"),
                sort: Sort::Relevance,
                ..base.clone()
            },
        ),
        (
            "years",
            ItemFilter {
                year_from: Some(1990),
                year_to: Some(1999),
                sort: Sort::Year,
                ..base.clone()
            },
        ),
        (
            "kids profile",
            ItemFilter {
                max_age: Some(12),
                ..base.clone()
            },
        ),
    ];

    let mut samples: Vec<(Duration, &str)> = Vec::new();
    for (name, filter) in &queries {
        // Warm-up, then measure.
        browse::list(&db, filter).await.unwrap();
        for _ in 0..15 {
            let start = Instant::now();
            let (rows, total) = browse::list(&db, filter).await.unwrap();
            samples.push((start.elapsed(), name));
            assert!(!rows.is_empty() && total > 0, "{name} returned nothing");
        }
    }
    let start = Instant::now();
    browse::letters(&db, &base).await.unwrap();
    samples.push((start.elapsed(), "letters"));

    for (name, _) in &queries {
        let worst = samples
            .iter()
            .filter(|(_, n)| n == name)
            .map(|(d, _)| *d)
            .max()
            .unwrap();
        eprintln!("  {name:<16} worst {worst:?}");
    }
    samples.sort();
    let p95 = samples[samples.len() * 95 / 100].0;
    let worst = samples.last().unwrap();
    eprintln!("browse p95 = {p95:?}, worst = {:?} ({})", worst.0, worst.1);
    assert!(
        p95 < Duration::from_millis(50),
        "p95 {p95:?} (worst {:?} in {})",
        worst.0,
        worst.1
    );
}
