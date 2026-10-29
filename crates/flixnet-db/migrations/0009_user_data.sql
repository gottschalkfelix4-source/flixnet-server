-- Stable identity for user data: provider-based once matched ("tmdb:movie:603",
-- "tmdb:tv:1399:s1e2"), otherwise the content key. Watch state hangs on this, so
-- renames, re-scans and replaced files keep a user's history.
ALTER TABLE items ADD COLUMN identity_key TEXT;
UPDATE items SET identity_key = content_key;
CREATE INDEX items_identity ON items (identity_key);

CREATE TABLE user_item_data (
    user_id        INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    identity_key   TEXT    NOT NULL,
    played         INTEGER NOT NULL DEFAULT 0,
    play_count     INTEGER NOT NULL DEFAULT 0,
    position_ms    INTEGER NOT NULL DEFAULT 0,
    last_played_at INTEGER,
    favorite       INTEGER NOT NULL DEFAULT 0,
    rating         REAL,
    updated_at     INTEGER NOT NULL,
    PRIMARY KEY (user_id, identity_key)
);
