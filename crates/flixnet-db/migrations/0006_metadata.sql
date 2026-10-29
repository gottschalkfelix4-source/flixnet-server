-- Artwork per item: local files next to the media or provider images (cached later).
CREATE TABLE images (
    id         INTEGER PRIMARY KEY,
    item_id    INTEGER NOT NULL REFERENCES items (id) ON DELETE CASCADE,
    kind       TEXT    NOT NULL,            -- poster | backdrop | logo | thumb | banner
    source     TEXT    NOT NULL,            -- local | nfo | tmdb | tvdb | fanart
    path       TEXT,                        -- local file
    url        TEXT,                        -- remote original
    language   TEXT,
    width      INTEGER,
    height     INTEGER,
    blurhash   TEXT,
    score      REAL    NOT NULL DEFAULT 0,  -- higher = preferred
    created_at INTEGER NOT NULL
);
CREATE INDEX images_item ON images (item_id, kind);

CREATE TABLE genres (
    id   INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE COLLATE NOCASE
);
CREATE TABLE item_genres (
    item_id  INTEGER NOT NULL REFERENCES items (id) ON DELETE CASCADE,
    genre_id INTEGER NOT NULL REFERENCES genres (id) ON DELETE CASCADE,
    PRIMARY KEY (item_id, genre_id)
);

CREATE TABLE studios (
    id   INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE COLLATE NOCASE
);
CREATE TABLE item_studios (
    item_id   INTEGER NOT NULL REFERENCES items (id) ON DELETE CASCADE,
    studio_id INTEGER NOT NULL REFERENCES studios (id) ON DELETE CASCADE,
    PRIMARY KEY (item_id, studio_id)
);

CREATE TABLE people (
    id         INTEGER PRIMARY KEY,
    name       TEXT    NOT NULL,
    sort_name  TEXT    NOT NULL,
    tmdb_id    INTEGER UNIQUE,
    image_url  TEXT,
    image_path TEXT,
    created_at INTEGER NOT NULL
);
CREATE INDEX people_name ON people (name COLLATE NOCASE);

CREATE TABLE credits (
    id        INTEGER PRIMARY KEY,
    item_id   INTEGER NOT NULL REFERENCES items (id) ON DELETE CASCADE,
    person_id INTEGER NOT NULL REFERENCES people (id) ON DELETE CASCADE,
    role      TEXT    NOT NULL,             -- actor | director | writer | producer | creator
    character TEXT,
    ord       INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX credits_item ON credits (item_id, role, ord);
CREATE INDEX credits_person ON credits (person_id);

-- When metadata was last refreshed (local NFO/artwork and providers).
ALTER TABLE items ADD COLUMN metadata_refreshed_at INTEGER;
