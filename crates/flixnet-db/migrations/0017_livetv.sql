-- Live TV: tuner sources, their channels, XMLTV guide sources and programs.
-- Recording rules and recordings (DVR) follow with milestone 7.5.

-- A source of live channels: an M3U playlist, an Xtream Codes account or an
-- HDHomeRun device.
CREATE TABLE tuner_hosts (
    id INTEGER PRIMARY KEY,
    -- 'm3u' | 'xtream' | 'hdhomerun'
    kind TEXT NOT NULL,
    name TEXT NOT NULL,
    -- m3u: playlist URL or file path; xtream: server URL; hdhomerun: device URL
    url TEXT NOT NULL,
    username TEXT,
    password TEXT,
    user_agent TEXT,
    -- HDHomeRun device id (from discover.json)
    device_id TEXT,
    -- Streams that may run at once; 0 = unlimited (from the device / account, or set)
    tuner_count INTEGER NOT NULL DEFAULT 0,
    -- The admin set tuner_count; imports do not overwrite it
    tuner_count_manual INTEGER NOT NULL DEFAULT 0,
    enabled INTEGER NOT NULL DEFAULT 1,
    last_import_at INTEGER,
    last_error TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

-- XMLTV guide sources (URL or file, optionally gzip-compressed).
CREATE TABLE epg_sources (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    url TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1,
    -- Programs of the last complete refresh carry this generation; older rows are
    -- deleted in batches after a refresh, so the guide never shows a half import.
    generation INTEGER NOT NULL DEFAULT 0,
    channel_count INTEGER NOT NULL DEFAULT 0,
    program_count INTEGER NOT NULL DEFAULT 0,
    last_refresh_at INTEGER,
    last_error TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE channels (
    id INTEGER PRIMARY KEY,
    tuner_host_id INTEGER NOT NULL REFERENCES tuner_hosts (id) ON DELETE CASCADE,
    -- Stable identity within the source (tvg-id, Xtream stream id, GuideNumber)
    remote_id TEXT NOT NULL,
    -- Display number ("1", "2.1")
    number TEXT,
    -- Numeric order (number, else position in the source)
    sort_order REAL NOT NULL,
    name TEXT NOT NULL,
    logo_url TEXT,
    group_title TEXT,
    stream_url TEXT NOT NULL,
    tvg_id TEXT,
    tvg_name TEXT,
    is_hd INTEGER NOT NULL DEFAULT 0,
    enabled INTEGER NOT NULL DEFAULT 1,
    -- Codecs of the stream (from the lineup or a probe), for the playback decision
    video_codec TEXT,
    audio_codec TEXT,
    width INTEGER,
    height INTEGER,
    audio_channels INTEGER,
    probed_at INTEGER,
    -- Guide channel this channel shows ('auto' = matcher, 'manual' = admin; a manual
    -- match without channel means "no guide")
    epg_source_id INTEGER REFERENCES epg_sources (id) ON DELETE SET NULL,
    epg_channel_id TEXT,
    epg_match TEXT,
    epg_match_score REAL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    UNIQUE (tuner_host_id, remote_id)
);

CREATE INDEX channels_order ON channels (enabled, sort_order, name);
CREATE INDEX channels_epg ON channels (epg_source_id, epg_channel_id);

-- `<channel>` entries of a guide source, the candidates for matching.
CREATE TABLE epg_channels (
    epg_source_id INTEGER NOT NULL REFERENCES epg_sources (id) ON DELETE CASCADE,
    channel_id TEXT NOT NULL,
    -- JSON array of display names (first = preferred)
    display_names TEXT NOT NULL,
    icon_url TEXT,
    PRIMARY KEY (epg_source_id, channel_id)
);

CREATE TABLE programs (
    id INTEGER PRIMARY KEY,
    epg_source_id INTEGER NOT NULL REFERENCES epg_sources (id) ON DELETE CASCADE,
    generation INTEGER NOT NULL,
    -- XMLTV channel id
    channel_id TEXT NOT NULL,
    start_ms INTEGER NOT NULL,
    end_ms INTEGER NOT NULL,
    title TEXT NOT NULL,
    subtitle TEXT,
    description TEXT,
    -- JSON array
    categories TEXT NOT NULL DEFAULT '[]',
    season INTEGER,
    episode INTEGER,
    episode_label TEXT,
    year INTEGER,
    icon_url TEXT,
    rating TEXT,
    is_new INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX programs_channel_time ON programs (epg_source_id, channel_id, start_ms);
CREATE INDEX programs_generation ON programs (epg_source_id, generation);

-- Program search (accent-insensitive), like items_fts.
CREATE VIRTUAL TABLE programs_fts USING fts5(
    title, subtitle, description,
    content = 'programs', content_rowid = 'id',
    tokenize = 'unicode61 remove_diacritics 2'
);

CREATE TRIGGER programs_fts_insert AFTER INSERT ON programs BEGIN
    INSERT INTO programs_fts (rowid, title, subtitle, description)
    VALUES (new.id, new.title, new.subtitle, new.description);
END;

CREATE TRIGGER programs_fts_delete AFTER DELETE ON programs BEGIN
    INSERT INTO programs_fts (programs_fts, rowid, title, subtitle, description)
    VALUES ('delete', old.id, old.title, old.subtitle, old.description);
END;
