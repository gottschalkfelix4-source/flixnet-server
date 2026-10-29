-- Key/value server settings. Values are JSON so a setting can hold any shape.
-- Times are unix milliseconds (UTC), which keeps the schema portable to Postgres BIGINT.
CREATE TABLE settings (
    key        TEXT    PRIMARY KEY NOT NULL,
    value_json TEXT    NOT NULL,
    updated_at INTEGER NOT NULL
);
