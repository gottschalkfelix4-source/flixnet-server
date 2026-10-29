-- Collections group movies and series: `auto` ones come from TMDB
-- (`belongs_to_collection` of a movie), `manual` ones are curated by an administrator.
-- Artwork is kept as provider URLs here (items' images table needs an item).
CREATE TABLE collections (
    id                INTEGER PRIMARY KEY,
    kind              TEXT    NOT NULL,          -- auto | manual
    provider          TEXT,                      -- tmdb (auto collections)
    provider_id       TEXT,
    name              TEXT    NOT NULL,
    sort_name         TEXT    NOT NULL,
    overview          TEXT,
    poster_url        TEXT,
    poster_blurhash   TEXT,
    backdrop_url      TEXT,
    backdrop_blurhash TEXT,
    created_at        INTEGER NOT NULL,
    updated_at        INTEGER NOT NULL
);
-- NULLs are distinct (SQLite and Postgres), so manual collections never collide.
CREATE UNIQUE INDEX collections_provider ON collections (provider, provider_id);
CREATE INDEX collections_sort ON collections (sort_name);

-- Members by stable identity (items.identity_key, e.g. "tmdb:movie:603"), so they
-- survive rescans and renames, and several copies of one movie count once. Auto
-- collections list every part TMDB knows; parts not in the library simply resolve
-- to no item.
CREATE TABLE collection_items (
    collection_id INTEGER NOT NULL REFERENCES collections (id) ON DELETE CASCADE,
    identity_key  TEXT    NOT NULL,
    ord           INTEGER NOT NULL DEFAULT 0,  -- provider release order / insertion order
    added_at      INTEGER NOT NULL,
    PRIMARY KEY (collection_id, identity_key)
);
CREATE INDEX collection_items_identity ON collection_items (identity_key);

-- Playlists belong to one user. `shared` makes them visible (read-only) to every
-- profile of the household. With `smart_filter_json` set, the playlist is a saved
-- filter evaluated live instead of a list of entries.
CREATE TABLE playlists (
    id                INTEGER PRIMARY KEY,
    user_id           INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    name              TEXT    NOT NULL,
    shared            INTEGER NOT NULL DEFAULT 0,
    smart_filter_json TEXT,
    created_at        INTEGER NOT NULL,
    updated_at        INTEGER NOT NULL
);
CREATE INDEX playlists_user ON playlists (user_id);

-- Entries by stable identity as well (see collection_items); series and seasons are
-- expanded to their episodes when added.
CREATE TABLE playlist_items (
    id           INTEGER PRIMARY KEY,
    playlist_id  INTEGER NOT NULL REFERENCES playlists (id) ON DELETE CASCADE,
    identity_key TEXT    NOT NULL,
    position     INTEGER NOT NULL,
    added_at     INTEGER NOT NULL,
    UNIQUE (playlist_id, identity_key)
);
CREATE INDEX playlist_items_order ON playlist_items (playlist_id, position);
CREATE INDEX playlist_items_identity ON playlist_items (identity_key);
