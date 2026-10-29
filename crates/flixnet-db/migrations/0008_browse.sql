-- Full-text search over titles and descriptions (accent-insensitive).
CREATE VIRTUAL TABLE items_fts USING fts5(
    title, original_title, overview,
    content = 'items', content_rowid = 'id',
    tokenize = 'unicode61 remove_diacritics 2'
);

CREATE TRIGGER items_fts_insert AFTER INSERT ON items BEGIN
    INSERT INTO items_fts (rowid, title, original_title, overview)
    VALUES (new.id, new.title, new.original_title, new.overview);
END;

CREATE TRIGGER items_fts_delete AFTER DELETE ON items BEGIN
    INSERT INTO items_fts (items_fts, rowid, title, original_title, overview)
    VALUES ('delete', old.id, old.title, old.original_title, old.overview);
END;

CREATE TRIGGER items_fts_update AFTER UPDATE OF title, original_title, overview ON items BEGIN
    INSERT INTO items_fts (items_fts, rowid, title, original_title, overview)
    VALUES ('delete', old.id, old.title, old.original_title, old.overview);
    INSERT INTO items_fts (rowid, title, original_title, overview)
    VALUES (new.id, new.title, new.original_title, new.overview);
END;

INSERT INTO items_fts (items_fts) VALUES ('rebuild');

-- Browsing indexes: library grids sorted by title, date added, year, rating.
CREATE INDEX items_browse_title ON items (library_id, kind, sort_title);
CREATE INDEX items_browse_added ON items (library_id, kind, date_added);
CREATE INDEX items_browse_year ON items (library_id, kind, year);
CREATE INDEX items_browse_rating ON items (library_id, kind, community_rating);
CREATE INDEX images_item_kind ON images (item_id, kind, source, score);
CREATE INDEX item_genres_genre ON item_genres (genre_id, item_id);
