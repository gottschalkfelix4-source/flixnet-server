//! Hybrid search: the FTS5/bm25 ranking and a vector ranking, fused with Reciprocal
//! Rank Fusion. Without vectors (AI off, model not downloaded yet) the caller falls
//! back to full-text search alone.

use std::collections::HashSet;

use flixnet_db::browse::{self, ItemFilter, Sort};

use crate::Ai;
use crate::index::VectorIndex;
use crate::math::{RRF_K, rrf};

/// How many full-text hits take part in the fusion.
const FTS_DEPTH: i64 = 200;
/// How many vector hits at most (before the relevance margin).
const VECTOR_DEPTH: usize = 40;

/// One page of hybrid results: item ids in fused order plus the total.
#[derive(Debug, Clone, PartialEq)]
pub struct HybridPage {
    pub ids: Vec<i64>,
    pub total: i64,
}

/// Runs a hybrid search for `raw` (the user's text) within everything `filter`
/// allows; `filter.search` holds the FTS query built from the same text. `None` when
/// semantic search is unavailable — use plain full-text search then.
pub async fn hybrid_page(
    ai: &Ai,
    filter: &ItemFilter,
    raw: &str,
) -> anyhow::Result<Option<HybridPage>> {
    if filter.search.is_none() || raw.trim().is_empty() {
        return Ok(None);
    }
    // Only movies and series have vectors.
    let kinds: Vec<String> = if filter.kinds.is_empty() {
        vec!["movie".into(), "series".into()]
    } else {
        filter
            .kinds
            .iter()
            .filter(|k| matches!(k.as_str(), "movie" | "series"))
            .cloned()
            .collect()
    };
    if kinds.is_empty() {
        return Ok(None);
    }
    let Some((query, margin)) = ai.query_vector(raw).await else {
        return Ok(None);
    };
    let db = ai.db();

    let mut fts = filter.clone();
    fts.sort = Sort::Relevance;
    fts.descending = false;
    fts.offset = 0;
    fts.limit = FTS_DEPTH;
    let (fts_ids, fts_total) = browse::page_ids(db, &fts).await?;

    let mut visible = filter.clone();
    visible.kinds = kinds;
    let allowed: HashSet<i64> = browse::visible_ids(db, &visible)
        .await?
        .into_iter()
        .collect();
    let hits = {
        let loaded = ai.index().await;
        loaded
            .as_ref()
            .map(|l| l.index.search(&query, VECTOR_DEPTH, Some(&allowed)))
            .unwrap_or_default()
    };
    let vector_ids = relevant_hits(&hits, margin);

    let fused = rrf(&[&fts_ids, &vector_ids], RRF_K);
    let total = fused.len() as i64 + (fts_total - fts_ids.len() as i64).max(0);
    let ids = fused
        .into_iter()
        .skip(filter.offset.max(0) as usize)
        .take(filter.limit.max(0) as usize)
        .map(|(id, _)| id)
        .collect();
    Ok(Some(HybridPage { ids, total }))
}

/// Vector hits close enough to the best one: a query about dinosaurs should not list
/// the whole library just because every item has *some* similarity.
pub fn relevant_hits(hits: &[(i64, f32)], margin: f32) -> Vec<i64> {
    let Some(best) = hits.first().map(|h| h.1) else {
        return Vec::new();
    };
    hits.iter()
        .take_while(|(_, score)| *score >= best - margin)
        .map(|(id, _)| *id)
        .collect()
}

/// Vector-only ranking of every movie/series `filter` allows (evaluation, "Mehr wie das").
pub async fn semantic_ids(
    ai: &Ai,
    filter: &ItemFilter,
    raw: &str,
    k: usize,
) -> anyhow::Result<Vec<(i64, f32)>> {
    let Some((query, _)) = ai.query_vector(raw).await else {
        return Ok(Vec::new());
    };
    let mut visible = filter.clone();
    if visible.kinds.is_empty() {
        visible.kinds = vec!["movie".into(), "series".into()];
    }
    let allowed: HashSet<i64> = browse::visible_ids(ai.db(), &visible)
        .await?
        .into_iter()
        .collect();
    let loaded = ai.index().await;
    Ok(loaded
        .as_ref()
        .map(|l| l.index.search(&query, k, Some(&allowed)))
        .unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_hits_near_the_best_one_count() {
        let hits = [(1, 0.9), (2, 0.88), (3, 0.86), (4, 0.7)];
        assert_eq!(relevant_hits(&hits, 0.035), [1, 2]);
        assert_eq!(relevant_hits(&hits, 0.25), [1, 2, 3, 4]);
        assert!(relevant_hits(&[], 0.1).is_empty());
    }
}
