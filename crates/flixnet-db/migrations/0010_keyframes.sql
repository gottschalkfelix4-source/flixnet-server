-- Keyframe index of a video stream: copied (remuxed) video can only be cut into HLS
-- segments at the file's own keyframes. Computed on first remux, then reused.
CREATE TABLE keyframes (
    file_id      INTEGER NOT NULL REFERENCES media_files (id) ON DELETE CASCADE,
    stream_index INTEGER NOT NULL,
    times_ms     TEXT    NOT NULL,   -- comma-separated, ascending
    computed_at  INTEGER NOT NULL,
    PRIMARY KEY (file_id, stream_index)
);
