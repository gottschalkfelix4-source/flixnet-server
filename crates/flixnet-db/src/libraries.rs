//! Libraries, their folders and per-user access.

use crate::{Db, Result, now_ms};

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Library {
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub created_at: i64,
    pub last_scan_at: Option<i64>,
}

macro_rules! library_columns {
    () => {
        "id, name, kind, created_at, last_scan_at"
    };
}

pub async fn create(db: &Db, name: &str, kind: &str, paths: &[String]) -> Result<Library> {
    let now = now_ms();
    let mut tx = db.writer().begin().await?;
    let library: Library = sqlx::query_as(concat!(
        "INSERT INTO libraries (name, kind, created_at, updated_at) VALUES (?, ?, ?, ?) RETURNING ",
        library_columns!()
    ))
    .bind(name)
    .bind(kind)
    .bind(now)
    .bind(now)
    .fetch_one(&mut *tx)
    .await?;
    for path in paths {
        sqlx::query("INSERT INTO library_paths (library_id, path) VALUES (?, ?)")
            .bind(library.id)
            .bind(path)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(library)
}

pub async fn list(db: &Db) -> Result<Vec<Library>> {
    let libraries = sqlx::query_as(concat!(
        "SELECT ",
        library_columns!(),
        " FROM libraries ORDER BY name COLLATE NOCASE"
    ))
    .fetch_all(db.reader())
    .await?;
    Ok(libraries)
}

/// Libraries a user may see: all for `all_libraries` users, otherwise their grants.
pub async fn list_for_user(db: &Db, user_id: i64) -> Result<Vec<Library>> {
    let libraries = sqlx::query_as(concat!(
        "SELECT ",
        library_columns!(),
        " FROM libraries l
          WHERE (SELECT all_libraries FROM users WHERE id = ?) = 1
             OR EXISTS (SELECT 1 FROM user_library_access a WHERE a.user_id = ? AND a.library_id = l.id)
          ORDER BY name COLLATE NOCASE"
    ))
    .bind(user_id)
    .bind(user_id)
    .fetch_all(db.reader())
    .await?;
    Ok(libraries)
}

pub async fn get(db: &Db, id: i64) -> Result<Option<Library>> {
    let library = sqlx::query_as(concat!(
        "SELECT ",
        library_columns!(),
        " FROM libraries WHERE id = ?"
    ))
    .bind(id)
    .fetch_optional(db.reader())
    .await?;
    Ok(library)
}

pub async fn paths(db: &Db, library_id: i64) -> Result<Vec<String>> {
    let rows: Vec<(String,)> =
        sqlx::query_as("SELECT path FROM library_paths WHERE library_id = ? ORDER BY path")
            .bind(library_id)
            .fetch_all(db.reader())
            .await?;
    Ok(rows.into_iter().map(|(p,)| p).collect())
}

/// Renames and/or replaces the folder list.
pub async fn update(
    db: &Db,
    id: i64,
    name: Option<&str>,
    paths: Option<&[String]>,
) -> Result<bool> {
    let mut tx = db.writer().begin().await?;
    let result =
        sqlx::query("UPDATE libraries SET name = COALESCE(?, name), updated_at = ? WHERE id = ?")
            .bind(name)
            .bind(now_ms())
            .bind(id)
            .execute(&mut *tx)
            .await?;
    if result.rows_affected() == 0 {
        return Ok(false);
    }
    if let Some(paths) = paths {
        sqlx::query("DELETE FROM library_paths WHERE library_id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        for path in paths {
            sqlx::query("INSERT INTO library_paths (library_id, path) VALUES (?, ?)")
                .bind(id)
                .bind(path)
                .execute(&mut *tx)
                .await?;
        }
    }
    tx.commit().await?;
    Ok(true)
}

pub async fn delete(db: &Db, id: i64) -> Result<bool> {
    let result = sqlx::query("DELETE FROM libraries WHERE id = ?")
        .bind(id)
        .execute(db.writer())
        .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn mark_scanned(db: &Db, id: i64) -> Result<()> {
    sqlx::query("UPDATE libraries SET last_scan_at = ? WHERE id = ?")
        .bind(now_ms())
        .bind(id)
        .execute(db.writer())
        .await?;
    Ok(())
}

#[derive(Debug, Clone, Default, PartialEq, Eq, sqlx::FromRow)]
pub struct LibraryCounts {
    pub movies: i64,
    pub series: i64,
    pub episodes: i64,
    pub files: i64,
    pub unprobed: i64,
}

pub async fn counts(db: &Db, library_id: i64) -> Result<LibraryCounts> {
    let counts = sqlx::query_as(
        "SELECT
            (SELECT COUNT(*) FROM items WHERE library_id = ?1 AND kind = 'movie') AS movies,
            (SELECT COUNT(*) FROM items WHERE library_id = ?1 AND kind = 'series') AS series,
            (SELECT COUNT(*) FROM items WHERE library_id = ?1 AND kind = 'episode') AS episodes,
            (SELECT COUNT(*) FROM media_files WHERE library_id = ?1 AND missing_since IS NULL) AS files,
            (SELECT COUNT(*) FROM media_files WHERE library_id = ?1 AND missing_since IS NULL
                                                   AND probed_at IS NULL) AS unprobed",
    )
    .bind(library_id)
    .fetch_one(db.reader())
    .await?;
    Ok(counts)
}

// ---- Access grants ------------------------------------------------------------------

pub async fn granted_library_ids(db: &Db, user_id: i64) -> Result<Vec<i64>> {
    let rows: Vec<(i64,)> = sqlx::query_as(
        "SELECT library_id FROM user_library_access WHERE user_id = ? ORDER BY library_id",
    )
    .bind(user_id)
    .fetch_all(db.reader())
    .await?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

/// Sets `all_libraries` and, when restricted, the exact list of granted libraries.
pub async fn set_access(db: &Db, user_id: i64, all: bool, library_ids: &[i64]) -> Result<()> {
    let mut tx = db.writer().begin().await?;
    sqlx::query("UPDATE users SET all_libraries = ? WHERE id = ?")
        .bind(all)
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM user_library_access WHERE user_id = ?")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    if !all {
        for library_id in library_ids {
            sqlx::query(
                "INSERT INTO user_library_access (user_id, library_id)
                 SELECT ?, id FROM libraries WHERE id = ?",
            )
            .bind(user_id)
            .bind(library_id)
            .execute(&mut *tx)
            .await?;
        }
    }
    tx.commit().await?;
    Ok(())
}

/// Can this user see this library?
pub async fn user_can_access(db: &Db, user_id: i64, library_id: i64) -> Result<bool> {
    let (ok,): (bool,) = sqlx::query_as(
        "SELECT (SELECT all_libraries FROM users WHERE id = ?1) = 1
             OR EXISTS (SELECT 1 FROM user_library_access WHERE user_id = ?1 AND library_id = ?2)",
    )
    .bind(user_id)
    .bind(library_id)
    .fetch_one(db.reader())
    .await?;
    Ok(ok)
}
