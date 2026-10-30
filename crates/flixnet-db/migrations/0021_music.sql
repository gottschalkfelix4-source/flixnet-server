-- Music libraries (kind `music`).
--
-- Items form the chain artist (the album artist) → album → track: albums and tracks
-- have parent_id = their parent and series_id = the album artist, so everything of an
-- artist is one indexed lookup. Discs are not items: a track's disc number is its
-- parent_index_number and its track number its index_number.
--
-- Tags are read from the files (lofty) during the scan and kept per file, so the scan
-- can rebuild the chain when a file is re-tagged and playback can apply ReplayGain
-- without touching the file again.
CREATE TABLE audio_tags (
    file_id             INTEGER PRIMARY KEY REFERENCES media_files (id) ON DELETE CASCADE,
    title               TEXT,
    -- Track artist(s) as shown ("A feat. B"); may differ from the album artist.
    artist              TEXT,
    album_artist        TEXT,
    album               TEXT,
    track_number        INTEGER,
    track_total         INTEGER,
    disc_number         INTEGER,
    disc_total          INTEGER,
    year                INTEGER,
    -- JSON array of genre names.
    genres_json         TEXT    NOT NULL DEFAULT '[]',
    compilation         INTEGER NOT NULL DEFAULT 0,
    -- iTunes advisory / "explicit" flag.
    explicit            INTEGER NOT NULL DEFAULT 0,
    -- ReplayGain in dB and linear peaks (R128 gains are converted to the RG scale).
    track_gain_db       REAL,
    track_peak          REAL,
    album_gain_db       REAL,
    album_peak          REAL,
    has_cover           INTEGER NOT NULL DEFAULT 0,
    has_lyrics          INTEGER NOT NULL DEFAULT 0,
    mb_recording_id     TEXT,
    mb_track_id         TEXT,
    mb_release_id       TEXT,
    mb_release_group_id TEXT,
    mb_artist_id        TEXT,
    mb_album_artist_id  TEXT,
    -- Duration from the tag reader (ffprobe fills media_files.duration_ms later).
    duration_ms         INTEGER,
    read_at             INTEGER NOT NULL
);

-- "Appears on": tracks of an artist on other artists' albums (compilations).
CREATE INDEX IF NOT EXISTS audio_tags_artist ON audio_tags (artist COLLATE NOCASE);

-- Popular tracks: play counts summed over the household's profiles.
CREATE INDEX IF NOT EXISTS user_item_data_identity ON user_item_data (identity_key);
