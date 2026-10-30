//! Feeds the assistant's semantic hook (`flixnet_llm::semantic`) from the local
//! embeddings (`flixnet_ai`): natural-language search then also ranks movies and
//! series by meaning. Callers re-check every hit against the viewer's restrictions.
//!
//! Transcript chunks are not embedded (yet), so chunk search stays full-text only.

use flixnet_ai::Ai;
use flixnet_ai::index::VectorIndex;
use flixnet_llm::semantic::{BoxFuture, SemanticIndex};

pub struct AiSemantic(pub Ai);

impl SemanticIndex for AiSemantic {
    fn search_items<'a>(
        &'a self,
        query: &'a str,
        limit: usize,
    ) -> BoxFuture<'a, anyhow::Result<Vec<(i64, f32)>>> {
        Box::pin(async move {
            // No vectors or no runnable embedder: nothing semantic to add.
            let Some((vector, relevance)) = self.0.query_vector(query).await else {
                return Ok(Vec::new());
            };
            let loaded = self.0.index().await;
            let hits = loaded
                .as_ref()
                .map(|l| l.index.search(&vector, limit, None))
                .unwrap_or_default();
            // Like the hybrid /items search: only hits close to the best one and above
            // the model's floor, not everything with *some* similarity.
            let best = hits.first().map_or(0.0, |h| h.1);
            Ok(hits
                .into_iter()
                .take_while(|(_, score)| {
                    *score >= best - relevance.margin && *score >= relevance.floor
                })
                .collect())
        })
    }

    fn search_chunks<'a>(
        &'a self,
        _query: &'a str,
        _limit: usize,
    ) -> BoxFuture<'a, anyhow::Result<Vec<(i64, f32)>>> {
        Box::pin(async { Ok(Vec::new()) })
    }
}
