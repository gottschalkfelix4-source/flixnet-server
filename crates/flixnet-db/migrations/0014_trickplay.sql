-- Trickplay thumbnails of a media file: JPEG sprite sheets in
-- <config_dir>/cache/trickplay/{file_id}/{n}.jpg, one tile per interval, row-major.
-- Dropped when the file is probed again (its content changed).
CREATE TABLE trickplay (
    file_id     INTEGER PRIMARY KEY REFERENCES media_files (id) ON DELETE CASCADE,
    interval_ms INTEGER NOT NULL,
    tile_width  INTEGER NOT NULL,
    tile_height INTEGER NOT NULL,
    tile_columns INTEGER NOT NULL,
    tile_rows   INTEGER NOT NULL,
    thumbnails  INTEGER NOT NULL,   -- tiles in total (the last sheet may be partly filled)
    sheets      INTEGER NOT NULL,
    created_at  INTEGER NOT NULL    -- also versions the sheet URLs (browser caches)
);

-- Chapter images in <config_dir>/cache/chapters/{file_id}/{idx}.jpg: when the image
-- of a chapter was made (NULL = none yet). A new probe replaces the chapter rows, so
-- changed chapters get new images.
ALTER TABLE chapters ADD COLUMN image_at INTEGER;
