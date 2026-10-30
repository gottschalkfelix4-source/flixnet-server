-- Scene search (Szenensuche): the spoken text of a file in time-stamped windows of
-- roughly 30–60 s, from its text subtitles or, opt-in per library, from a Whisper
-- transcription. One `transcripts` row per indexed source of a file; `signature`
-- (size and mtime of the source) tells when it has to be indexed again.
CREATE TABLE transcripts (
    id           INTEGER PRIMARY KEY,
    file_id      INTEGER NOT NULL REFERENCES media_files (id) ON DELETE CASCADE,
    source       TEXT    NOT NULL,          -- subtitle | whisper
    stream_index INTEGER NOT NULL,          -- subtitle stream (sidecars from 1000), -1 for whisper
    language     TEXT,                      -- ISO 639-2 as the stream states it
    signature    TEXT    NOT NULL,
    chunk_count  INTEGER NOT NULL,
    created_at   INTEGER NOT NULL,
    UNIQUE (file_id, source, stream_index)
);

CREATE TABLE transcript_chunks (
    id            INTEGER PRIMARY KEY,
    transcript_id INTEGER NOT NULL REFERENCES transcripts (id) ON DELETE CASCADE,
    file_id       INTEGER NOT NULL REFERENCES media_files (id) ON DELETE CASCADE,
    start_ms      INTEGER NOT NULL,
    end_ms        INTEGER NOT NULL,
    text          TEXT    NOT NULL
);
CREATE INDEX transcript_chunks_file ON transcript_chunks (file_id, start_ms);
CREATE INDEX transcript_chunks_transcript ON transcript_chunks (transcript_id);

CREATE VIRTUAL TABLE transcript_chunks_fts USING fts5(
    text,
    content = 'transcript_chunks', content_rowid = 'id',
    tokenize = 'unicode61 remove_diacritics 2'
);

CREATE TRIGGER transcript_chunks_fts_insert AFTER INSERT ON transcript_chunks BEGIN
    INSERT INTO transcript_chunks_fts (rowid, text) VALUES (new.id, new.text);
END;

CREATE TRIGGER transcript_chunks_fts_delete AFTER DELETE ON transcript_chunks BEGIN
    INSERT INTO transcript_chunks_fts (transcript_chunks_fts, rowid, text)
    VALUES ('delete', old.id, old.text);
END;

-- Whisper transcription is opt-in per library (it is slow and only runs for files
-- without text subtitles).
ALTER TABLE libraries ADD COLUMN transcribe_audio INTEGER NOT NULL DEFAULT 0;

-- Theme collections (collections.kind = 'theme', provider 'llm') need no new columns:
-- they are proposed by the LLM over the catalogue and stored like automatic ones.
