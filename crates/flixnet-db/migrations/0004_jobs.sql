-- Persistent background job queue. Workers claim jobs atomically per pool, ordered
-- by priority; a heartbeat lets a restarted server requeue jobs that were running.
CREATE TABLE jobs (
    id           INTEGER PRIMARY KEY,
    kind         TEXT    NOT NULL,
    pool         TEXT    NOT NULL,
    payload      TEXT    NOT NULL,              -- JSON
    priority     INTEGER NOT NULL DEFAULT 0,
    state        TEXT    NOT NULL DEFAULT 'queued', -- queued | running | done | failed | cancelled
    attempts     INTEGER NOT NULL DEFAULT 0,
    max_attempts INTEGER NOT NULL DEFAULT 3,
    run_after    INTEGER NOT NULL,
    dedupe_key   TEXT,
    progress     REAL,
    error        TEXT,
    created_at   INTEGER NOT NULL,
    started_at   INTEGER,
    finished_at  INTEGER,
    heartbeat_at INTEGER
);

CREATE INDEX jobs_claim ON jobs (pool, state, priority DESC, id);
CREATE INDEX jobs_state ON jobs (state, finished_at);
-- At most one pending/running job per dedupe key (e.g. "probe:file:42").
CREATE UNIQUE INDEX jobs_dedupe ON jobs (dedupe_key)
    WHERE dedupe_key IS NOT NULL AND state IN ('queued', 'running');

-- Recurring maintenance (library rescans, EPG refresh, cleanup).
CREATE TABLE scheduled_tasks (
    name        TEXT    PRIMARY KEY NOT NULL,
    interval_ms INTEGER NOT NULL,
    enabled     INTEGER NOT NULL DEFAULT 1,
    last_run    INTEGER,
    next_run    INTEGER NOT NULL
);
