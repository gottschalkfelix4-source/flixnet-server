//! Server-wide Live TV settings (stored as one JSON setting, changed by
//! administrators in the web UI).

use flixnet_db::{Db, DbError, settings};
use serde::{Deserialize, Serialize};

/// Settings key of [`LiveTvConfig`].
pub const SETTINGS_KEY: &str = "livetv";

/// Longest timeshift buffer an administrator may set (minutes).
pub const MAX_TIMESHIFT_MINUTES: i64 = 6 * 60;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LiveTvConfig {
    /// Timeshift buffer per running channel (0 = live window only).
    pub timeshift_minutes: i64,
}

impl Default for LiveTvConfig {
    fn default() -> Self {
        Self {
            timeshift_minutes: 120,
        }
    }
}

impl LiveTvConfig {
    pub fn timeshift(&self) -> std::time::Duration {
        std::time::Duration::from_secs(
            self.timeshift_minutes.clamp(0, MAX_TIMESHIFT_MINUTES) as u64 * 60,
        )
    }
}

/// The stored settings, or the defaults.
pub async fn load(db: &Db) -> Result<LiveTvConfig, DbError> {
    Ok(settings::get(db, SETTINGS_KEY).await?.unwrap_or_default())
}

pub async fn save(db: &Db, config: &LiveTvConfig) -> Result<(), DbError> {
    settings::set(db, SETTINGS_KEY, config).await
}
