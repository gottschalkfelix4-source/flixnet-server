-- Phase 9: photo and audiobook libraries.
--
-- Photo libraries: every folder with pictures is an item of kind `photo_album`, its
-- pictures are `photo` items and short clips `video` items (each with one media file).
-- What EXIF says about a picture lives here; `taken_at` falls back to the file's
-- modification time, so the timeline never has gaps.
CREATE TABLE photo_metadata (
    item_id      INTEGER PRIMARY KEY REFERENCES items (id) ON DELETE CASCADE,
    taken_at     INTEGER NOT NULL,          -- unix ms (EXIF local time read as UTC)
    width        INTEGER,
    height       INTEGER,
    orientation  INTEGER,                   -- EXIF orientation 1–8
    camera_make  TEXT,
    camera_model TEXT,
    lens         TEXT,
    f_number     REAL,
    exposure     TEXT,                      -- e.g. "1/250"
    iso          INTEGER,
    focal_length REAL,                      -- mm
    latitude     REAL,
    longitude    REAL,
    updated_at   INTEGER NOT NULL
);
CREATE INDEX photo_metadata_taken ON photo_metadata (taken_at);

-- Audiobook libraries: one `audiobook` item per book (folder, or a single m4b file);
-- its files are the parts in playback order (`media_files.part`).
CREATE TABLE audiobook_metadata (
    item_id      INTEGER PRIMARY KEY REFERENCES items (id) ON DELETE CASCADE,
    author       TEXT,
    narrator     TEXT,
    series_name  TEXT,
    series_index REAL,
    updated_at   INTEGER NOT NULL
);
CREATE INDEX audiobook_metadata_author ON audiobook_metadata (author);
