-- Accounts. Names are unique case-insensitively ("Felix" and "felix" are the same user).
CREATE TABLE users (
    id            INTEGER PRIMARY KEY,
    name          TEXT    NOT NULL UNIQUE COLLATE NOCASE,
    password_hash TEXT,                       -- argon2id PHC string; NULL = no password login
    is_admin      INTEGER NOT NULL DEFAULT 0,
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL
);

-- A client installation (browser, TV app, phone). `device_uid` is chosen by the client
-- and stays stable across logins, so re-logging in replaces that device's old token.
CREATE TABLE devices (
    id         INTEGER PRIMARY KEY,
    user_id    INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    device_uid TEXT    NOT NULL,
    client     TEXT    NOT NULL,
    name       TEXT    NOT NULL,
    created_at INTEGER NOT NULL,
    last_seen  INTEGER NOT NULL,
    UNIQUE (user_id, device_uid)
);

-- Long-lived device tokens. Only the SHA-256 of a token is stored, so a leaked
-- database does not contain usable credentials.
CREATE TABLE access_tokens (
    id           INTEGER PRIMARY KEY,
    token_hash   TEXT    NOT NULL UNIQUE,
    user_id      INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    device_id    INTEGER NOT NULL REFERENCES devices (id) ON DELETE CASCADE,
    created_at   INTEGER NOT NULL,
    last_used_at INTEGER NOT NULL,
    revoked_at   INTEGER
);

CREATE INDEX access_tokens_device ON access_tokens (device_id);
