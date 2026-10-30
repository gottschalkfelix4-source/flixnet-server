//! Database access for Flixnet (SQLite in WAL mode).
//!
//! Writes go through a pool with exactly one connection, so they are serialized by
//! the pool instead of fighting over SQLite's lock ("database is locked"). Reads use
//! a separate pool of read-only connections that never block on the writer.
//! Rule: never hold a write transaction across external I/O — compute first, then
//! write in short batches.

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub use sqlx::SqliteConnection;
use sqlx::SqlitePool;
use sqlx::migrate::Migrator;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};

pub mod browse;
pub mod cache;
pub mod collections;
pub mod invitations;
pub mod items;
pub mod jobs;
pub mod libraries;
pub mod livetv;
pub mod media;
pub mod metadata;
pub mod playlists;
pub mod playstate;
pub mod segments;
pub mod sessions;
pub mod settings;
pub mod trickplay;
pub mod userdata;
pub mod users;

static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    /// A UNIQUE constraint rejected the write (e.g. a user name that is taken).
    #[error("unique constraint violated")]
    Conflict,
    #[error("database migration failed: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("invalid JSON value: {0}")]
    Json(#[from] serde_json::Error),
}

impl DbError {
    /// Maps UNIQUE violations to [`DbError::Conflict`], everything else to `Sqlx`.
    pub(crate) fn from_unique(err: sqlx::Error) -> Self {
        match &err {
            sqlx::Error::Database(db) if db.is_unique_violation() => DbError::Conflict,
            _ => DbError::Sqlx(err),
        }
    }
}

pub type Result<T, E = DbError> = std::result::Result<T, E>;

/// Handle to the database: one writer connection plus a pool of readers. Cheap to clone.
#[derive(Debug, Clone)]
pub struct Db {
    writer: SqlitePool,
    reader: SqlitePool,
}

impl Db {
    /// Opens (or creates) the database file and applies pending migrations.
    pub async fn open(path: &Path) -> Result<Self> {
        let base = SqliteConnectOptions::new()
            .filename(path)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .busy_timeout(BUSY_TIMEOUT)
            .foreign_keys(true)
            .pragma("temp_store", "memory")
            // Memory-mapped reads share the OS page cache across all connections, so a
            // fresh reader does not start cold; plus a 32 MB page cache per connection.
            .pragma("mmap_size", "268435456")
            .pragma("cache_size", "-32000");

        let writer = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(base.clone().create_if_missing(true))
            .await?;
        MIGRATOR.run(&writer).await?;

        // The file exists now, so readers can be opened read-only.
        let readers =
            std::thread::available_parallelism().map_or(4, |n| n.get().clamp(2, 8)) as u32;
        let reader = SqlitePoolOptions::new()
            .max_connections(readers)
            .connect_with(base.read_only(true))
            .await?;

        tracing::debug!(path = %path.display(), readers, "database opened");
        Ok(Self { writer, reader })
    }

    /// Pool for INSERT/UPDATE/DELETE and transactions (single connection).
    pub fn writer(&self) -> &SqlitePool {
        &self.writer
    }

    /// Pool for read-only queries.
    pub fn reader(&self) -> &SqlitePool {
        &self.reader
    }

    /// Flushes the WAL into the main file and closes all connections.
    pub async fn close(&self) {
        if let Err(err) = sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
            .execute(&self.writer)
            .await
        {
            tracing::warn!(%err, "WAL checkpoint on close failed");
        }
        self.reader.close().await;
        self.writer.close().await;
    }
}

/// Current time as unix milliseconds (UTC), the timestamp format of every table.
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}
