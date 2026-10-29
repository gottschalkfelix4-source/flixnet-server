//! Guide refresh: download (or open) the XMLTV file, parse it on a blocking thread
//! and write the programs in batches under a new generation; only a complete
//! refresh becomes visible, then older programs are deleted.

use std::path::{Path, PathBuf};

use flixnet_db::livetv::{self, NewEpgChannel, NewProgram};
use flixnet_db::{Db, now_ms};

use crate::http;
use crate::xmltv::{self, XmltvItem};

/// Programs that ended longer ago than this are not stored.
const KEEP_PAST_MS: i64 = 12 * 60 * 60 * 1000;
/// Programs per batch handed from the parser to the writer.
const PARSE_BATCH: usize = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RefreshReport {
    pub channels: usize,
    pub programs: usize,
}

enum Parsed {
    Programs(Vec<NewProgram>),
    Done(Result<Vec<NewEpgChannel>, String>),
}

/// Where a source's guide is read from: downloaded into `cache_dir`, or the local
/// file it names.
async fn fetch(
    client: &reqwest::Client,
    cache_dir: &Path,
    source_id: i64,
    location: &str,
) -> anyhow::Result<PathBuf> {
    if http::is_http(location) {
        let target = cache_dir.join(format!("guide-{source_id}.xml"));
        let bytes = http::download(client, location.trim(), &target).await?;
        tracing::debug!(source = source_id, bytes, "guide downloaded");
        Ok(target)
    } else {
        let path = PathBuf::from(location.trim());
        if !path.is_file() {
            anyhow::bail!("Datei {} nicht gefunden", path.display());
        }
        Ok(path)
    }
}

/// Refreshes one guide source. Failures are recorded on the source and returned.
pub async fn refresh_source(
    db: &Db,
    client: &reqwest::Client,
    cache_dir: &Path,
    source_id: i64,
) -> anyhow::Result<Option<RefreshReport>> {
    let Some(source) = livetv::get_source(db, source_id).await? else {
        return Ok(None);
    };
    match refresh_inner(db, client, cache_dir, source_id, &source.url).await {
        Ok(report) => {
            tracing::info!(
                source = source_id,
                channels = report.channels,
                programs = report.programs,
                "guide refreshed"
            );
            Ok(Some(report))
        }
        Err(err) => {
            let message = format!("{err:#}");
            // Rows of the aborted refresh are invisible; clean them up.
            if let Some(current) = livetv::get_source(db, source_id).await? {
                livetv::delete_stale_programs(db, source_id, current.generation).await?;
            }
            livetv::set_source_error(db, source_id, &message).await?;
            anyhow::bail!("guide refresh of source {source_id} failed: {message}")
        }
    }
}

async fn refresh_inner(
    db: &Db,
    client: &reqwest::Client,
    cache_dir: &Path,
    source_id: i64,
    location: &str,
) -> anyhow::Result<RefreshReport> {
    let path = fetch(client, cache_dir, source_id, location).await?;
    let generation = livetv::next_generation(db, source_id).await?;
    let cutoff = now_ms() - KEEP_PAST_MS;

    let (tx, mut rx) = tokio::sync::mpsc::channel::<Parsed>(4);
    let parser = tokio::task::spawn_blocking(move || {
        let mut channels: Vec<NewEpgChannel> = Vec::new();
        let mut batch: Vec<NewProgram> = Vec::with_capacity(PARSE_BATCH);
        let result = xmltv::open(&path)
            .map_err(xmltv::XmltvError::from)
            .and_then(|input| {
                xmltv::parse(input, |item| {
                    match item {
                        XmltvItem::Channel(c) => channels.push((c.id, c.names, c.icon)),
                        XmltvItem::Program(p) if p.end_ms >= cutoff => {
                            batch.push(p);
                            if batch.len() >= PARSE_BATCH {
                                let full =
                                    std::mem::replace(&mut batch, Vec::with_capacity(PARSE_BATCH));
                                // The writer is gone only after a database error.
                                return tx.blocking_send(Parsed::Programs(full)).is_ok();
                            }
                        }
                        XmltvItem::Program(_) => {}
                    }
                    true
                })
            });
        if !batch.is_empty() {
            let _ = tx.blocking_send(Parsed::Programs(batch));
        }
        let _ = tx.blocking_send(Parsed::Done(
            result.map(|()| channels).map_err(|e| e.to_string()),
        ));
    });

    let mut programs = 0;
    let mut channels = None;
    while let Some(message) = rx.recv().await {
        match message {
            Parsed::Programs(batch) => {
                programs += batch.len();
                livetv::insert_programs(db, source_id, generation, &batch).await?;
            }
            Parsed::Done(result) => channels = Some(result.map_err(anyhow::Error::msg)?),
        }
    }
    parser.await?;
    let channels = channels.ok_or_else(|| anyhow::anyhow!("guide parser stopped early"))?;
    livetv::finish_refresh(db, source_id, generation, &channels).await?;
    Ok(RefreshReport {
        channels: channels.len(),
        programs,
    })
}
