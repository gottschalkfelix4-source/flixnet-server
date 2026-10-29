-- Libraries and the folders they cover.
CREATE TABLE libraries (
    id           INTEGER PRIMARY KEY,
    name         TEXT    NOT NULL,
    kind         TEXT    NOT NULL,          -- movies | shows
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL,
    last_scan_at INTEGER
);

CREATE TABLE library_paths (
    id         INTEGER PRIMARY KEY,
    library_id INTEGER NOT NULL REFERENCES libraries (id) ON DELETE CASCADE,
    path       TEXT    NOT NULL,
    UNIQUE (library_id, path)
);

-- Per-user library grants (used when users.all_libraries = 0).
CREATE TABLE user_library_access (
    user_id    INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    library_id INTEGER NOT NULL REFERENCES libraries (id) ON DELETE CASCADE,
    PRIMARY KEY (user_id, library_id)
);

-- Everything browsable: movies, series, seasons, episodes (music etc. later).
-- content_key is a stable identity ("tmdb:movie:603", or "local:movie:<title>:<year>"
-- before matching) that user data hangs on, so renames do not lose watch state.
CREATE TABLE items (
    id                  INTEGER PRIMARY KEY,
    library_id          INTEGER NOT NULL REFERENCES libraries (id) ON DELETE CASCADE,
    kind                TEXT    NOT NULL,   -- movie | series | season | episode
    parent_id           INTEGER REFERENCES items (id) ON DELETE CASCADE,
    series_id           INTEGER REFERENCES items (id) ON DELETE CASCADE,
    title               TEXT    NOT NULL,
    sort_title          TEXT    NOT NULL,
    original_title      TEXT,
    year                INTEGER,
    index_number        INTEGER,            -- season number / episode number
    index_end           INTEGER,            -- last episode of a multi-episode file
    parent_index_number INTEGER,            -- season number of an episode
    premiere_date       TEXT,               -- YYYY-MM-DD
    overview            TEXT,
    tagline             TEXT,
    runtime_ms          INTEGER,
    community_rating    REAL,
    official_rating     TEXT,
    age_rating          INTEGER,            -- minimum age derived from official_rating
    content_key         TEXT    NOT NULL,
    match_state         TEXT    NOT NULL DEFAULT 'unmatched', -- unmatched | matched | manual | failed
    match_confidence    REAL,
    locked_fields       TEXT    NOT NULL DEFAULT '[]',
    date_added          INTEGER NOT NULL,
    date_modified       INTEGER NOT NULL
);

CREATE UNIQUE INDEX items_content_key ON items (library_id, content_key);
CREATE INDEX items_library_kind ON items (library_id, kind);
CREATE INDEX items_parent ON items (parent_id);
CREATE INDEX items_series ON items (series_id);

-- Physical files. Several files may belong to one item (versions, multi-part).
CREATE TABLE media_files (
    id            INTEGER PRIMARY KEY,
    library_id    INTEGER NOT NULL REFERENCES libraries (id) ON DELETE CASCADE,
    item_id       INTEGER REFERENCES items (id) ON DELETE SET NULL,
    path          TEXT    NOT NULL UNIQUE,
    relative_path TEXT    NOT NULL,
    size          INTEGER NOT NULL,
    mtime         INTEGER NOT NULL,
    role          TEXT    NOT NULL DEFAULT 'main', -- main | extra
    extra_kind    TEXT,
    part          INTEGER,
    container     TEXT,
    duration_ms   INTEGER,
    bitrate       INTEGER,
    probed_at     INTEGER,
    probe_error   TEXT,
    parsed_json   TEXT,
    missing_since INTEGER,
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL
);

CREATE INDEX media_files_item ON media_files (item_id);
CREATE INDEX media_files_library ON media_files (library_id);

-- Streams as reported by ffprobe; external subtitle files use idx >= 1000.
CREATE TABLE media_streams (
    file_id         INTEGER NOT NULL REFERENCES media_files (id) ON DELETE CASCADE,
    idx             INTEGER NOT NULL,
    kind            TEXT    NOT NULL,       -- video | audio | subtitle | attachment | data
    codec           TEXT,
    profile         TEXT,
    level           INTEGER,
    width           INTEGER,
    height          INTEGER,
    fps             REAL,
    bit_depth       INTEGER,
    pix_fmt         TEXT,
    color_transfer  TEXT,
    color_primaries TEXT,
    hdr             TEXT,                   -- sdr | hdr10 | hlg | dovi
    channels        INTEGER,
    channel_layout  TEXT,
    sample_rate     INTEGER,
    bitrate         INTEGER,
    language        TEXT,
    title           TEXT,
    is_default      INTEGER NOT NULL DEFAULT 0,
    is_forced       INTEGER NOT NULL DEFAULT 0,
    is_sdh          INTEGER NOT NULL DEFAULT 0,
    is_external     INTEGER NOT NULL DEFAULT 0,
    external_path   TEXT,
    PRIMARY KEY (file_id, idx)
);

CREATE TABLE chapters (
    file_id  INTEGER NOT NULL REFERENCES media_files (id) ON DELETE CASCADE,
    idx      INTEGER NOT NULL,
    start_ms INTEGER NOT NULL,
    end_ms   INTEGER NOT NULL,
    title    TEXT,
    PRIMARY KEY (file_id, idx)
);

CREATE TABLE provider_ids (
    item_id  INTEGER NOT NULL REFERENCES items (id) ON DELETE CASCADE,
    provider TEXT    NOT NULL,              -- tmdb | tvdb | imdb
    value    TEXT    NOT NULL,
    PRIMARY KEY (item_id, provider)
);

CREATE INDEX provider_ids_lookup ON provider_ids (provider, value);
