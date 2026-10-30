//! Music: tags per audio file, the artist → album → track chain and the listening
//! queries (album track lists, popular tracks, "appears on", play counts).
//!
//! Item layout (see migration 0021): an artist item is an album artist; albums and
//! tracks carry `parent_id` (artist resp. album) and `series_id` (the album artist).
//! A track's disc is `parent_index_number`, its position on the disc `index_number`.

use sqlx::{QueryBuilder, Sqlite, SqliteConnection};

use crate::browse::{ItemFilter, Restriction, push_filters};
use crate::{Db, Result, now_ms};

/// Item kinds of music libraries.
pub const MUSIC_KINDS: &[&str] = &["artist", "album", "track"];

/// Tags of one audio file, as read by the scanner.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AudioTags {
    pub title: Option<String>,
    /// Track artist(s) as shown, e.g. "A feat. B".
    pub artist: Option<String>,
    pub album_artist: Option<String>,
    pub album: Option<String>,
    pub track_number: Option<i64>,
    pub track_total: Option<i64>,
    pub disc_number: Option<i64>,
    pub disc_total: Option<i64>,
    pub year: Option<i64>,
    pub genres: Vec<String>,
    pub compilation: bool,
    pub explicit: bool,
    pub track_gain_db: Option<f64>,
    pub track_peak: Option<f64>,
    pub album_gain_db: Option<f64>,
    pub album_peak: Option<f64>,
    pub has_cover: bool,
    pub has_lyrics: bool,
    pub mb_recording_id: Option<String>,
    pub mb_track_id: Option<String>,
    pub mb_release_id: Option<String>,
    pub mb_release_group_id: Option<String>,
    pub mb_artist_id: Option<String>,
    pub mb_album_artist_id: Option<String>,
    pub duration_ms: Option<i64>,
}

#[derive(sqlx::FromRow)]
struct TagRow {
    title: Option<String>,
    artist: Option<String>,
    album_artist: Option<String>,
    album: Option<String>,
    track_number: Option<i64>,
    track_total: Option<i64>,
    disc_number: Option<i64>,
    disc_total: Option<i64>,
    year: Option<i64>,
    genres_json: String,
    compilation: bool,
    explicit: bool,
    track_gain_db: Option<f64>,
    track_peak: Option<f64>,
    album_gain_db: Option<f64>,
    album_peak: Option<f64>,
    has_cover: bool,
    has_lyrics: bool,
    mb_recording_id: Option<String>,
    mb_track_id: Option<String>,
    mb_release_id: Option<String>,
    mb_release_group_id: Option<String>,
    mb_artist_id: Option<String>,
    mb_album_artist_id: Option<String>,
    duration_ms: Option<i64>,
}

impl From<TagRow> for AudioTags {
    fn from(r: TagRow) -> Self {
        Self {
            title: r.title,
            artist: r.artist,
            album_artist: r.album_artist,
            album: r.album,
            track_number: r.track_number,
            track_total: r.track_total,
            disc_number: r.disc_number,
            disc_total: r.disc_total,
            year: r.year,
            genres: serde_json::from_str(&r.genres_json).unwrap_or_default(),
            compilation: r.compilation,
            explicit: r.explicit,
            track_gain_db: r.track_gain_db,
            track_peak: r.track_peak,
            album_gain_db: r.album_gain_db,
            album_peak: r.album_peak,
            has_cover: r.has_cover,
            has_lyrics: r.has_lyrics,
            mb_recording_id: r.mb_recording_id,
            mb_track_id: r.mb_track_id,
            mb_release_id: r.mb_release_id,
            mb_release_group_id: r.mb_release_group_id,
            mb_artist_id: r.mb_artist_id,
            mb_album_artist_id: r.mb_album_artist_id,
            duration_ms: r.duration_ms,
        }
    }
}

macro_rules! tag_columns {
    () => {
        "title, artist, album_artist, album, track_number, track_total, disc_number,
         disc_total, year, genres_json, compilation, explicit, track_gain_db, track_peak,
         album_gain_db, album_peak, has_cover, has_lyrics, mb_recording_id, mb_track_id,
         mb_release_id, mb_release_group_id, mb_artist_id, mb_album_artist_id, duration_ms"
    };
}

/// Stores (or replaces) the tags of a file (inside the scanner's batch transaction).
pub async fn store_tags(conn: &mut SqliteConnection, file_id: i64, t: &AudioTags) -> Result<()> {
    sqlx::query(concat!(
        "INSERT OR REPLACE INTO audio_tags (file_id, ",
        tag_columns!(),
        ", read_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
    ))
    .bind(file_id)
    .bind(&t.title)
    .bind(&t.artist)
    .bind(&t.album_artist)
    .bind(&t.album)
    .bind(t.track_number)
    .bind(t.track_total)
    .bind(t.disc_number)
    .bind(t.disc_total)
    .bind(t.year)
    .bind(serde_json::to_string(&t.genres)?)
    .bind(t.compilation)
    .bind(t.explicit)
    .bind(t.track_gain_db)
    .bind(t.track_peak)
    .bind(t.album_gain_db)
    .bind(t.album_peak)
    .bind(t.has_cover)
    .bind(t.has_lyrics)
    .bind(&t.mb_recording_id)
    .bind(&t.mb_track_id)
    .bind(&t.mb_release_id)
    .bind(&t.mb_release_group_id)
    .bind(&t.mb_artist_id)
    .bind(&t.mb_album_artist_id)
    .bind(t.duration_ms)
    .bind(now_ms())
    .execute(&mut *conn)
    .await?;
    Ok(())
}

pub async fn tags(db: &Db, file_id: i64) -> Result<Option<AudioTags>> {
    let row: Option<TagRow> = sqlx::query_as(concat!(
        "SELECT ",
        tag_columns!(),
        " FROM audio_tags WHERE file_id = ?"
    ))
    .bind(file_id)
    .fetch_optional(db.reader())
    .await?;
    Ok(row.map(Into::into))
}

/// Points a (re-tagged) file at its new track item.
pub async fn attach_file(conn: &mut SqliteConnection, file_id: i64, item_id: i64) -> Result<()> {
    sqlx::query("UPDATE media_files SET item_id = ?, updated_at = ? WHERE id = ?")
        .bind(item_id)
        .bind(now_ms())
        .bind(file_id)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Fields of a music item taken from tags. Tags are the source of truth for music,
/// so they overwrite earlier tag values, but never fields locked by a manual edit.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TagFields<'a> {
    pub title: &'a str,
    pub sort_title: &'a str,
    pub year: Option<i64>,
    pub index_number: Option<i64>,
    pub parent_index_number: Option<i64>,
    /// Explicit tracks get an age rating; everything else stays unrated.
    pub age_rating: Option<i64>,
}

pub async fn apply_tag_fields(
    conn: &mut SqliteConnection,
    item_id: i64,
    f: &TagFields<'_>,
) -> Result<()> {
    let (locked_json,): (String,) = sqlx::query_as("SELECT locked_fields FROM items WHERE id = ?")
        .bind(item_id)
        .fetch_one(&mut *conn)
        .await?;
    let locked: Vec<String> = serde_json::from_str(&locked_json).unwrap_or_default();
    let free = |name: &str| !locked.iter().any(|l| l == name);
    sqlx::query(
        "UPDATE items SET
            title = CASE WHEN ? THEN ? ELSE title END,
            sort_title = CASE WHEN ? THEN ? ELSE sort_title END,
            year = CASE WHEN ? THEN ? ELSE year END,
            index_number = ?, parent_index_number = ?,
            age_rating = CASE WHEN ? THEN ? ELSE age_rating END,
            official_rating = CASE WHEN ? THEN ? ELSE official_rating END,
            date_modified = ?
         WHERE id = ?",
    )
    .bind(free("title"))
    .bind(f.title)
    .bind(free("sort_title"))
    .bind(f.sort_title)
    .bind(free("year"))
    .bind(f.year)
    .bind(f.index_number)
    .bind(f.parent_index_number)
    .bind(free("age_rating"))
    .bind(f.age_rating)
    .bind(free("official_rating"))
    .bind(f.age_rating.map(|_| "Explicit"))
    .bind(now_ms())
    .bind(item_id)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Replaces an item's genres inside an open transaction.
pub async fn set_genres(conn: &mut SqliteConnection, item_id: i64, names: &[String]) -> Result<()> {
    sqlx::query("DELETE FROM item_genres WHERE item_id = ?")
        .bind(item_id)
        .execute(&mut *conn)
        .await?;
    for name in names.iter().map(|n| n.trim()).filter(|n| !n.is_empty()) {
        sqlx::query("INSERT INTO genres (name) VALUES (?) ON CONFLICT (name) DO NOTHING")
            .bind(name)
            .execute(&mut *conn)
            .await?;
        sqlx::query(
            "INSERT INTO item_genres (item_id, genre_id)
             SELECT ?, id FROM genres WHERE name = ? ON CONFLICT DO NOTHING",
        )
        .bind(item_id)
        .bind(name)
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

/// A present audio file of an album's tracks.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct AlbumFile {
    pub file_id: i64,
    pub track_id: i64,
    pub path: String,
    pub relative_path: String,
    pub disc_number: Option<i64>,
    pub track_number: Option<i64>,
}

/// Files of an album's tracks (or of all tracks of an artist), in disc/track order.
pub async fn files_below(db: &Db, item_id: i64) -> Result<Vec<AlbumFile>> {
    let rows = sqlx::query_as(
        "SELECT f.id AS file_id, i.id AS track_id, f.path, f.relative_path,
                i.parent_index_number AS disc_number, i.index_number AS track_number
         FROM items i JOIN media_files f ON f.item_id = i.id
         WHERE i.kind = 'track' AND (i.parent_id = ?1 OR i.series_id = ?1)
           AND f.missing_since IS NULL
         ORDER BY i.parent_index_number, i.index_number, f.path",
    )
    .bind(item_id)
    .fetch_all(db.reader())
    .await?;
    Ok(rows)
}

/// What an album's tracks say about the album as a whole.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AlbumSummary {
    /// Earliest year of its tracks.
    pub year: Option<i64>,
    /// Genres of its tracks, most frequent first.
    pub genres: Vec<String>,
    pub compilation: bool,
    pub tracks: i64,
    pub duration_ms: i64,
    pub mb_release_id: Option<String>,
    pub mb_release_group_id: Option<String>,
    pub mb_album_artist_id: Option<String>,
    /// A file with embedded cover art.
    pub cover_file: Option<String>,
}

pub async fn album_summary(db: &Db, album_id: i64) -> Result<AlbumSummary> {
    #[derive(sqlx::FromRow)]
    struct Row {
        path: String,
        duration_ms: Option<i64>,
        year: Option<i64>,
        genres_json: Option<String>,
        compilation: Option<bool>,
        has_cover: Option<bool>,
        mb_release_id: Option<String>,
        mb_release_group_id: Option<String>,
        mb_album_artist_id: Option<String>,
    }
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT f.path, COALESCE(f.duration_ms, t.duration_ms) AS duration_ms, t.year,
                t.genres_json, t.compilation, t.has_cover, t.mb_release_id,
                t.mb_release_group_id, t.mb_album_artist_id
         FROM items i JOIN media_files f ON f.item_id = i.id
         LEFT JOIN audio_tags t ON t.file_id = f.id
         WHERE i.kind = 'track' AND i.parent_id = ? AND f.missing_since IS NULL
         ORDER BY i.parent_index_number, i.index_number, f.path",
    )
    .bind(album_id)
    .fetch_all(db.reader())
    .await?;
    let mut out = AlbumSummary::default();
    let mut genre_counts: Vec<(String, usize)> = Vec::new();
    let mut seen_tracks = std::collections::HashSet::new();
    for row in rows {
        // Several versions of a track count once.
        if seen_tracks.insert(row.path.clone()) {
            out.tracks += 1;
            out.duration_ms += row.duration_ms.unwrap_or(0);
        }
        out.year = match (out.year, row.year) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        out.compilation |= row.compilation.unwrap_or(false);
        let genres: Vec<String> = row
            .genres_json
            .as_deref()
            .and_then(|j| serde_json::from_str(j).ok())
            .unwrap_or_default();
        for genre in genres {
            match genre_counts
                .iter_mut()
                .find(|(g, _)| g.eq_ignore_ascii_case(&genre))
            {
                Some((_, n)) => *n += 1,
                None => genre_counts.push((genre, 1)),
            }
        }
        if out.cover_file.is_none() && row.has_cover.unwrap_or(false) {
            out.cover_file = Some(row.path.clone());
        }
        out.mb_release_id = out.mb_release_id.or(row.mb_release_id);
        out.mb_release_group_id = out.mb_release_group_id.or(row.mb_release_group_id);
        out.mb_album_artist_id = out.mb_album_artist_id.or(row.mb_album_artist_id);
    }
    // Stable: most frequent first, then first seen.
    genre_counts.sort_by_key(|g| std::cmp::Reverse(g.1));
    out.genres = genre_counts.into_iter().map(|(g, _)| g).take(5).collect();
    Ok(out)
}

/// Sets an album's runtime (sum of its tracks) — a derived value, not locked.
pub async fn set_runtime(db: &Db, item_id: i64, runtime_ms: Option<i64>) -> Result<()> {
    sqlx::query("UPDATE items SET runtime_ms = ? WHERE id = ?")
        .bind(runtime_ms)
        .bind(item_id)
        .execute(db.writer())
        .await?;
    Ok(())
}

/// One track as shown in lists and played by the player.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct TrackRow {
    pub id: i64,
    pub library_id: i64,
    pub title: String,
    pub album_id: Option<i64>,
    pub album_title: Option<String>,
    pub album_artist_id: Option<i64>,
    pub album_artist: Option<String>,
    /// Track artist from the tags (falls back to the album artist in the API).
    pub artist: Option<String>,
    pub disc_number: Option<i64>,
    pub track_number: Option<i64>,
    pub year: Option<i64>,
    pub file_id: Option<i64>,
    pub duration_ms: Option<i64>,
    pub track_gain_db: Option<f64>,
    pub track_peak: Option<f64>,
    pub album_gain_db: Option<f64>,
    pub album_peak: Option<f64>,
    pub has_lyrics: bool,
    /// Best cover of the album.
    pub cover_id: Option<i64>,
    pub cover_blurhash: Option<String>,
    pub favorite: bool,
    pub play_count: i64,
    pub last_played_at: Option<i64>,
}

/// Track rows for specific track ids (callers check visibility and keep the order).
pub async fn track_rows(db: &Db, ids: &[i64], user_id: Option<i64>) -> Result<Vec<TrackRow>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new(
        "SELECT i.id, i.library_id, i.title, i.parent_id AS album_id, al.title AS album_title,
                i.series_id AS album_artist_id, ar.title AS album_artist, t.artist,
                i.parent_index_number AS disc_number, i.index_number AS track_number,
                COALESCE(i.year, al.year) AS year, f.id AS file_id,
                COALESCE(f.duration_ms, t.duration_ms) AS duration_ms,
                t.track_gain_db, t.track_peak, t.album_gain_db, t.album_peak,
                COALESCE(t.has_lyrics, 0) AS has_lyrics,
                (SELECT im.id FROM images im WHERE im.item_id = i.parent_id AND im.kind = 'poster'
                   ORDER BY CASE im.source WHEN 'local' THEN 0 WHEN 'nfo' THEN 1 ELSE 2 END, im.score DESC, im.id
                   LIMIT 1) AS cover_id,
                (SELECT im.blurhash FROM images im WHERE im.item_id = i.parent_id AND im.kind = 'poster'
                   ORDER BY CASE im.source WHEN 'local' THEN 0 WHEN 'nfo' THEN 1 ELSE 2 END, im.score DESC, im.id
                   LIMIT 1) AS cover_blurhash,
                COALESCE(ud.favorite, 0) AS favorite,
                COALESCE(ud.play_count, 0) AS play_count,
                ud.last_played_at
         FROM items i
         LEFT JOIN items al ON al.id = i.parent_id
         LEFT JOIN items ar ON ar.id = i.series_id
         LEFT JOIN media_files f ON f.id = (SELECT f2.id FROM media_files f2
              WHERE f2.item_id = i.id AND f2.missing_since IS NULL ORDER BY f2.id LIMIT 1)
         LEFT JOIN audio_tags t ON t.file_id = f.id
         LEFT JOIN user_item_data ud ON ud.identity_key = i.identity_key AND ud.user_id = ",
    );
    qb.push_bind(user_id.unwrap_or(-1));
    qb.push(" WHERE i.kind = 'track' AND i.id IN (");
    {
        let mut list = qb.separated(", ");
        for id in ids {
            list.push_bind(*id);
        }
    }
    qb.push(")");
    let mut rows: Vec<TrackRow> = qb.build_query_as().fetch_all(db.reader()).await?;
    let position: std::collections::HashMap<i64, usize> =
        ids.iter().enumerate().map(|(p, id)| (*id, p)).collect();
    rows.sort_by_key(|r| position.get(&r.id).copied().unwrap_or(usize::MAX));
    Ok(rows)
}

fn visible_filter(library_ids: &[i64], restriction: &Restriction, kind: &str) -> ItemFilter {
    let mut filter = ItemFilter {
        library_ids: library_ids.to_vec(),
        kinds: vec![kind.to_owned()],
        ..Default::default()
    };
    filter.restrict(restriction);
    filter
}

/// An artist's most played tracks (play counts of all profiles), then album order.
/// Covers the artist's own albums and tracks credited to them on other albums.
pub async fn popular_tracks(
    db: &Db,
    artist_id: i64,
    artist_name: &str,
    library_ids: &[i64],
    restriction: &Restriction,
    limit: i64,
) -> Result<Vec<i64>> {
    let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new("SELECT i.id FROM items i");
    push_filters(&mut qb, &visible_filter(library_ids, restriction, "track"));
    qb.push(" AND (i.series_id = ")
        .push_bind(artist_id)
        .push(
            " OR EXISTS (SELECT 1 FROM media_files f JOIN audio_tags t ON t.file_id = f.id
                         WHERE f.item_id = i.id AND t.artist = ",
        )
        .push_bind(artist_name.to_owned())
        .push(
            " COLLATE NOCASE))
         ORDER BY (SELECT COALESCE(SUM(ud.play_count), 0) FROM user_item_data ud
                   WHERE ud.identity_key = i.identity_key) DESC,
                  (SELECT al.year FROM items al WHERE al.id = i.parent_id),
                  i.parent_id, i.parent_index_number, i.index_number, i.id
         LIMIT ",
        )
        .push_bind(limit);
    let rows: Vec<(i64,)> = qb.build_query_as().fetch_all(db.reader()).await?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

/// Albums of other album artists (compilations) with tracks by this artist.
pub async fn appears_on(
    db: &Db,
    artist_id: i64,
    artist_name: &str,
    library_ids: &[i64],
    restriction: &Restriction,
) -> Result<Vec<i64>> {
    let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new("SELECT i.id FROM items i");
    push_filters(&mut qb, &visible_filter(library_ids, restriction, "album"));
    qb.push(" AND (i.series_id IS NULL OR i.series_id != ")
        .push_bind(artist_id)
        .push(
            ") AND EXISTS (SELECT 1 FROM items tr JOIN media_files f ON f.item_id = tr.id
                          JOIN audio_tags t ON t.file_id = f.id
                          WHERE tr.parent_id = i.id AND t.artist = ",
        )
        .push_bind(artist_name.to_owned())
        .push(" COLLATE NOCASE) ORDER BY i.year, i.sort_title, i.id LIMIT 100");
    let rows: Vec<(i64,)> = qb.build_query_as().fetch_all(db.reader()).await?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

/// A finished listen: one more play, last played now, marked played.
pub async fn record_play(db: &Db, user_id: i64, item_id: i64) -> Result<bool> {
    let now = now_ms();
    let result = sqlx::query(
        "INSERT INTO user_item_data
             (user_id, identity_key, played, play_count, position_ms, last_played_at, updated_at)
         SELECT ?, COALESCE(identity_key, content_key), 1, 1, 0, ?, ? FROM items
         WHERE id = ? AND kind = 'track'
         ON CONFLICT (user_id, identity_key) DO UPDATE SET
             played = 1,
             play_count = user_item_data.play_count + 1,
             last_played_at = excluded.last_played_at,
             updated_at = excluded.updated_at",
    )
    .bind(user_id)
    .bind(now)
    .bind(now)
    .bind(item_id)
    .execute(db.writer())
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Albums and tracks counted for a library card.
pub async fn counts(db: &Db, library_id: i64) -> Result<(i64, i64, i64)> {
    let row = sqlx::query_as(
        "SELECT
            (SELECT COUNT(*) FROM items WHERE library_id = ?1 AND kind = 'artist'),
            (SELECT COUNT(*) FROM items WHERE library_id = ?1 AND kind = 'album'),
            (SELECT COUNT(*) FROM items WHERE library_id = ?1 AND kind = 'track')",
    )
    .bind(library_id)
    .fetch_one(db.reader())
    .await?;
    Ok(row)
}
