-- #1669 S1 — captured source texts, one row per capture in a track.
-- `body` and the metadata are immutable once written; `quotes` (a JSON
-- array of {id, text, start, end}) only ever grows. Rows follow their
-- track: ON DELETE CASCADE, and a fork copies them verbatim inside the
-- fork's own transaction (source ids and anchors are preserved).
CREATE TABLE report_sources (
    track_id     TEXT    NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    source_id    TEXT    NOT NULL,
    provenance   TEXT    NOT NULL,
    origin       TEXT    NOT NULL,
    title        TEXT    NOT NULL,
    published_at TEXT,
    body         TEXT    NOT NULL,
    body_sha256  TEXT    NOT NULL,
    captured_at  INTEGER NOT NULL,
    quotes       TEXT    NOT NULL,
    PRIMARY KEY (track_id, source_id)
);
