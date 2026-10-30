//! Consistent database snapshots (`VACUUM INTO`) and restoring them.
//!
//! A snapshot is a complete, compacted copy of the database taken while the server
//! keeps running; readers and the writer are not blocked. Restoring replaces the
//! database file and therefore needs the server to be stopped.

use std::path::{Path, PathBuf};

use sqlx::Connection;
use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection};

use crate::{Db, DbError, Result};

#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    #[error(transparent)]
    Db(#[from] DbError),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0} is not a Flixnet database: {1}")]
    Invalid(PathBuf, String),
}

impl From<sqlx::Error> for BackupError {
    fn from(err: sqlx::Error) -> Self {
        BackupError::Db(DbError::Sqlx(err))
    }
}

/// Writes a snapshot of the database to `dest` (which must not exist yet).
pub async fn snapshot(db: &Db, dest: &Path) -> Result<(), BackupError> {
    let dest_str = dest.to_string_lossy().into_owned();
    // A read connection suffices and never waits for the writer.
    let result = sqlx::query("VACUUM INTO ?")
        .bind(&dest_str)
        .execute(db.reader())
        .await;
    if let Err(err) = result {
        // Some SQLite builds refuse VACUUM INTO on read-only connections.
        tracing::debug!(%err, "VACUUM INTO on a reader failed; using the writer");
        let _ = std::fs::remove_file(dest);
        sqlx::query("VACUUM INTO ?")
            .bind(&dest_str)
            .execute(db.writer())
            .await?;
    }
    Ok(())
}

/// Checks that `path` is an intact Flixnet database (integrity check plus the
/// migration table). Returns the number of applied migrations.
pub async fn verify(path: &Path) -> Result<i64, BackupError> {
    if !path.is_file() {
        return Err(BackupError::Invalid(
            path.to_path_buf(),
            "file not found".into(),
        ));
    }
    let options = SqliteConnectOptions::new().filename(path).read_only(true);
    let mut conn = SqliteConnection::connect_with(&options)
        .await
        .map_err(|e| BackupError::Invalid(path.to_path_buf(), e.to_string()))?;
    let check: Result<(String,), sqlx::Error> = sqlx::query_as("PRAGMA integrity_check")
        .fetch_one(&mut conn)
        .await;
    let migrations: Result<(i64,), sqlx::Error> =
        sqlx::query_as("SELECT COUNT(*) FROM _sqlx_migrations WHERE success = 1")
            .fetch_one(&mut conn)
            .await;
    let _ = conn.close().await;
    match (check, migrations) {
        (Ok((ok,)), Ok((count,))) if ok == "ok" && count > 0 => Ok(count),
        (Ok((ok,)), _) if ok != "ok" => Err(BackupError::Invalid(path.to_path_buf(), ok)),
        (Err(e), _) | (_, Err(e)) => Err(BackupError::Invalid(path.to_path_buf(), e.to_string())),
        _ => Err(BackupError::Invalid(
            path.to_path_buf(),
            "no migrations applied".into(),
        )),
    }
}

/// Replaces the database at `db_path` with the snapshot `backup`. The current file
/// (with its WAL) is kept next to it as `<name>.before-restore-<timestamp>`; its path
/// is returned. The server must not be running.
pub async fn restore(backup: &Path, db_path: &Path) -> Result<Option<PathBuf>, BackupError> {
    verify(backup).await?;
    let stamp = crate::now_ms();
    let mut kept = None;
    if db_path.exists() {
        let name = db_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "flixnet.db".into());
        let aside = db_path.with_file_name(format!("{name}.before-restore-{stamp}"));
        std::fs::rename(db_path, &aside)?;
        for suffix in ["-wal", "-shm"] {
            let side = PathBuf::from(format!("{}{suffix}", db_path.display()));
            if side.exists() {
                std::fs::rename(&side, format!("{}{suffix}", aside.display()))?;
            }
        }
        kept = Some(aside);
    }
    if let Some(parent) = db_path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(backup, db_path)?;
    Ok(kept)
}
