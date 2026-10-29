//! Channel import: read a tuner's lineup, then write it in batches.

use flixnet_db::{Db, livetv};

use crate::tuner::{AnyTuner, Tuner};

/// Result of one import.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImportReport {
    pub channels: usize,
    pub removed: u64,
}

/// Imports the channels of one host. Errors of the source are recorded on the host
/// (and returned); database errors are returned only.
pub async fn import_host(
    db: &Db,
    client: &reqwest::Client,
    host_id: i64,
) -> anyhow::Result<Option<ImportReport>> {
    let Some(host) = livetv::get_host(db, host_id).await? else {
        return Ok(None); // deleted meanwhile
    };
    let lineup = match AnyTuner::from_host(&host) {
        Ok(tuner) => tuner.lineup(client).await,
        Err(err) => Err(err),
    };
    let lineup = match lineup {
        Ok(lineup) => lineup,
        Err(err) => {
            let message = err.to_string();
            livetv::finish_import(db, host_id, Some(&message), None, None).await?;
            anyhow::bail!("channel import of tuner {host_id} failed: {message}");
        }
    };
    let (_, removed) = livetv::sync_channels(db, host_id, &lineup.channels).await?;
    livetv::finish_import(
        db,
        host_id,
        None,
        lineup.info.device_id.as_deref(),
        lineup.info.tuner_count,
    )
    .await?;
    tracing::info!(
        tuner = host_id,
        channels = lineup.channels.len(),
        removed,
        "channels imported"
    );
    Ok(Some(ImportReport {
        channels: lineup.channels.len(),
        removed,
    }))
}
