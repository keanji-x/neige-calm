-- #1838 S1 — structured tags on a track report (a report is 1:1 with its track).
-- Tags are metadata beside the report: never stored in its payload or CRDT body,
-- never parsed from its text. `ordinal` orders a track's tags by insertion; an add
-- takes the track's current maximum plus one. Rows follow their track: ON DELETE
-- CASCADE, and a fork copies them inside the fork's own transaction.
CREATE TABLE report_tags (
    track_id TEXT    NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    tag      TEXT    NOT NULL CHECK (length(tag) > 0),
    ordinal  INTEGER NOT NULL,
    PRIMARY KEY (track_id, tag),
    UNIQUE (track_id, ordinal)
);
