//! Background jobs: channel imports and guide refreshes (plus their scheduled tasks).
//! Both re-run the channel matcher afterwards.

use std::path::PathBuf;
use std::time::Duration;

use flixnet_core::jobs::{JobContext, JobHandler, JobOptions, JobQueue, Pool};
use flixnet_core::scheduler::TaskDef;
use flixnet_db::{DbError, livetv};
use serde::{Deserialize, Serialize};

use crate::{guide, import, matching};

// ---- Channel import ----------------------------------------------------------------

pub struct ImportChannels {
    pub http: reqwest::Client,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportPayload {
    /// One host, or every enabled host.
    pub tuner_host_id: Option<i64>,
}

impl JobHandler for ImportChannels {
    const KIND: &'static str = "livetv.import_channels";
    const POOL: Pool = Pool::Io;
    const MAX_ATTEMPTS: i64 = 2;
    type Payload = ImportPayload;

    async fn run(&self, ctx: JobContext, payload: ImportPayload) -> anyhow::Result<()> {
        let hosts: Vec<i64> = match payload.tuner_host_id {
            Some(id) => vec![id],
            None => livetv::list_hosts(ctx.db())
                .await?
                .into_iter()
                .filter(|h| h.enabled)
                .map(|h| h.id)
                .collect(),
        };
        let mut failed = None;
        for id in hosts {
            if let Err(err) = import::import_host(ctx.db(), &self.http, id).await {
                tracing::warn!(tuner = id, "{err:#}");
                failed = Some(err);
            }
        }
        matching::auto_match(ctx.db()).await?;
        failed.map_or(Ok(()), Err)
    }
}

/// Queues an import (deduplicated per host).
pub async fn queue_import(
    queue: &JobQueue,
    tuner_host_id: Option<i64>,
    priority: i64,
) -> Result<Option<i64>, DbError> {
    let key = tuner_host_id.map_or_else(|| "all".to_owned(), |id| id.to_string());
    queue
        .enqueue::<ImportChannels>(
            &ImportPayload { tuner_host_id },
            JobOptions::priority(priority).dedupe(format!("livetv-import:{key}")),
        )
        .await
}

// ---- Guide refresh -----------------------------------------------------------------

pub struct RefreshGuide {
    pub http: reqwest::Client,
    /// Where downloaded guide files are kept (`<config>/cache/epg`).
    pub cache_dir: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefreshPayload {
    /// One source, or every enabled source.
    pub epg_source_id: Option<i64>,
}

impl JobHandler for RefreshGuide {
    const KIND: &'static str = "livetv.refresh_guide";
    const POOL: Pool = Pool::Io;
    const MAX_ATTEMPTS: i64 = 2;
    type Payload = RefreshPayload;

    async fn run(&self, ctx: JobContext, payload: RefreshPayload) -> anyhow::Result<()> {
        let sources: Vec<i64> = match payload.epg_source_id {
            Some(id) => vec![id],
            None => livetv::list_sources(ctx.db())
                .await?
                .into_iter()
                .filter(|s| s.enabled)
                .map(|s| s.id)
                .collect(),
        };
        let mut failed = None;
        for id in sources {
            if ctx.is_cancelled() {
                break;
            }
            if let Err(err) = guide::refresh_source(ctx.db(), &self.http, &self.cache_dir, id).await
            {
                tracing::warn!(source = id, "{err:#}");
                failed = Some(err);
            }
        }
        matching::auto_match(ctx.db()).await?;
        // New programs: schedule what the recording rules want.
        if let Err(err) = crate::dvr::plan::plan(ctx.db()).await {
            tracing::warn!(%err, "planning recordings after the guide refresh failed");
        }
        failed.map_or(Ok(()), Err)
    }
}

/// Queues a guide refresh (deduplicated per source).
pub async fn queue_refresh(
    queue: &JobQueue,
    epg_source_id: Option<i64>,
    priority: i64,
) -> Result<Option<i64>, DbError> {
    let key = epg_source_id.map_or_else(|| "all".to_owned(), |id| id.to_string());
    queue
        .enqueue::<RefreshGuide>(
            &RefreshPayload { epg_source_id },
            JobOptions::priority(priority).dedupe(format!("livetv-guide:{key}")),
        )
        .await
}

/// Recurring tasks: guide twice a day, channel lists once a day.
pub fn tasks() -> Vec<TaskDef> {
    vec![
        TaskDef::new::<RefreshGuide>(
            "refresh_guide",
            "TV-Programm (XMLTV) aller Quellen neu laden",
            Duration::from_secs(12 * 60 * 60),
            &RefreshPayload {
                epg_source_id: None,
            },
        ),
        TaskDef::new::<ImportChannels>(
            "import_channels",
            "Senderlisten aller Tuner neu einlesen",
            Duration::from_secs(24 * 60 * 60),
            &ImportPayload {
                tuner_host_id: None,
            },
        ),
    ]
}
