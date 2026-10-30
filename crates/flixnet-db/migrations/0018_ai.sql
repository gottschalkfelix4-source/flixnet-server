-- Local AI: item embeddings, onboarding picks and TMDB keywords.

-- One row per embedding model ever used ("multilingual-e5-base-int8",
-- "openai:text-embedding-3-small", "stub-v1"). Vectors of different models never mix.
CREATE TABLE embedding_models (
    id         INTEGER PRIMARY KEY,
    model_key  TEXT    NOT NULL UNIQUE,
    dimensions INTEGER NOT NULL,
    created_at INTEGER NOT NULL
);

-- Item vectors (movies and series): little-endian f32 × dimensions, L2-normalized.
-- text_hash is the SHA-256 of the embedded text, so changed metadata gets a new vector
-- and unchanged items are skipped. Searched by brute-force cosine in Rust (see
-- flixnet-ai `VectorIndex`), which stays fast up to ~100k items.
CREATE TABLE item_embeddings (
    model_id   INTEGER NOT NULL REFERENCES embedding_models (id) ON DELETE CASCADE,
    item_id    INTEGER NOT NULL REFERENCES items (id) ON DELETE CASCADE,
    text_hash  TEXT    NOT NULL,
    vector     BLOB    NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (model_id, item_id)
);
CREATE INDEX item_embeddings_item ON item_embeddings (item_id);

-- Onboarding: titles a profile without history said it likes ("Für dich").
-- Keyed by identity like user_item_data, so picks survive rescans.
CREATE TABLE taste_seeds (
    user_id      INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    identity_key TEXT    NOT NULL,
    created_at   INTEGER NOT NULL,
    PRIMARY KEY (user_id, identity_key)
);

-- Keywords from TMDB ("time travel", "dinosaur"); part of the embedding text.
CREATE TABLE item_keywords (
    item_id INTEGER NOT NULL REFERENCES items (id) ON DELETE CASCADE,
    keyword TEXT    NOT NULL,
    ord     INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (item_id, keyword)
);
