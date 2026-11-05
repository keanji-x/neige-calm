-- A close obligation is independent of the live harness snapshot and survives its writes.
CREATE TABLE track_close_cleanups (
    worker_session_id TEXT PRIMARY KEY REFERENCES worker_sessions(id) ON DELETE CASCADE,
    card_id TEXT NOT NULL,
    track_id TEXT NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    requested_at_ms INTEGER NOT NULL
);
CREATE INDEX track_close_cleanups_card ON track_close_cleanups(card_id);
