//! Offline evaluation of the recommendations: synthetic profiles with known tastes
//! watch 70 % of "their" titles in the fixture catalogue; the other 30 % are held out.
//! Recall@10 of the held-out titles is compared with a popularity baseline (best rated
//! unseen titles) and the expected value of random picks. Runs with the stub
//! embedder, so it is deterministic and needs no model.

mod common;

use flixnet_ai::jobs::embed_all;
use flixnet_ai::recommend::{self, Viewer};
use flixnet_db::browse::Restriction;

use flixnet_ai::Ai;

use common::{Catalogue, Entry, e5_ai, load_catalogue, stub_ai, user, watched};

struct Persona {
    name: &'static str,
    likes: fn(&Entry) -> bool,
}

fn has(e: &Entry, genres: &[&str]) -> bool {
    e.g.iter().any(|g| genres.contains(&g.as_str()))
}

const PERSONAS: &[Persona] = &[
    Persona {
        name: "Science-Fiction",
        likes: |e| has(e, &["Science Fiction", "Sci-Fi & Fantasy"]),
    },
    Persona {
        name: "Horror",
        likes: |e| has(e, &["Horror"]),
    },
    Persona {
        name: "Familie",
        likes: |e| has(e, &["Animation", "Familie", "Kids"]),
    },
    Persona {
        name: "Krimi",
        likes: |e| has(e, &["Krimi"]),
    },
    Persona {
        name: "Romantik",
        likes: |e| has(e, &["Liebesfilm"]),
    },
    Persona {
        name: "Geschichte",
        likes: |e| has(e, &["Kriegsfilm", "Historie"]),
    },
    // Two separate interests: the taste clusters must serve both.
    Persona {
        name: "Western+Musik",
        likes: |e| has(e, &["Western", "Musik"]),
    },
];

const DAY: i64 = 24 * 3600 * 1000;

/// Deterministic shuffle key.
fn key(title: &str) -> u64 {
    title.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ b as u64).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Mean Recall@10 of (our recommendations, popularity baseline, random baseline).
async fn evaluate(cat: &Catalogue, ai: &Ai) -> (f64, f64, f64) {
    let embedder = ai.embedder().await.unwrap().unwrap();
    embed_all(ai, &embedder, false, &|| false, |_| async {})
        .await
        .unwrap();
    let now = flixnet_db::now_ms();

    let mut ours = Vec::new();
    let mut popular = Vec::new();
    let mut random = Vec::new();
    for persona in PERSONAS {
        let mut liked: Vec<&Entry> = cat.entries.iter().filter(|e| (persona.likes)(e)).collect();
        liked.sort_by_key(|e| key(&e.t));
        let split = (liked.len() * 7).div_ceil(10);
        let (history, held_out) = liked.split_at(split);
        assert!(held_out.len() >= 2, "{}: too few titles", persona.name);

        let user_id = user(&cat.db, persona.name).await;
        for (n, e) in history.iter().enumerate() {
            watched(&cat.db, user_id, cat.ids[&e.t], now - (n as i64 * 7) * DAY).await;
        }
        let viewer = Viewer {
            user_id,
            name: persona.name.to_owned(),
            library_ids: vec![cat.library_id],
            restriction: Restriction::default(),
        };
        let list = recommend::for_viewer(ai, &viewer, 10).await.unwrap();
        assert!(list.available && !list.needs_onboarding);
        assert_eq!(list.items.len(), 10);
        let held: Vec<i64> = held_out.iter().map(|e| cat.ids[&e.t]).collect();
        let seen: Vec<i64> = history.iter().map(|e| cat.ids[&e.t]).collect();
        assert!(
            list.items.iter().all(|r| !seen.contains(&r.item_id)),
            "watched titles are not recommended"
        );
        assert!(
            list.items.iter().all(|r| r.reason.starts_with("Weil du „")),
            "{:?}",
            list.items
        );
        let recall = |top: &[i64]| {
            held.iter().filter(|id| top.contains(id)).count() as f64 / held.len().min(10) as f64
        };

        let top: Vec<i64> = list.items.iter().map(|r| r.item_id).collect();
        let mut unseen: Vec<&Entry> = cat
            .entries
            .iter()
            .filter(|e| !seen.contains(&cat.ids[&e.t]))
            .collect();
        unseen.sort_by(|a, b| b.r.total_cmp(&a.r));
        let by_rating: Vec<i64> = unseen.iter().take(10).map(|e| cat.ids[&e.t]).collect();
        let expected_random =
            (10.0 / unseen.len() as f64) * held.len() as f64 / held.len().min(10) as f64;

        println!(
            "{:<16} history {:>2}, held out {:>2}: Recall@10 ours {:.2}, popularity {:.2}, random {:.2}",
            persona.name,
            history.len(),
            held.len(),
            recall(&top),
            recall(&by_rating),
            expected_random
        );
        ours.push(recall(&top));
        popular.push(recall(&by_rating));
        random.push(expected_random);
    }
    let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len() as f64;
    let (ours, popular, random) = (mean(&ours), mean(&popular), mean(&random));
    println!("mean Recall@10: ours {ours:.3}, popularity {popular:.3}, random {random:.3}");
    (ours, popular, random)
}

#[tokio::test]
async fn recommendations_find_held_out_titles_far_better_than_baselines() {
    let cat = load_catalogue().await;
    let (ours, popular, random) = evaluate(&cat, &stub_ai(&cat)).await;
    assert!(ours >= 0.4, "Recall@10 {ours}");
    assert!(ours >= 2.0 * popular, "ours {ours} vs popularity {popular}");
    assert!(ours >= 4.0 * random, "ours {ours} vs random {random}");
}

#[tokio::test]
#[ignore = "downloads multilingual-e5-base (~300 MB); run with --ignored"]
async fn e5_recommendations_beat_the_baselines() {
    let cat = load_catalogue().await;
    let (ours, popular, random) = evaluate(&cat, &e5_ai(&cat).await).await;
    assert!(
        ours >= 2.0 * popular.max(random),
        "ours {ours} vs popularity {popular} / random {random}"
    );
}

#[tokio::test]
async fn household_co_watching_and_onboarding_shape_the_list() {
    let cat = load_catalogue().await;
    let ai = stub_ai(&cat);
    let embedder = ai.embedder().await.unwrap().unwrap();
    embed_all(&ai, &embedder, false, &|| false, |_| async {})
        .await
        .unwrap();
    let now = flixnet_db::now_ms();
    let viewer = |user_id| Viewer {
        user_id,
        name: "Test".into(),
        library_ids: vec![cat.library_id],
        restriction: Restriction::default(),
    };

    // Without history: onboarding.
    let anna = user(&cat.db, "Anna").await;
    let empty = recommend::for_viewer(&ai, &viewer(anna), 10).await.unwrap();
    assert!(empty.needs_onboarding && empty.items.is_empty());
    let picks = recommend::onboarding_candidates(&ai, &viewer(anna), 12)
        .await
        .unwrap();
    assert_eq!(picks.len(), 12);

    // Onboarding picks act like liked titles.
    let jurassic = cat.ids["Jurassic Park"];
    flixnet_db::ai::set_taste_seeds(&cat.db, anna, &[jurassic])
        .await
        .unwrap();
    let seeded = recommend::for_viewer(&ai, &viewer(anna), 5).await.unwrap();
    assert!(!seeded.needs_onboarding);
    assert_eq!(seeded.items[0].reason, "Weil du „Jurassic Park“ magst");
    assert!(seeded.items.iter().all(|r| r.item_id != jurassic));
    let titles: Vec<&str> = seeded
        .items
        .iter()
        .map(|r| {
            cat.ids
                .iter()
                .find(|(_, id)| **id == r.item_id)
                .unwrap()
                .0
                .as_str()
        })
        .collect();
    assert!(
        titles
            .iter()
            .any(|t| t.contains("Jurassic World") || *t == "A Sound of Thunder"),
        "{titles:?}"
    );

    // Co-watch: Ben shares two titles with Cem, who also loved a third one.
    let ben = user(&cat.db, "Ben").await;
    let cem = user(&cat.db, "Cem").await;
    let (a, b, c) = (
        cat.ids["Rocky"],
        cat.ids["Whiplash"],
        cat.ids["Systemsprenger"],
    );
    for id in [a, b] {
        watched(&cat.db, ben, id, now).await;
        watched(&cat.db, cem, id, now).await;
    }
    watched(&cat.db, cem, c, now).await;
    let scores = recommend::cowatch_scores(
        ben,
        &flixnet_db::ai::household_watched(&cat.db).await.unwrap(),
    );
    assert!(scores[&c] > 0.9);
    let with_household = recommend::for_viewer(&ai, &viewer(ben), 30).await.unwrap();
    let rank_with = with_household.items.iter().position(|r| r.item_id == c);
    assert!(
        rank_with.is_some_and(|r| r < 15),
        "co-watched title ranks high: {rank_with:?}"
    );

    // Because-rows and "Mehr wie das".
    let rows = recommend::because_rows(&ai, &viewer(ben), 2, 8)
        .await
        .unwrap();
    assert!(!rows.is_empty());
    assert!(
        rows.iter()
            .all(|r| r.items.len() >= 3 && !r.items.contains(&a) && !r.items.contains(&b))
    );
    let similar = recommend::similar(&ai, &viewer(ben), jurassic, 5)
        .await
        .unwrap();
    assert_eq!(similar.len(), 5);
    assert!(!similar.contains(&jurassic));
}

#[tokio::test]
async fn group_mode_respects_the_strictest_member_and_skips_what_anyone_saw() {
    let cat = load_catalogue().await;
    let ai = stub_ai(&cat);
    let embedder = ai.embedder().await.unwrap().unwrap();
    embed_all(&ai, &embedder, false, &|| false, |_| async {})
        .await
        .unwrap();
    let now = flixnet_db::now_ms();

    let parent = user(&cat.db, "Mama").await;
    let kid = user(&cat.db, "Kind").await;
    for t in [
        "Der Exorzist",
        "Shining",
        "Alien – Das unheimliche Wesen aus einer fremden Welt",
        "Toy Story",
    ] {
        watched(&cat.db, parent, cat.ids[t], now).await;
    }
    for t in [
        "Findet Nemo",
        "Die Eiskönigin – Völlig unverfroren",
        "Bluey",
    ] {
        watched(&cat.db, kid, cat.ids[t], now).await;
    }
    let kid_limit = Restriction {
        max_age: Some(6),
        ..Default::default()
    };
    let members = [
        Viewer {
            user_id: parent,
            name: "Mama".into(),
            library_ids: vec![cat.library_id],
            restriction: Restriction::default(),
        },
        Viewer {
            user_id: kid,
            name: "Kind".into(),
            library_ids: vec![cat.library_id],
            restriction: kid_limit.clone(),
        },
    ];
    let combined = recommend::strictest(&[
        members[0].restriction.clone(),
        members[1].restriction.clone(),
    ]);
    let list = recommend::for_group(&ai, &members, &[cat.library_id], &combined, true, 12)
        .await
        .unwrap();
    assert_eq!(list.items.len(), 12);
    let ages: std::collections::HashMap<i64, i64> =
        cat.entries.iter().map(|e| (cat.ids[&e.t], e.a)).collect();
    for item in &list.items {
        assert!(
            ages[&item.item_id] <= 6,
            "{} is above the child's limit",
            item.item_id
        );
        assert!(
            ![
                cat.ids["Toy Story"],
                cat.ids["Findet Nemo"],
                cat.ids["Bluey"]
            ]
            .contains(&item.item_id),
            "already seen by a member"
        );
        assert!(!item.reason.is_empty());
    }
    // Without the exclusion, seen titles may come back.
    let again = recommend::for_group(&ai, &members, &[cat.library_id], &combined, false, 40)
        .await
        .unwrap();
    assert!(again.items.len() > list.items.len());
}
