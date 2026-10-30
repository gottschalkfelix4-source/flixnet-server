-- Operations and integrations (phase 9): notification targets, Trakt accounts and
-- single sign-on identities. Server-wide integration settings (SMTP, Trakt app,
-- OIDC provider, backups) live in `settings` as JSON.

-- Where server events are sent: a webhook, an ntfy topic, a Discord webhook or e-mail.
CREATE TABLE notification_targets (
    id          INTEGER PRIMARY KEY,
    name        TEXT    NOT NULL,
    kind        TEXT    NOT NULL,               -- webhook | ntfy | discord | email
    enabled     INTEGER NOT NULL DEFAULT 1,
    config_json TEXT    NOT NULL DEFAULT '{}',  -- kind-specific (URL, topic, recipients, secrets)
    events_json TEXT    NOT NULL DEFAULT '[]',  -- event kinds this target receives
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
);

-- A user's connection to Trakt.tv (OAuth tokens never leave the server).
CREATE TABLE trakt_accounts (
    user_id          INTEGER PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
    username         TEXT,
    access_token     TEXT    NOT NULL,
    refresh_token    TEXT    NOT NULL,
    expires_at       INTEGER NOT NULL,
    scrobble         INTEGER NOT NULL DEFAULT 1,
    sync_ratings     INTEGER NOT NULL DEFAULT 1,
    last_sync_at     INTEGER,
    last_sync_report TEXT,
    created_at       INTEGER NOT NULL,
    updated_at       INTEGER NOT NULL
);

-- Accounts of an OpenID Connect provider linked to Flixnet users (`sub` per issuer).
CREATE TABLE oidc_identities (
    id            INTEGER PRIMARY KEY,
    user_id       INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    issuer        TEXT    NOT NULL,
    subject       TEXT    NOT NULL,
    email         TEXT,
    created_at    INTEGER NOT NULL,
    last_login_at INTEGER NOT NULL,
    UNIQUE (issuer, subject)
);

CREATE INDEX oidc_identities_user ON oidc_identities (user_id);
