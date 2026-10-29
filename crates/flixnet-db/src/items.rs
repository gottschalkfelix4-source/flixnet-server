//! Browsable items (movies, series, seasons, episodes).

use sqlx::SqliteConnection;

use crate::{Db, Result, now_ms};

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct Item {
    pub id: i64,
    pub library_id: i64,
    pub kind: String,
    pub parent_id: Option<i64>,
    pub series_id: Option<i64>,
    pub title: String,
    pub sort_title: String,
    pub year: Option<i64>,
    pub index_number: Option<i64>,
    pub index_end: Option<i64>,
    pub parent_index_number: Option<i64>,
    pub premiere_date: Option<String>,
    pub content_key: String,
    pub match_state: String,
    pub date_added: i64,
}

macro_rules! item_columns {
    () => {
        "id, library_id, kind, parent_id, series_id, title, sort_title, year, index_number,
         index_end, parent_index_number, premiere_date, content_key, match_state, date_added"
    };
}

#[derive(Debug, Clone)]
pub struct NewItem<'a> {
    pub library_id: i64,
    pub kind: &'a str,
    pub parent_id: Option<i64>,
    pub series_id: Option<i64>,
    pub title: &'a str,
    pub sort_title: &'a str,
    pub year: Option<i64>,
    pub index_number: Option<i64>,
    pub index_end: Option<i64>,
    pub parent_index_number: Option<i64>,
    pub premiere_date: Option<&'a str>,
    pub content_key: &'a str,
}

/// Inserts an item or returns the existing one with the same content key.
/// Existing titles are never overwritten here (they may be matched or edited).
pub async fn upsert(conn: &mut SqliteConnection, item: &NewItem<'_>) -> Result<i64> {
    let now = now_ms();
    let (id,): (i64,) = sqlx::query_as(
        "INSERT INTO items (library_id, kind, parent_id, series_id, title, sort_title, year,
                            index_number, index_end, parent_index_number, premiere_date,
                            content_key, date_added, date_modified)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (library_id, content_key) DO UPDATE SET
             index_end = CASE WHEN excluded.index_end IS NULL THEN items.index_end
                              ELSE MAX(COALESCE(items.index_end, 0), excluded.index_end) END
         RETURNING id",
    )
    .bind(item.library_id)
    .bind(item.kind)
    .bind(item.parent_id)
    .bind(item.series_id)
    .bind(item.title)
    .bind(item.sort_title)
    .bind(item.year)
    .bind(item.index_number)
    .bind(item.index_end)
    .bind(item.parent_index_number)
    .bind(item.premiere_date)
    .bind(item.content_key)
    .bind(now)
    .bind(now)
    .fetch_one(&mut *conn)
    .await?;
    Ok(id)
}

/// Records a provider id unless the item already has one for that provider.
pub async fn add_provider_id(
    conn: &mut SqliteConnection,
    item_id: i64,
    provider: &str,
    value: &str,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO provider_ids (item_id, provider, value) VALUES (?, ?, ?)
         ON CONFLICT (item_id, provider) DO NOTHING",
    )
    .bind(item_id)
    .bind(provider)
    .bind(value)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

pub async fn get(db: &Db, id: i64) -> Result<Option<Item>> {
    let item = sqlx::query_as(concat!(
        "SELECT ",
        item_columns!(),
        " FROM items WHERE id = ?"
    ))
    .bind(id)
    .fetch_optional(db.reader())
    .await?;
    Ok(item)
}

/// All items of a kind in a library, ordered for stable output.
pub async fn list_by_kind(db: &Db, library_id: i64, kind: &str) -> Result<Vec<Item>> {
    let items = sqlx::query_as(concat!(
        "SELECT ",
        item_columns!(),
        " FROM items WHERE library_id = ? AND kind = ?
          ORDER BY parent_index_number, index_number, sort_title"
    ))
    .bind(library_id)
    .bind(kind)
    .fetch_all(db.reader())
    .await?;
    Ok(items)
}

pub async fn provider_ids(db: &Db, item_id: i64) -> Result<Vec<(String, String)>> {
    let rows = sqlx::query_as(
        "SELECT provider, value FROM provider_ids WHERE item_id = ? ORDER BY provider",
    )
    .bind(item_id)
    .fetch_all(db.reader())
    .await?;
    Ok(rows)
}

/// Removes items that no longer have any present file (and no children with files).
/// Movies/episodes first, then empty seasons, then empty series.
pub async fn prune_orphans(db: &Db, library_id: i64) -> Result<u64> {
    let mut removed = 0;
    let mut tx = db.writer().begin().await?;
    for sql in [
        "DELETE FROM items WHERE library_id = ? AND kind IN ('movie', 'episode')
           AND NOT EXISTS (SELECT 1 FROM media_files f WHERE f.item_id = items.id)",
        "DELETE FROM items WHERE library_id = ? AND kind = 'season'
           AND NOT EXISTS (SELECT 1 FROM items c WHERE c.parent_id = items.id)",
        "DELETE FROM items WHERE library_id = ? AND kind = 'series'
           AND NOT EXISTS (SELECT 1 FROM items c WHERE c.series_id = items.id)
           AND NOT EXISTS (SELECT 1 FROM media_files f WHERE f.item_id = items.id)",
    ] {
        removed += sqlx::query(sql)
            .bind(library_id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
    }
    tx.commit().await?;
    Ok(removed)
}
