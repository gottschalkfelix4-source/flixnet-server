-- Append-only playback history: starts, progress milestones (25/50/75 %) and stops.
-- Keyed by identity like user_item_data, so history survives renames and rescans;
-- item_id is kept for convenience and nulled when the item disappears. The basis for
-- recommendations, Trakt sync and history import later on.
CREATE TABLE watch_events (
    id           INTEGER PRIMARY KEY,
    user_id      INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    identity_key TEXT    NOT NULL,
    item_id      INTEGER REFERENCES items (id) ON DELETE SET NULL,
    event        TEXT    NOT NULL,          -- start | progress | stop
    position_ms  INTEGER NOT NULL,
    duration_ms  INTEGER,
    play_method  TEXT,                      -- direct_play | remux | transcode
    completed    INTEGER NOT NULL DEFAULT 0, -- stop events: counted as watched
    session_id   TEXT,                      -- playback session (groups one viewing)
    device_id    INTEGER REFERENCES devices (id) ON DELETE SET NULL,
    client       TEXT,
    device_name  TEXT,
    created_at   INTEGER NOT NULL
);

CREATE INDEX watch_events_user_time ON watch_events (user_id, created_at);
CREATE INDEX watch_events_identity ON watch_events (identity_key, created_at);

-- "Weiterschauen" and "Nächste Folge" read a user's most recent activity.
CREATE INDEX user_item_data_recent ON user_item_data (user_id, last_played_at);
