-- #2130 S1 — mail between the Tracks of one Area. A mail is unread until the
-- recipient's first `neige mail cat` stamps `read_at`. The Area is not stored:
-- same-Area is checked at send. Rows follow either Track: ON DELETE CASCADE.
CREATE TABLE mails (
    id            TEXT    PRIMARY KEY,
    from_track_id TEXT    NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    to_track_id   TEXT    NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    reply_to      TEXT    NULL REFERENCES mails(id) ON DELETE SET NULL,
    summary       TEXT    NOT NULL CHECK (length(summary) BETWEEN 1 AND 200),
    text          TEXT    NOT NULL CHECK (length(text) BETWEEN 1 AND 8000),
    hop           INTEGER NOT NULL CHECK (hop BETWEEN 1 AND 6),
    sent_at       INTEGER NOT NULL,
    read_at       INTEGER NULL,
    CHECK (from_track_id <> to_track_id)
);
CREATE INDEX idx_mails_to   ON mails(to_track_id, sent_at, id);
CREATE INDEX idx_mails_from ON mails(from_track_id, sent_at, id);
