//! Background jobs: embedding items (ai pool, low priority, pauses for transcodes)
//! and downloading the local model (io pool, with progress).

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use flixnet_core::events::{EventBus, ServerEvent};
use flixnet_core::jobs::{JobContext, JobHandler, JobOptions, JobQueue, Pool, priority};
use flixnet_core::scheduler::TaskDef;
use flixnet_db::DbError;
use flixnet_db::ai::{self as store, NewEmbedding};
use serde::{Deserialize, Serialize};

use crate::Ai;
use crate::embed::{BATCH, Purpose};
use crate::math::to_bytes;
use crate::settings::EmbedderKind;
use crate::text::{embedding_text, text_hash};

/// Items read per page of the sweep.
const PAGE: i64 = 200;

pub struct EmbedItems {
    pub ai: Ai,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EmbedPayload {
    /// Recompute every vector, not only new and changed items.
    #[serde(default)]
    pub rebuild: bool,
}

/// What one sweep did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EmbedReport {
    pub embedded: usize,
    pub unchanged: usize,
}

impl JobHandler for EmbedItems {
    const KIND: &'static str = "ai.embed_items";
    const POOL: Pool = Pool::Ai;
    type Payload = EmbedPayload;

    async fn run(&self, ctx: JobContext, payload: EmbedPayload) -> anyhow::Result<()> {
        let cancelled = || ctx.is_cancelled();
        let Some(embedder) = self.ai.embedder().await? else {
            // Local model missing: fetch it first (the download queues us again).
            let settings = self.ai.settings().await?;
            if !self.ai.is_stub()
                && settings.embedder == EmbedderKind::Local
                && settings.auto_download
                && !self.ai.models().missing().is_empty()
                && self.ai.models().platform_supported()
            {
                queue_download(ctx.queue(), priority::BACKFILL).await?;
            }
            return Ok(());
        };
        let report = embed_all(&self.ai, &embedder, payload.rebuild, &cancelled, |p| {
            let ctx = ctx.clone();
            async move { ctx.progress(p).await }
        })
        .await?;
        if report.embedded > 0 {
            tracing::info!(embedded = report.embedded, unchanged = report.unchanged, model = %embedder.model_key(), "items embedded");
        }
        Ok(())
    }
}

/// Embeds every movie and series whose text changed since its vector was computed
/// (or all of them with `rebuild`). Waits while transcodes run; writes in short
/// batches after computing, never across the model call.
pub async fn embed_all<F, Fut>(
    ai: &Ai,
    embedder: &crate::embed::Embedder,
    rebuild: bool,
    cancelled: &(dyn Fn() -> bool + Sync),
    progress: F,
) -> anyhow::Result<EmbedReport>
where
    F: Fn(f64) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let db = ai.db();
    let key = embedder.model_key();
    let mut model_id = None;
    if let Some((id, _)) = store::find_model(db, &key).await? {
        model_id = Some(id);
        if rebuild {
            store::delete_model_vectors(db, id).await?;
            ai.drop_index().await;
        }
    }
    let (_, total) = store::embedding_counts(db, None).await?;
    let mut report = EmbedReport::default();
    let mut after = 0;
    let mut seen = 0_usize;
    loop {
        let sources = store::embedding_sources(db, after, PAGE).await?;
        let Some(last) = sources.last() else { break };
        after = last.id;
        seen += sources.len();

        let texts: Vec<(i64, String, String)> = sources
            .iter()
            .map(|s| {
                let text = embedding_text(s);
                (s.id, text_hash(&key, &text), text)
            })
            .collect();
        let stored: std::collections::HashMap<i64, String> = match model_id {
            Some(id) => {
                store::text_hashes(db, id, &sources.iter().map(|s| s.id).collect::<Vec<_>>())
                    .await?
                    .into_iter()
                    .collect()
            }
            None => Default::default(),
        };
        let todo: Vec<&(i64, String, String)> = texts
            .iter()
            .filter(|(id, hash, _)| stored.get(id) != Some(hash))
            .collect();
        report.unchanged += texts.len() - todo.len();

        for chunk in todo.chunks(BATCH) {
            if !ai.wait_until_idle(cancelled).await {
                anyhow::bail!("cancelled");
            }
            let input: Vec<String> = chunk.iter().map(|(_, _, t)| t.clone()).collect();
            let vectors = embedder.embed(&input, Purpose::Passage).await?;
            let dims = vectors.first().map_or(0, Vec::len) as i64;
            let id = match model_id {
                Some(id) => id,
                None => {
                    let id = store::model_id(db, &key, dims).await?;
                    // Switching models: vectors of the old one are useless now.
                    store::delete_other_models(db, id).await?;
                    model_id = Some(id);
                    id
                }
            };
            let rows: Vec<NewEmbedding> = chunk
                .iter()
                .zip(&vectors)
                .map(|((item_id, hash, _), v)| NewEmbedding {
                    item_id: *item_id,
                    text_hash: hash.clone(),
                    vector: to_bytes(v),
                })
                .collect();
            store::upsert_embeddings(db, id, &rows).await?;
            let fresh: Vec<(i64, Vec<f32>)> =
                chunk.iter().map(|(i, _, _)| *i).zip(vectors).collect();
            ai.index_insert(&key, id, &fresh).await;
            report.embedded += chunk.len();
            progress((seen as f64 / total.max(1) as f64).min(1.0)).await;
        }
        if cancelled() {
            anyhow::bail!("cancelled");
        }
    }
    Ok(report)
}

/// Queues an embedding sweep (deduplicated; `delay` collects bursts of changes).
pub async fn queue_embedding(
    queue: &JobQueue,
    prio: i64,
    delay: Option<Duration>,
    rebuild: bool,
) -> Result<Option<i64>, DbError> {
    queue
        .enqueue::<EmbedItems>(
            &EmbedPayload { rebuild },
            JobOptions {
                priority: prio,
                dedupe_key: Some(
                    if rebuild {
                        "ai:embed:rebuild"
                    } else {
                        "ai:embed"
                    }
                    .to_owned(),
                ),
                delay,
            },
        )
        .await
}

// ---- Model download ---------------------------------------------------------------

pub struct DownloadModel {
    pub ai: Ai,
}

impl JobHandler for DownloadModel {
    const KIND: &'static str = "ai.download_model";
    const POOL: Pool = Pool::Io;
    type Payload = ();

    async fn run(&self, ctx: JobContext, _: ()) -> anyhow::Result<()> {
        let missing = self.ai.models().missing();
        if missing.is_empty() {
            return Ok(());
        }
        let total: u64 = missing.iter().map(|a| a.size.max(20_000_000)).sum();
        let mut before = 0_u64;
        for artifact in &missing {
            tracing::info!(
                file = artifact.label,
                url = artifact.url,
                "downloading AI model file"
            );
            let weight = artifact.size.max(20_000_000);
            // Share of this file done, in millionths; reported once a second.
            let share = Arc::new(AtomicU64::new(0));
            let reporter = tokio::spawn({
                let (share, ctx) = (share.clone(), ctx.clone());
                async move {
                    loop {
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        let s = share.load(Ordering::Relaxed) as f64 / 1e6;
                        ctx.progress((before as f64 + s * weight as f64) / total as f64)
                            .await;
                    }
                }
            });
            let result = self
                .ai
                .models()
                .install(
                    self.ai.download_client(),
                    artifact,
                    |done, size| {
                        let s = if size > 0 {
                            done.min(size) as f64 / size as f64
                        } else {
                            0.5
                        };
                        share.store((s * 1e6) as u64, Ordering::Relaxed);
                    },
                    || ctx.is_cancelled(),
                )
                .await;
            reporter.abort();
            if let Err(err) = result {
                self.ai.set_error(Some(format!("{err:#}")));
                return Err(err);
            }
            before += weight;
            ctx.progress(before as f64 / total as f64).await;
        }
        self.ai.set_error(None);
        queue_embedding(ctx.queue(), priority::NEW, None, false).await?;
        Ok(())
    }
}

/// Queues the model download (deduplicated).
pub async fn queue_download(queue: &JobQueue, prio: i64) -> Result<Option<i64>, DbError> {
    queue
        .enqueue::<DownloadModel>(&(), JobOptions::priority(prio).dedupe("ai:download"))
        .await
}

/// Hourly catch-up: embeds whatever changed and was not picked up by an event.
pub fn tasks() -> Vec<TaskDef> {
    vec![TaskDef::new::<EmbedItems>(
        "ai_embeddings",
        "KI: Embeddings für neue und geänderte Titel berechnen",
        Duration::from_secs(60 * 60),
        &EmbedPayload::default(),
    )]
}

/// After metadata refreshes and scans, queue an embedding sweep (debounced).
pub fn spawn_event_listener(
    events: &EventBus,
    queue: JobQueue,
    debounce: Duration,
) -> tokio::task::JoinHandle<()> {
    let mut rx = events.subscribe();
    tokio::spawn(async move {
        loop {
            let event = match rx.recv().await {
                Ok(event) => event,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            };
            let relevant = match &event {
                ServerEvent::JobUpdated { kind, state, .. } => {
                    state == "done" && matches!(kind.as_str(), "metadata.refresh" | "library.scan")
                }
                ServerEvent::LibraryChanged { .. } => true,
                #[allow(unreachable_patterns)]
                _ => false,
            };
            if relevant
                && let Err(err) =
                    queue_embedding(&queue, priority::BACKFILL, Some(debounce), false).await
            {
                tracing::warn!(%err, "queueing embeddings failed");
            }
        }
    })
}
