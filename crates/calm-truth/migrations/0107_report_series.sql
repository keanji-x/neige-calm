-- #1628 S2 — resolved chart.series data, one row per (block, request fingerprint).
-- `pending` is not a row state: no row means pending. A pinned row is immutable
-- at the DB layer (the writer's ON CONFLICT ... WHERE pinned = 0 refuses it).
CREATE TABLE report_series (
    track_id     TEXT    NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    block_id     TEXT    NOT NULL,
    request_hash TEXT    NOT NULL,
    status       TEXT    NOT NULL,
    reason       TEXT,
    as_of        TEXT    NOT NULL,
    resolved_at  INTEGER NOT NULL,
    pinned       INTEGER NOT NULL DEFAULT 0,
    summary      TEXT,
    data         TEXT,
    PRIMARY KEY (track_id, block_id, request_hash)
);
