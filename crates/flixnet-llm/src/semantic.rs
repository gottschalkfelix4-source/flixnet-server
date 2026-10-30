//! Hook for local embeddings (flixnet-ai's `VectorIndex`): semantic hits for items
//! and transcript chunks, merged with the keyword hits by reciprocal rank fusion.
//!
//! Nothing here knows about models; an implementation is handed to the API at
//! start-up. Without one, natural-language search and scene search use FTS5 only.
//! Ids returned by an index are candidates — the caller still applies the viewer's
//! library access and parental controls.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub trait SemanticIndex: Send + Sync {
    /// Movies/series/episodes similar to the text: (item id, score), best first.
    fn search_items<'a>(
        &'a self,
        query: &'a str,
        limit: usize,
    ) -> BoxFuture<'a, anyhow::Result<Vec<(i64, f32)>>>;

    /// Transcript chunks similar to the text: (chunk id, score), best first.
    fn search_chunks<'a>(
        &'a self,
        query: &'a str,
        limit: usize,
    ) -> BoxFuture<'a, anyhow::Result<Vec<(i64, f32)>>>;
}

/// Reciprocal rank fusion of ranked id lists (k = 60 as usual): ids high in several
/// lists come first; ties keep first-seen order.
pub fn rrf(lists: &[Vec<i64>], k: f64) -> Vec<i64> {
    let mut scores: HashMap<i64, (f64, usize)> = HashMap::new();
    let mut seen = 0;
    for list in lists {
        for (rank, id) in list.iter().enumerate() {
            let entry = scores.entry(*id).or_insert_with(|| {
                seen += 1;
                (0.0, seen)
            });
            entry.0 += 1.0 / (k + rank as f64 + 1.0);
        }
    }
    let mut ids: Vec<(i64, f64, usize)> =
        scores.into_iter().map(|(id, (s, o))| (id, s, o)).collect();
    ids.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.2.cmp(&b.2)));
    ids.into_iter().map(|(id, _, _)| id).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rrf_prefers_ids_in_both_lists() {
        assert_eq!(rrf(&[vec![1, 2, 3], vec![3, 4]], 60.0), vec![3, 1, 2, 4]);
        assert_eq!(rrf(&[vec![5, 6]], 60.0), vec![5, 6]);
        assert!(rrf(&[], 60.0).is_empty());
    }
}
