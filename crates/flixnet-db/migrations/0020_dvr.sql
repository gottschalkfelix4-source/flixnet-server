-- DVR (milestone 7.5): recording rules and recordings. Also Live TV parental
-- controls per profile, guide series ids, and guide sources other than XMLTV files
-- (Schedules Direct, tvheadend; milestone 7.6).

-- What to record: one broadcast ('program': channel + start, because program ids
-- change with every guide refresh) or a series ('series': by title or guide series
-- id, optionally on one channel and within a time window of the day).
CREATE TABLE recording_rules (
    id INTEGER PRIMARY KEY,
    -- 'program' | 'series'
    kind TEXT NOT NULL,
    user_id INTEGER REFERENCES users (id) ON DELETE SET NULL,
    title TEXT NOT NULL,
    series_id TEXT,
    channel_id INTEGER REFERENCES channels (id) ON DELETE CASCADE,
    program_start_ms INTEGER,
    -- "Nur neue Folgen"
    new_only INTEGER NOT NULL DEFAULT 0,
    -- Time window in minutes after local midnight [start, end); NULL = any time
    window_start_min INTEGER,
    window_end_min INTEGER,
    -- Local time of the window and of air dates = UTC + this many minutes
    utc_offset_min INTEGER NOT NULL DEFAULT 0,
    padding_before_s INTEGER NOT NULL DEFAULT 0,
    padding_after_s INTEGER NOT NULL DEFAULT 0,
    -- Higher wins tuner conflicts
    priority INTEGER NOT NULL DEFAULT 0,
    -- Retention: keep the newest N completed recordings (NULL = all)
    keep_last INTEGER,
    enabled INTEGER NOT NULL DEFAULT 1,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

-- One broadcast to record, recording or recorded. A guide snapshot is kept, so a
-- recording outlives guide refreshes and deleted channels.
CREATE TABLE recordings (
    id INTEGER PRIMARY KEY,
    rule_id INTEGER REFERENCES recording_rules (id) ON DELETE SET NULL,
    channel_id INTEGER REFERENCES channels (id) ON DELETE SET NULL,
    channel_name TEXT NOT NULL,
    -- 'scheduled' | 'recording' | 'completed' | 'failed' | 'cancelled'
    status TEXT NOT NULL,
    -- Scheduled, but no tuner left for it (see priority)
    conflict INTEGER NOT NULL DEFAULT 0,
    title TEXT NOT NULL,
    subtitle TEXT,
    description TEXT,
    -- JSON array
    categories TEXT NOT NULL DEFAULT '[]',
    season INTEGER,
    episode INTEGER,
    year INTEGER,
    rating TEXT,
    series_id TEXT,
    is_new INTEGER NOT NULL DEFAULT 0,
    icon_url TEXT,
    -- The broadcast; the recording runs from start - padding_before to end + padding_after
    start_ms INTEGER NOT NULL,
    end_ms INTEGER NOT NULL,
    padding_before_s INTEGER NOT NULL DEFAULT 0,
    padding_after_s INTEGER NOT NULL DEFAULT 0,
    priority INTEGER NOT NULL DEFAULT 0,
    utc_offset_min INTEGER NOT NULL DEFAULT 0,
    -- File of the recording (first part, without part suffix); parts > 1 means
    -- "<stem> - part<n>.ts" files (a server restart interrupted it)
    path TEXT,
    parts INTEGER NOT NULL DEFAULT 0,
    bytes INTEGER NOT NULL DEFAULT 0,
    error TEXT,
    started_at INTEGER,
    finished_at INTEGER,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

-- A broadcast slot is recorded once, whichever rules want it.
CREATE UNIQUE INDEX recordings_slot ON recordings (channel_id, start_ms);
CREATE INDEX recordings_status ON recordings (status, start_ms);
CREATE INDEX recordings_rule ON recordings (rule_id, status);

-- Parental controls: whether a profile may use Live TV at all.
ALTER TABLE users ADD COLUMN live_tv_allowed INTEGER NOT NULL DEFAULT 1;

-- Series identity from the guide (XMLTV dd_progid, Schedules Direct seriesId).
ALTER TABLE programs ADD COLUMN series_id TEXT;

-- Guide source kind: 'xmltv' | 'schedules_direct' (url = lineup id) | 'tvheadend'
-- (the EPG of a tvheadend tuner host).
ALTER TABLE epg_sources ADD COLUMN kind TEXT NOT NULL DEFAULT 'xmltv';
ALTER TABLE epg_sources ADD COLUMN tuner_host_id INTEGER REFERENCES tuner_hosts (id) ON DELETE CASCADE;
