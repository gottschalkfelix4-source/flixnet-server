//! Search quality on a golden set: 50 German and English queries over the fixture
//! catalogue, Recall@5 = mean over queries of |relevant ∩ top 5| / min(|relevant|, 5).
//!
//! * `stub_*` runs everywhere (CI): the whole pipeline with the deterministic stub.
//! * `e5_*` is the real milestone check with multilingual-e5-base. It downloads the
//!   model (~300 MB) on first run, so it is `#[ignore]`d:
//!   `cargo test -p flixnet-ai --test golden -- --ignored --nocapture`
//!   (cache: `FLIXNET_AI_MODELS`, default `target/ai-models`).

mod common;

use flixnet_ai::Ai;
use flixnet_ai::jobs::embed_all;
use flixnet_ai::search::{hybrid_page, semantic_ids};
use flixnet_db::browse::{self, ItemFilter};

use common::{Catalogue, GoldenQuery, e5_ai, golden, load_catalogue, stub_ai};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Vector,
    Hybrid,
}

/// Recall@5 plus the queries that missed something.
async fn recall_at_5(
    ai: &Ai,
    cat: &Catalogue,
    queries: &[GoldenQuery],
    mode: Mode,
) -> (f64, Vec<String>) {
    let mut sum = 0.0;
    let mut misses = Vec::new();
    for query in queries {
        let filter = ItemFilter {
            library_ids: vec![cat.library_id],
            search: browse::fts_query(&query.q),
            limit: 5,
            ..Default::default()
        };
        let top: Vec<i64> = match mode {
            Mode::Vector => semantic_ids(ai, &filter, &query.q, 5)
                .await
                .unwrap()
                .into_iter()
                .map(|(id, _)| id)
                .collect(),
            Mode::Hybrid => match hybrid_page(ai, &filter, &query.q).await.unwrap() {
                Some(page) => page.ids,
                None => browse::page_ids(&cat.db, &filter).await.unwrap().0,
            },
        };
        let relevant: Vec<i64> = query.rel.iter().map(|t| cat.ids[t]).collect();
        let hits = relevant.iter().filter(|id| top.contains(id)).count();
        let recall = hits as f64 / relevant.len().min(5) as f64;
        sum += recall;
        if recall < 1.0 {
            let titles: Vec<String> = top
                .iter()
                .map(|id| {
                    cat.ids
                        .iter()
                        .find(|(_, v)| *v == id)
                        .map(|(t, _)| t.clone())
                        .unwrap_or_default()
                })
                .collect();
            misses.push(format!(
                "[{}] {} → {:?} (wanted {:?})",
                query.lang, query.q, titles, query.rel
            ));
        }
    }
    (sum / queries.len() as f64, misses)
}

async fn report(ai: &Ai, cat: &Catalogue, label: &str) -> (f64, f64) {
    let queries = golden();
    assert_eq!(queries.len(), 50);
    let (vector, misses) = recall_at_5(ai, cat, &queries, Mode::Vector).await;
    let (hybrid, hybrid_misses) = recall_at_5(ai, cat, &queries, Mode::Hybrid).await;
    let de: Vec<GoldenQuery> = queries.iter().filter(|q| q.lang == "de").cloned().collect();
    let en: Vec<GoldenQuery> = queries.iter().filter(|q| q.lang == "en").cloned().collect();
    let (hybrid_de, _) = recall_at_5(ai, cat, &de, Mode::Hybrid).await;
    let (hybrid_en, _) = recall_at_5(ai, cat, &en, Mode::Hybrid).await;
    println!(
        "{label}: Recall@5 vector {vector:.3}, hybrid {hybrid:.3} (de {hybrid_de:.3}, en {hybrid_en:.3})"
    );
    for miss in &misses {
        println!("  vector miss: {miss}");
    }
    for miss in &hybrid_misses {
        println!("  hybrid miss: {miss}");
    }
    (vector, hybrid)
}

#[tokio::test]
async fn stub_pipeline_embeds_the_catalogue_and_answers_the_golden_set() {
    let cat = load_catalogue().await;
    let ai = stub_ai(&cat);
    let embedder = ai.embedder().await.unwrap().unwrap();
    let first = embed_all(&ai, &embedder, false, &|| false, |_| async {})
        .await
        .unwrap();
    assert_eq!(first.embedded, cat.entries.len());
    // A second sweep finds nothing changed.
    let second = embed_all(&ai, &embedder, false, &|| false, |_| async {})
        .await
        .unwrap();
    assert_eq!((second.embedded, second.unchanged), (0, cat.entries.len()));

    let (vector, hybrid) = report(&ai, &cat, "stub").await;
    // The stub only knows word stems and a handful of concepts; this guards the
    // plumbing, not quality (that is the e5 test below).
    assert!(vector >= 0.4, "stub vector recall {vector}");
    // Hybrid only keeps stub hits above its noise floor, so it may trail here.
    assert!(hybrid >= 0.3, "stub hybrid recall {hybrid}");
}

#[tokio::test]
#[ignore = "downloads multilingual-e5-base (~300 MB); run with --ignored"]
async fn e5_reaches_recall_at_5_of_0_8_on_the_golden_set() {
    let cat = load_catalogue().await;
    let ai = e5_ai(&cat).await;
    let embedder = ai.embedder().await.unwrap().expect("local model loads");
    let started = std::time::Instant::now();
    embed_all(&ai, &embedder, false, &|| false, |_| async {})
        .await
        .unwrap();
    println!(
        "embedded {} items in {:?}",
        cat.entries.len(),
        started.elapsed()
    );
    // How far below the best hit relevant titles score (tunes the search margin).
    let mut gaps = Vec::new();
    for query in golden() {
        let filter = ItemFilter {
            library_ids: vec![cat.library_id],
            ..Default::default()
        };
        let hits = semantic_ids(&ai, &filter, &query.q, 20).await.unwrap();
        let best = hits[0].1;
        for title in &query.rel {
            if let Some((_, s)) = hits.iter().find(|(id, _)| *id == cat.ids[title]) {
                gaps.push(best - s);
            }
        }
        println!(
            "  {:<60} best {best:.3}, 5th {:.3}, 10th {:.3}",
            query.q, hits[4].1, hits[9].1
        );
    }
    gaps.sort_by(f32::total_cmp);
    println!(
        "gap of relevant titles below the best hit: median {:.3}, p90 {:.3}, max {:.3}",
        gaps[gaps.len() / 2],
        gaps[gaps.len() * 9 / 10],
        gaps[gaps.len() - 1]
    );
    let (vector, hybrid) = report(&ai, &cat, "multilingual-e5-base int8").await;
    assert!(hybrid >= 0.8, "hybrid Recall@5 {hybrid}");
    assert!(vector >= 0.8, "vector Recall@5 {vector}");
    assert!(
        hybrid >= vector - 0.05,
        "hybrid {hybrid} should not lose to vectors alone {vector}"
    );
}
