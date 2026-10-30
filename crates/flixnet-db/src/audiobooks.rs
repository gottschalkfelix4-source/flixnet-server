//! Audiobook libraries: book details (author, narrator, series) and listening progress.
//!
//! A book is an `audiobook` item; its files are the parts in playback order. The
//! resume point is a position across all parts and, like all user data, hangs on the
//! book's identity (see [`crate::userdata`]).

use sqlx::{QueryBuilder, Sqlite, SqliteConnection};

use crate::browse::{ItemFilter, Restriction, push_filters};
use crate::{Db, Result, now_ms};

/// Below this a stop leaves no resume point (books are long; resume early).
pub const MIN_RESUME_MS: i64 = 10_000;
/// Within this distance of the end a book counts as finished.
pub const FINISHED_REMAINING_MS: i64 = 30_000;

#[derive(Debug, Clone, Default, PartialEq, sqlx::FromRow)]
pub struct BookMeta {
    pub author: Option<String>,
    pub narrator: Option<String>,
    pub series_name: Option<String>,
    pub series_index: Option<f64>,
}

/// Book details found in folder names (only fills what is unknown).
pub async fn init_meta(conn: &mut SqliteConnection, item_id: i64, m: &BookMeta) -> Result<()> {
    sqlx::query(
        "INSERT INTO audiobook_metadata (item_id, author, narrator, series_name, series_index, updated_at)
         VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT (item_id) DO NOTHING",
    )
    .bind(item_id)
    .bind(&m.author)
    .bind(&m.narrator)
    .bind(&m.series_name)
    .bind(m.series_index)
    .bind(now_ms())
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Replaces the book details (after reading the tags).
pub async fn set_meta(db: &Db, item_id: i64, m: &BookMeta) -> Result<()> {
    sqlx::query(
        "INSERT INTO audiobook_metadata (item_id, author, narrator, series_name, series_index, updated_at)
         VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT (item_id) DO UPDATE SET
             author = excluded.author, narrator = excluded.narrator,
             series_name = excluded.series_name, series_index = excluded.series_index,
             updated_at = excluded.updated_at",
    )
    .bind(item_id)
    .bind(&m.author)
    .bind(&m.narrator)
    .bind(&m.series_name)
    .bind(m.series_index)
    .bind(now_ms())
    .execute(db.writer())
    .await?;
    Ok(())
}

pub async fn meta(db: &Db, item_id: i64) -> Result<BookMeta> {
    let row: Option<BookMeta> = sqlx::query_as(
        "SELECT author, narrator, series_name, series_index FROM audiobook_metadata WHERE item_id = ?",
    )
    .bind(item_id)
    .fetch_optional(db.reader())
    .await?;
    Ok(row.unwrap_or_default())
}

/// Title, year, description and total runtime from the tags.
pub async fn set_details(
    db: &Db,
    item_id: i64,
    title: Option<&str>,
    sort_title: Option<&str>,
    year: Option<i64>,
    overview: Option<&str>,
    runtime_ms: Option<i64>,
) -> Result<()> {
    sqlx::query(
        "UPDATE items SET title = COALESCE(?, title), sort_title = COALESCE(?, sort_title),
                          year = COALESCE(?, year), overview = COALESCE(?, overview),
                          runtime_ms = COALESCE(?, runtime_ms), date_modified = ?
         WHERE id = ?",
    )
    .bind(title)
    .bind(sort_title)
    .bind(year)
    .bind(overview)
    .bind(runtime_ms)
    .bind(now_ms())
    .bind(item_id)
    .execute(db.writer())
    .await?;
    Ok(())
}

/// Numbers the parts of a book 1, 2, … in path order.
pub async fn renumber_parts(conn: &mut SqliteConnection, item_id: i64) -> Result<()> {
    sqlx::query(
        "UPDATE media_files SET part = (
             SELECT COUNT(*) FROM media_files o
             WHERE o.item_id = media_files.item_id AND o.relative_path <= media_files.relative_path)
         WHERE item_id = ?",
    )
    .bind(item_id)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Removes books without files.
pub async fn prune(db: &Db, library_id: i64) -> Result<u64> {
    let result = sqlx::query(
        "DELETE FROM items WHERE library_id = ? AND kind = 'audiobook'
           AND NOT EXISTS (SELECT 1 FROM media_files f WHERE f.item_id = items.id)",
    )
    .bind(library_id)
    .execute(db.writer())
    .await?;
    Ok(result.rows_affected())
}

/// A book in a list.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct BookRow {
    pub id: i64,
    pub library_id: i64,
    pub title: String,
    pub year: Option<i64>,
    pub runtime_ms: Option<i64>,
    pub author: Option<String>,
    pub narrator: Option<String>,
    pub series_name: Option<String>,
    pub series_index: Option<f64>,
    pub cover_image_id: Option<i64>,
    pub cover_blurhash: Option<String>,
    pub position_ms: i64,
    pub played: bool,
    pub last_played_at: Option<i64>,
}

#[derive(Debug, Clone, Default)]
pub struct BookQuery {
    pub library_ids: Vec<i64>,
    pub restriction: Restriction,
    pub user_id: i64,
    pub id: Option<i64>,
    /// Free text: title, author or narrator contain it (case-insensitive).
    pub search: Option<String>,
    pub limit: i64,
    pub offset: i64,
}

/// Visible books by author, series and title (or just one with `id`), plus the total.
pub async fn list(db: &Db, q: &BookQuery) -> Result<(Vec<BookRow>, i64)> {
    let mut filter = ItemFilter {
        library_ids: q.library_ids.clone(),
        kinds: vec!["audiobook".to_owned()],
        ids: q.id.into_iter().collect(),
        ..Default::default()
    };
    filter.restrict(&q.restriction);
    let pattern = q
        .search
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            format!(
                "%{}%",
                s.replace('\\', "\\\\")
                    .replace('%', "\\%")
                    .replace('_', "\\_")
            )
        });
    let push_search = |qb: &mut QueryBuilder<Sqlite>| {
        if let Some(pattern) = &pattern {
            qb.push(" AND (i.title LIKE ")
                .push_bind(pattern.clone())
                .push(" ESCAPE '\\' OR bm.author LIKE ")
                .push_bind(pattern.clone())
                .push(" ESCAPE '\\' OR bm.narrator LIKE ")
                .push_bind(pattern.clone())
                .push(" ESCAPE '\\')");
        }
    };

    let mut page: QueryBuilder<Sqlite> = QueryBuilder::new(
        "SELECT i.id, i.library_id, i.title, i.year,
                COALESCE(i.runtime_ms, (SELECT SUM(f.duration_ms) FROM media_files f
                   WHERE f.item_id = i.id AND f.missing_since IS NULL)) AS runtime_ms,
                bm.author, bm.narrator, bm.series_name, bm.series_index,
                (SELECT im.id FROM images im WHERE im.item_id = i.id AND im.kind = 'poster'
                   ORDER BY im.score DESC, im.id LIMIT 1) AS cover_image_id,
                (SELECT im.blurhash FROM images im WHERE im.item_id = i.id AND im.kind = 'poster'
                   ORDER BY im.score DESC, im.id LIMIT 1) AS cover_blurhash,
                COALESCE(ud.position_ms, 0) AS position_ms,
                COALESCE(ud.played, 0) AS played,
                ud.last_played_at
         FROM items i
         LEFT JOIN audiobook_metadata bm ON bm.item_id = i.id
         LEFT JOIN user_item_data ud ON ud.identity_key = i.identity_key AND ud.user_id = ",
    );
    page.push_bind(q.user_id);
    push_filters(&mut page, &filter);
    push_search(&mut page);
    page.push(
        " ORDER BY (bm.author IS NULL), bm.author COLLATE NOCASE, (bm.series_name IS NULL),
                   bm.series_name COLLATE NOCASE, bm.series_index, i.sort_title, i.id
          LIMIT ",
    )
    .push_bind(q.limit)
    .push(" OFFSET ")
    .push_bind(q.offset);
    let rows: Vec<BookRow> = page.build_query_as().fetch_all(db.reader()).await?;

    let mut count: QueryBuilder<Sqlite> = QueryBuilder::new(
        "SELECT COUNT(*) FROM items i LEFT JOIN audiobook_metadata bm ON bm.item_id = i.id",
    );
    push_filters(&mut count, &filter);
    push_search(&mut count);
    let (total,): (i64,) = count.build_query_as().fetch_one(db.reader()).await?;
    Ok((rows, total))
}

/// What a reported listening position means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BookProgress {
    /// Just started: nothing remembered.
    TooEarly,
    Resume(i64),
    Finished,
}

/// Books resume from a few seconds in and count as finished just before the end
/// (unlike films, where the thresholds are a share of the runtime).
pub fn classify(position_ms: i64, duration_ms: Option<i64>) -> BookProgress {
    let position = position_ms.max(0);
    match duration_ms.filter(|d| *d > 0) {
        Some(duration) if position + FINISHED_REMAINING_MS >= duration => BookProgress::Finished,
        _ if position < MIN_RESUME_MS => BookProgress::TooEarly,
        _ => BookProgress::Resume(position),
    }
}

/// Stores the listening position (or "finished": played, position reset).
pub async fn save_progress(
    db: &Db,
    user_id: i64,
    item_id: i64,
    position_ms: i64,
    duration_ms: Option<i64>,
) -> Result<BookProgress> {
    let progress = classify(position_ms, duration_ms);
    let now = now_ms();
    match progress {
        BookProgress::Resume(position) => {
            sqlx::query(
                "INSERT INTO user_item_data (user_id, identity_key, position_ms, last_played_at, updated_at)
                 SELECT ?, COALESCE(identity_key, content_key), ?, ?, ? FROM items WHERE id = ?
                 ON CONFLICT (user_id, identity_key) DO UPDATE SET
                     position_ms = excluded.position_ms,
                     last_played_at = excluded.last_played_at,
                     updated_at = excluded.updated_at",
            )
            .bind(user_id)
            .bind(position)
            .bind(now)
            .bind(now)
            .bind(item_id)
            .execute(db.writer())
            .await?;
        }
        BookProgress::Finished => {
            sqlx::query(
                "INSERT INTO user_item_data
                     (user_id, identity_key, played, play_count, position_ms, last_played_at, updated_at)
                 SELECT ?, COALESCE(identity_key, content_key), 1, 1, 0, ?, ? FROM items WHERE id = ?
                 ON CONFLICT (user_id, identity_key) DO UPDATE SET
                     played = 1,
                     play_count = CASE WHEN user_item_data.position_ms > 0
                                       THEN user_item_data.play_count + 1
                                       ELSE user_item_data.play_count END,
                     position_ms = 0,
                     last_played_at = excluded.last_played_at,
                     updated_at = excluded.updated_at",
            )
            .bind(user_id)
            .bind(now)
            .bind(now)
            .bind(item_id)
            .execute(db.writer())
            .await?;
        }
        BookProgress::TooEarly => {}
    }
    Ok(progress)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn books_resume_early_and_finish_near_the_end() {
        let hours = |h: i64| h * 3_600_000;
        assert_eq!(classify(5_000, Some(hours(10))), BookProgress::TooEarly);
        // 20 minutes into a 10 h book: a film would not resume (< 5 %), a book does.
        assert_eq!(
            classify(1_200_000, Some(hours(10))),
            BookProgress::Resume(1_200_000)
        );
        assert_eq!(
            classify(hours(10) - 10_000, Some(hours(10))),
            BookProgress::Finished
        );
        assert_eq!(classify(60_000, None), BookProgress::Resume(60_000));
        assert_eq!(classify(-5, None), BookProgress::TooEarly);
    }
}
