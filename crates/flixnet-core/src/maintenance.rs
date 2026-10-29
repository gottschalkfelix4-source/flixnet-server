//! Built-in housekeeping jobs.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::jobs::{JobContext, JobHandler, Pool};
use crate::scheduler::TaskDef;

/// Deletes finished job records older than `keep_days`.
pub struct CleanupJobs;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CleanupJobsPayload {
    pub keep_days: i64,
}

impl JobHandler for CleanupJobs {
    const KIND: &'static str = "maintenance.cleanup_jobs";
    const POOL: Pool = Pool::Io;
    type Payload = CleanupJobsPayload;

    async fn run(&self, ctx: JobContext, payload: Self::Payload) -> anyhow::Result<()> {
        let cutoff = flixnet_db::now_ms() - payload.keep_days * 24 * 60 * 60 * 1000;
        let removed = flixnet_db::jobs::delete_finished_before(ctx.db(), cutoff).await?;
        tracing::info!(removed, "old job records cleaned up");
        Ok(())
    }
}

/// Default recurring maintenance tasks.
pub fn tasks() -> Vec<TaskDef> {
    vec![TaskDef::new::<CleanupJobs>(
        "cleanup_jobs",
        "Erledigte Aufgaben nach 7 Tagen aus dem Verlauf entfernen",
        Duration::from_secs(24 * 60 * 60),
        &CleanupJobsPayload { keep_days: 7 },
    )]
}
