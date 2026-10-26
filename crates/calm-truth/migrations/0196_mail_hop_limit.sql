-- Raise only the hop limit. The replacement references itself so dropping the old
-- table with foreign_keys ON cannot clear the copied reply chain.
CREATE TABLE mails_next (
    id            TEXT    PRIMARY KEY,
    from_track_id TEXT    NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    to_track_id   TEXT    NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    reply_to      TEXT    NULL REFERENCES mails_next(id) ON DELETE SET NULL,
    summary       TEXT    NOT NULL CHECK (length(summary) BETWEEN 1 AND 200),
    text          TEXT    NOT NULL CHECK (length(text) BETWEEN 1 AND 8000),
    hop           INTEGER NOT NULL CHECK (hop BETWEEN 1 AND 20),
    sent_at       INTEGER NOT NULL,
    read_at       INTEGER NULL,
    CHECK (from_track_id <> to_track_id)
);
INSERT INTO mails_next (id, from_track_id, to_track_id, reply_to, summary, text, hop, sent_at, read_at)
SELECT id, from_track_id, to_track_id, reply_to, summary, text, hop, sent_at, read_at FROM mails;
DROP TABLE mails;
ALTER TABLE mails_next RENAME TO mails;
CREATE INDEX idx_mails_to   ON mails(to_track_id, sent_at, id);
CREATE INDEX idx_mails_from ON mails(from_track_id, sent_at, id);
