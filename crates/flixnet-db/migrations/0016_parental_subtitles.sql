-- Parental controls (Jugendschutz): per-profile options next to max_age_rating.
-- allow_unrated: content without an age rating stays visible for a restricted profile
-- (default: hidden). blocked_genres_json: JSON array of genre names that are hidden
-- for the profile (series genres apply to their seasons and episodes).
ALTER TABLE users ADD COLUMN allow_unrated INTEGER NOT NULL DEFAULT 0;
ALTER TABLE users ADD COLUMN blocked_genres_json TEXT;

-- The rating table now uses the ages the US and UK systems state (PG-13 = 13,
-- R/TV-MA = 17, TV-14 = 14, BBFC 15 = 15); re-derive what was stored before.
UPDATE items SET age_rating = CASE upper(trim(official_rating))
        WHEN 'PG-13' THEN 13 WHEN 'RATED PG-13' THEN 13
        WHEN 'R' THEN 17 WHEN 'RATED R' THEN 17 WHEN 'TV-MA' THEN 17
        WHEN 'TV-14' THEN 14 WHEN '15' THEN 15
        ELSE age_rating END
    WHERE official_rating IS NOT NULL;

-- Subtitles downloaded from OpenSubtitles. Files next to the media file are found by
-- the scanner anyway; files in the config store (read-only libraries) only through
-- this table. Probing re-attaches both as external subtitle streams.
CREATE TABLE downloaded_subtitles (
    id               INTEGER PRIMARY KEY,
    file_id          INTEGER NOT NULL REFERENCES media_files (id) ON DELETE CASCADE,
    path             TEXT    NOT NULL,
    language         TEXT,                  -- ISO 639-2 (deu, eng, …)
    is_sdh           INTEGER NOT NULL DEFAULT 0,
    is_forced        INTEGER NOT NULL DEFAULT 0,
    provider         TEXT    NOT NULL,      -- opensubtitles
    provider_file_id TEXT,
    release_name     TEXT,
    created_at       INTEGER NOT NULL,
    UNIQUE (file_id, path)
);

-- Automatic downloads: which (file, language) pairs were already searched, so the
-- periodic task does not ask OpenSubtitles again and again.
CREATE TABLE subtitle_search_attempts (
    file_id      INTEGER NOT NULL REFERENCES media_files (id) ON DELETE CASCADE,
    language     TEXT    NOT NULL,          -- ISO 639-1 as OpenSubtitles uses it (de, en, …)
    attempted_at INTEGER NOT NULL,
    downloaded   INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (file_id, language)
);
