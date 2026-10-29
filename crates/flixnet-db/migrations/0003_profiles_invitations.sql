-- Household profiles: a PIN allows quick profile switching on an already signed-in
-- device (never for remote sign-in). max_age_rating limits content (NULL = unrestricted,
-- otherwise an age such as 6/12/16 that FSK/MPAA ratings are mapped to).
ALTER TABLE users ADD COLUMN pin_hash TEXT;
ALTER TABLE users ADD COLUMN max_age_rating INTEGER;
ALTER TABLE users ADD COLUMN is_hidden INTEGER NOT NULL DEFAULT 0;
-- Library access: 1 = all libraries (per-library grants arrive with libraries in phase 2).
ALTER TABLE users ADD COLUMN all_libraries INTEGER NOT NULL DEFAULT 1;

-- Single-use invitation links created by an administrator. Only a hash of the code is kept.
CREATE TABLE invitations (
    id             INTEGER PRIMARY KEY,
    code_hash      TEXT    NOT NULL UNIQUE,
    note           TEXT,
    max_age_rating INTEGER,
    created_by     INTEGER REFERENCES users (id) ON DELETE SET NULL,
    created_at     INTEGER NOT NULL,
    expires_at     INTEGER NOT NULL,
    used_at        INTEGER,
    used_by        INTEGER REFERENCES users (id) ON DELETE SET NULL
);
