-- Intro, credits and recap of a file, detected from the audio the episodes of a
-- season share (source 'fingerprint') or set by an administrator ('manual').
-- One segment per kind and file; times in ms from the start of the file.
CREATE TABLE media_segments (
    file_id     INTEGER NOT NULL REFERENCES media_files (id) ON DELETE CASCADE,
    kind        TEXT    NOT NULL,   -- intro | credits | recap
    start_ms    INTEGER NOT NULL,
    end_ms      INTEGER NOT NULL,
    source      TEXT    NOT NULL,   -- fingerprint | blackdetect | manual
    confidence  REAL    NOT NULL,   -- 0..1, share of compared episodes that agree
    computed_at INTEGER NOT NULL,
    PRIMARY KEY (file_id, kind)
);

-- Chromaprint fingerprints of a file's opening ('head') and end ('tail'), cached so
-- a new episode is compared with its season without fingerprinting the others again.
-- `items` holds little-endian u32 values; empty when the file has no usable audio.
CREATE TABLE audio_fingerprints (
    file_id      INTEGER NOT NULL REFERENCES media_files (id) ON DELETE CASCADE,
    region       TEXT    NOT NULL,   -- head | tail
    stream_index INTEGER NOT NULL,
    start_ms     INTEGER NOT NULL,
    items        BLOB    NOT NULL,
    computed_at  INTEGER NOT NULL,
    PRIMARY KEY (file_id, region)
);

-- Per-library switch for the analysis (on by default).
ALTER TABLE libraries ADD COLUMN intro_detection INTEGER NOT NULL DEFAULT 1;

-- Personal playback preferences, typed JSON values per key.
CREATE TABLE user_preferences (
    user_id    INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    key        TEXT    NOT NULL,
    value_json TEXT    NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, key)
);
