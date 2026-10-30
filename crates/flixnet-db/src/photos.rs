//! Photo libraries: EXIF details of pictures, the timeline and albums (folders).
//!
//! Pictures are `photo` items, short clips `video` items and folders `photo_album`
//! items; visibility follows the usual item rules (libraries, parental controls).

use sqlx::{QueryBuilder, Sqlite, SqliteConnection};

use crate::browse::{ItemFilter, Restriction, push_filters};
use crate::{Db, Result, now_ms};

/// Item kinds of a photo library that show up on the timeline.
pub const TIMELINE_KINDS: [&str; 2] = ["photo", "video"];

/// What the scanner read from a picture (EXIF and the decoded size).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PhotoMeta {
    /// Unix ms; the file's modification time when EXIF has no date.
    pub taken_at: i64,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub orientation: Option<i64>,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub lens: Option<String>,
    pub f_number: Option<f64>,
    pub exposure: Option<String>,
    pub iso: Option<i64>,
    pub focal_length: Option<f64>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
}

/// Creates or replaces the details of a picture or clip.
pub async fn upsert_meta(conn: &mut SqliteConnection, item_id: i64, m: &PhotoMeta) -> Result<()> {
    sqlx::query(
        "INSERT INTO photo_metadata (item_id, taken_at, width, height, orientation, camera_make,
                                     camera_model, lens, f_number, exposure, iso, focal_length,
                                     latitude, longitude, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (item_id) DO UPDATE SET
             taken_at = excluded.taken_at, width = excluded.width, height = excluded.height,
             orientation = excluded.orientation, camera_make = excluded.camera_make,
             camera_model = excluded.camera_model, lens = excluded.lens,
             f_number = excluded.f_number, exposure = excluded.exposure, iso = excluded.iso,
             focal_length = excluded.focal_length, latitude = excluded.latitude,
             longitude = excluded.longitude, updated_at = excluded.updated_at",
    )
    .bind(item_id)
    .bind(m.taken_at)
    .bind(m.width)
    .bind(m.height)
    .bind(m.orientation)
    .bind(&m.camera_make)
    .bind(&m.camera_model)
    .bind(&m.lens)
    .bind(m.f_number)
    .bind(&m.exposure)
    .bind(m.iso)
    .bind(m.focal_length)
    .bind(m.latitude)
    .bind(m.longitude)
    .bind(now_ms())
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Sets the item's year/date from the capture time (for sorting and search).
pub async fn set_item_date(conn: &mut SqliteConnection, item_id: i64, taken_at: i64) -> Result<()> {
    sqlx::query(
        "UPDATE items SET year = CAST(strftime('%Y', ? / 1000, 'unixepoch') AS INTEGER),
                          premiere_date = strftime('%Y-%m-%d', ? / 1000, 'unixepoch')
         WHERE id = ?",
    )
    .bind(taken_at)
    .bind(taken_at)
    .bind(item_id)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// The picture itself is the item's poster (resized by the image cache). Replaces an
/// earlier one for the same item.
pub async fn set_picture(conn: &mut SqliteConnection, item_id: i64, path: &str) -> Result<()> {
    sqlx::query("DELETE FROM images WHERE item_id = ? AND source = 'local' AND kind = 'poster'")
        .bind(item_id)
        .execute(&mut *conn)
        .await?;
    sqlx::query(
        "INSERT INTO images (item_id, kind, source, path, score, created_at)
         VALUES (?, 'poster', 'local', ?, 1, ?)",
    )
    .bind(item_id)
    .bind(path)
    .bind(now_ms())
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Pictures need no ffprobe: mark the file as analyzed with what the scanner knows.
pub async fn mark_file_read(
    conn: &mut SqliteConnection,
    file_id: i64,
    container: &str,
    error: Option<&str>,
) -> Result<()> {
    sqlx::query(
        "UPDATE media_files SET probed_at = ?, container = ?, probe_error = ?, updated_at = ?
         WHERE id = ?",
    )
    .bind(now_ms())
    .bind(container)
    .bind(error)
    .bind(now_ms())
    .bind(file_id)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Removes pictures without files and albums without pictures.
pub async fn prune(db: &Db, library_id: i64) -> Result<u64> {
    let mut removed = 0;
    let mut tx = db.writer().begin().await?;
    for sql in [
        "DELETE FROM items WHERE library_id = ? AND kind IN ('photo', 'video')
           AND NOT EXISTS (SELECT 1 FROM media_files f WHERE f.item_id = items.id)",
        "DELETE FROM items WHERE library_id = ? AND kind = 'photo_album'
           AND NOT EXISTS (SELECT 1 FROM items c WHERE c.parent_id = items.id)",
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

/// A picture or clip as the timeline shows it.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct PhotoRow {
    pub id: i64,
    pub library_id: i64,
    pub kind: String,
    /// The album (folder); `None` for pictures directly in a library folder.
    pub parent_id: Option<i64>,
    pub title: String,
    pub taken_at: i64,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub lens: Option<String>,
    pub f_number: Option<f64>,
    pub exposure: Option<String>,
    pub iso: Option<i64>,
    pub focal_length: Option<f64>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub image_id: Option<i64>,
    pub blurhash: Option<String>,
    pub file_id: Option<i64>,
    pub duration_ms: Option<i64>,
}

/// Which pictures to list.
#[derive(Debug, Clone, Default)]
pub struct PhotoQuery {
    pub library_ids: Vec<i64>,
    /// Only this album.
    pub album_id: Option<i64>,
    pub restriction: Restriction,
    /// Only one picture (detail view).
    pub id: Option<i64>,
    /// Oldest first (default: newest first).
    pub ascending: bool,
    pub limit: i64,
    pub offset: i64,
}

impl PhotoQuery {
    fn filter(&self) -> ItemFilter {
        let mut filter = ItemFilter {
            library_ids: self.library_ids.clone(),
            kinds: TIMELINE_KINDS.map(str::to_owned).to_vec(),
            parent_id: self.album_id,
            ids: self.id.into_iter().collect(),
            ..Default::default()
        };
        filter.restrict(&self.restriction);
        filter
    }
}

fn push_photo_select(qb: &mut QueryBuilder<Sqlite>) {
    qb.push(
        "SELECT i.id, i.library_id, i.kind, i.parent_id, i.title,
                COALESCE(pm.taken_at, i.date_added) AS taken_at,
                pm.width, pm.height, pm.camera_make, pm.camera_model, pm.lens, pm.f_number,
                pm.exposure, pm.iso, pm.focal_length, pm.latitude, pm.longitude,
                (SELECT im.id FROM images im WHERE im.item_id = i.id AND im.kind = 'poster'
                   ORDER BY im.score DESC, im.id LIMIT 1) AS image_id,
                (SELECT im.blurhash FROM images im WHERE im.item_id = i.id AND im.kind = 'poster'
                   ORDER BY im.score DESC, im.id LIMIT 1) AS blurhash,
                (SELECT f.id FROM media_files f WHERE f.item_id = i.id AND f.missing_since IS NULL
                   ORDER BY f.id LIMIT 1) AS file_id,
                (SELECT f.duration_ms FROM media_files f WHERE f.item_id = i.id
                   AND f.missing_since IS NULL ORDER BY f.id LIMIT 1) AS duration_ms
         FROM items i LEFT JOIN photo_metadata pm ON pm.item_id = i.id",
    );
}

/// One page of the timeline (newest first) plus the total.
pub async fn list(db: &Db, q: &PhotoQuery) -> Result<(Vec<PhotoRow>, i64)> {
    let filter = q.filter();
    let mut page: QueryBuilder<Sqlite> = QueryBuilder::new("");
    push_photo_select(&mut page);
    push_filters(&mut page, &filter);
    page.push(if q.ascending {
        " ORDER BY COALESCE(pm.taken_at, i.date_added) ASC, i.id ASC"
    } else {
        " ORDER BY COALESCE(pm.taken_at, i.date_added) DESC, i.id DESC"
    });
    page.push(" LIMIT ")
        .push_bind(q.limit)
        .push(" OFFSET ")
        .push_bind(q.offset);
    let rows: Vec<PhotoRow> = page.build_query_as().fetch_all(db.reader()).await?;

    let mut count: QueryBuilder<Sqlite> = QueryBuilder::new("SELECT COUNT(*) FROM items i");
    push_filters(&mut count, &filter);
    let (total,): (i64,) = count.build_query_as().fetch_one(db.reader()).await?;
    Ok((rows, total))
}

/// A single visible picture or clip.
pub async fn get(db: &Db, id: i64, q: &PhotoQuery) -> Result<Option<PhotoRow>> {
    let one = PhotoQuery {
        id: Some(id),
        album_id: None,
        limit: 1,
        offset: 0,
        ..q.clone()
    };
    Ok(list(db, &one).await?.0.pop())
}

/// Months of the timeline (`YYYY-MM`, newest first) with their number of pictures.
pub async fn months(db: &Db, q: &PhotoQuery) -> Result<Vec<(String, i64)>> {
    let filter = q.filter();
    let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new(
        "SELECT strftime('%Y-%m', COALESCE(pm.taken_at, i.date_added) / 1000, 'unixepoch') AS month,
                COUNT(*) AS n
         FROM items i LEFT JOIN photo_metadata pm ON pm.item_id = i.id",
    );
    push_filters(&mut qb, &filter);
    qb.push(" GROUP BY month ORDER BY month DESC");
    Ok(qb.build_query_as().fetch_all(db.reader()).await?)
}

/// An album (folder) with its visible pictures.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct AlbumRow {
    pub id: i64,
    pub library_id: i64,
    pub title: String,
    pub photo_count: i64,
    pub cover_image_id: Option<i64>,
    pub cover_blurhash: Option<String>,
    pub first_taken_at: Option<i64>,
    pub last_taken_at: Option<i64>,
}

/// Albums in the given libraries that contain visible pictures, newest first.
pub async fn albums(
    db: &Db,
    library_ids: &[i64],
    restriction: &Restriction,
) -> Result<Vec<AlbumRow>> {
    // Children are filtered like the timeline; albums themselves like any item.
    let mut albums = ItemFilter {
        library_ids: library_ids.to_vec(),
        kinds: vec!["photo_album".to_owned()],
        ..Default::default()
    };
    albums.restrict(restriction);
    let children = PhotoQuery {
        library_ids: library_ids.to_vec(),
        restriction: restriction.clone(),
        ..Default::default()
    }
    .filter();

    let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new(
        "WITH visible AS (SELECT i.id, i.parent_id, COALESCE(pm.taken_at, i.date_added) AS taken_at
                          FROM items i LEFT JOIN photo_metadata pm ON pm.item_id = i.id",
    );
    push_filters(&mut qb, &children);
    qb.push(
        ")
         SELECT i.id, i.library_id, i.title,
                (SELECT COUNT(*) FROM visible v WHERE v.parent_id = i.id) AS photo_count,
                (SELECT im.id FROM visible v JOIN images im ON im.item_id = v.id AND im.kind = 'poster'
                   WHERE v.parent_id = i.id ORDER BY v.taken_at DESC, v.id DESC LIMIT 1) AS cover_image_id,
                (SELECT im.blurhash FROM visible v JOIN images im ON im.item_id = v.id AND im.kind = 'poster'
                   WHERE v.parent_id = i.id ORDER BY v.taken_at DESC, v.id DESC LIMIT 1) AS cover_blurhash,
                (SELECT MIN(v.taken_at) FROM visible v WHERE v.parent_id = i.id) AS first_taken_at,
                (SELECT MAX(v.taken_at) FROM visible v WHERE v.parent_id = i.id) AS last_taken_at
         FROM items i",
    );
    push_filters(&mut qb, &albums);
    qb.push(
        " AND EXISTS (SELECT 1 FROM visible v WHERE v.parent_id = i.id)
          ORDER BY last_taken_at DESC, i.sort_title, i.id",
    );
    Ok(qb.build_query_as().fetch_all(db.reader()).await?)
}
