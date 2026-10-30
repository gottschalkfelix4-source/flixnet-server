//! The item tree as DLNA/UPnP clients browse it: top-level items of a library, the
//! children of an item, each with what a DIDL-Lite entry needs.

use sqlx::{QueryBuilder, Sqlite};

use crate::browse::{ItemFilter, push_filters};
use crate::{Db, Result};

/// One object of the tree.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct Node {
    pub id: i64,
    pub library_id: i64,
    pub kind: String,
    pub parent_id: Option<i64>,
    pub title: String,
    pub year: Option<i64>,
    pub premiere_date: Option<String>,
    pub overview: Option<String>,
    pub index_number: Option<i64>,
    pub parent_index_number: Option<i64>,
    /// Child items (seasons, episodes, pictures …).
    pub child_count: i64,
    /// Present files (audiobook parts).
    pub file_count: i64,
    pub poster_id: Option<i64>,
    /// The first present main file.
    pub file_id: Option<i64>,
    pub file_path: Option<String>,
    pub file_size: Option<i64>,
    pub duration_ms: Option<i64>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    /// Audiobooks: the author.
    pub author: Option<String>,
}

fn push_select(qb: &mut QueryBuilder<Sqlite>) {
    qb.push(
        "SELECT i.id, i.library_id, i.kind, i.parent_id, i.title, i.year, i.premiere_date,
                i.overview, i.index_number, i.parent_index_number,
                (SELECT COUNT(*) FROM items c WHERE c.parent_id = i.id) AS child_count,
                (SELECT COUNT(*) FROM media_files f WHERE f.item_id = i.id AND f.role = 'main'
                   AND f.missing_since IS NULL) AS file_count,
                (SELECT im.id FROM images im WHERE im.item_id = i.id
                   AND im.kind = CASE i.kind WHEN 'episode' THEN 'thumb' ELSE 'poster' END
                   ORDER BY CASE im.source WHEN 'local' THEN 0 WHEN 'nfo' THEN 1 ELSE 2 END,
                            im.score DESC, im.id LIMIT 1) AS poster_id,
                mf.id AS file_id, mf.path AS file_path, mf.size AS file_size,
                mf.duration_ms AS duration_ms,
                (SELECT s.width FROM media_streams s WHERE s.file_id = mf.id AND s.kind = 'video'
                   ORDER BY s.idx LIMIT 1) AS width,
                (SELECT s.height FROM media_streams s WHERE s.file_id = mf.id AND s.kind = 'video'
                   ORDER BY s.idx LIMIT 1) AS height,
                (SELECT bm.author FROM audiobook_metadata bm WHERE bm.item_id = i.id) AS author
         FROM items i
         LEFT JOIN media_files mf ON mf.id = (
             SELECT f.id FROM media_files f WHERE f.item_id = i.id AND f.role = 'main'
               AND f.missing_since IS NULL ORDER BY COALESCE(f.part, 0), f.id LIMIT 1)",
    );
}

/// Visible children of `parent` (`None`: the top level of the filter's libraries),
/// in index and title order; one page plus the total.
pub async fn children(
    db: &Db,
    filter: &ItemFilter,
    parent: Option<i64>,
    limit: i64,
    offset: i64,
) -> Result<(Vec<Node>, i64)> {
    let push_parent = |qb: &mut QueryBuilder<Sqlite>| match parent {
        Some(parent) => {
            qb.push(" AND i.parent_id = ").push_bind(parent);
        }
        None => {
            qb.push(" AND i.parent_id IS NULL");
        }
    };
    let mut page: QueryBuilder<Sqlite> = QueryBuilder::new("");
    push_select(&mut page);
    push_filters(&mut page, filter);
    push_parent(&mut page);
    page.push(
        " ORDER BY (i.parent_index_number IS NULL), i.parent_index_number,
                   (i.index_number IS NULL), i.index_number, i.sort_title, i.id
          LIMIT ",
    )
    .push_bind(limit)
    .push(" OFFSET ")
    .push_bind(offset);
    let rows: Vec<Node> = page.build_query_as().fetch_all(db.reader()).await?;

    let mut count: QueryBuilder<Sqlite> = QueryBuilder::new("SELECT COUNT(*) FROM items i");
    push_filters(&mut count, filter);
    push_parent(&mut count);
    let (total,): (i64,) = count.build_query_as().fetch_one(db.reader()).await?;
    Ok((rows, total))
}

/// One visible item.
pub async fn node(db: &Db, filter: &ItemFilter, id: i64) -> Result<Option<Node>> {
    let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new("");
    push_select(&mut qb);
    push_filters(&mut qb, filter);
    qb.push(" AND i.id = ").push_bind(id);
    Ok(qb.build_query_as().fetch_optional(db.reader()).await?)
}

/// A present main file of a visible item: (file id, item id, path, size, duration).
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct Part {
    pub id: i64,
    pub item_id: i64,
    pub path: String,
    pub size: i64,
    pub duration_ms: Option<i64>,
    pub part: Option<i64>,
}

/// The present main files of an item in part order (audiobook parts).
pub async fn parts(db: &Db, item_id: i64) -> Result<Vec<Part>> {
    Ok(sqlx::query_as(
        "SELECT id, item_id, path, size, duration_ms, part FROM media_files
         WHERE item_id = ? AND role = 'main' AND missing_since IS NULL
         ORDER BY COALESCE(part, 0), relative_path, id",
    )
    .bind(item_id)
    .fetch_all(db.reader())
    .await?)
}

/// One present main file.
pub async fn part(db: &Db, file_id: i64) -> Result<Option<Part>> {
    Ok(sqlx::query_as(
        "SELECT id, item_id, path, size, duration_ms, part FROM media_files
         WHERE id = ? AND item_id IS NOT NULL AND role = 'main' AND missing_since IS NULL",
    )
    .bind(file_id)
    .fetch_optional(db.reader())
    .await?)
}
