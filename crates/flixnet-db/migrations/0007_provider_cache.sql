-- Cached provider API responses (saves API quota and makes rescans fast).
CREATE TABLE provider_cache (
    provider   TEXT    NOT NULL,
    key        TEXT    NOT NULL,
    body       TEXT    NOT NULL,
    expires_at INTEGER NOT NULL,
    PRIMARY KEY (provider, key)
);
