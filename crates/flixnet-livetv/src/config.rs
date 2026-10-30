//! Server-wide Live TV settings (stored as one JSON setting, changed by
//! administrators in the web UI).

use std::path::{Path, PathBuf};

use flixnet_db::{Db, DbError, settings};
use serde::{Deserialize, Serialize};

/// Settings key of [`LiveTvConfig`].
pub const SETTINGS_KEY: &str = "livetv";

/// Longest timeshift buffer an administrator may set (minutes).
pub const MAX_TIMESHIFT_MINUTES: i64 = 6 * 60;
/// Longest padding before or after a recording (minutes).
pub const MAX_PADDING_MINUTES: i64 = 120;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LiveTvConfig {
    /// Timeshift buffer per running channel (0 = live window only).
    pub timeshift_minutes: i64,
    /// Folder recordings are written to (`Serien/…`, `Filme/…`); `None` =
    /// `<config>/recordings`.
    pub recordings_path: Option<String>,
    /// Default padding of new recording rules.
    pub padding_before_minutes: i64,
    pub padding_after_minutes: i64,
}

impl Default for LiveTvConfig {
    fn default() -> Self {
        Self {
            timeshift_minutes: 120,
            recordings_path: None,
            padding_before_minutes: 1,
            padding_after_minutes: 5,
        }
    }
}

impl LiveTvConfig {
    pub fn timeshift(&self) -> std::time::Duration {
        std::time::Duration::from_secs(
            self.timeshift_minutes.clamp(0, MAX_TIMESHIFT_MINUTES) as u64 * 60,
        )
    }

    /// The recordings folder, or `default` when none is configured.
    pub fn recordings_root(&self, default: &Path) -> PathBuf {
        self.recordings_path
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map_or_else(|| default.to_owned(), PathBuf::from)
    }
}

/// The stored settings, or the defaults.
pub async fn load(db: &Db) -> Result<LiveTvConfig, DbError> {
    Ok(settings::get(db, SETTINGS_KEY).await?.unwrap_or_default())
}

pub async fn save(db: &Db, config: &LiveTvConfig) -> Result<(), DbError> {
    settings::set(db, SETTINGS_KEY, config).await
}
