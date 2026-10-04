-- #2043 — a planner send carries an `Idempotency-Key`. Its binding commits in the same
-- transaction as the harness snapshot that durably holds the message, so a retry after a
-- lost answer replays this row instead of queueing the message twice.
-- `entry_id` is NULL exactly when the message folded into a queue entry that has no id.
-- A binding is a dedup wall, so it is kept as long as its card (docs/design-1428-idempotency-retention.md
-- §3.2); deleting the card takes its rows along.
CREATE TABLE planner_input_idempotency (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    card_id           TEXT    NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
    idempotency_key   TEXT    NOT NULL,
    payload_hash      TEXT    NOT NULL,
    worker_session_id TEXT    NOT NULL,
    entry_id          TEXT,
    created_at_ms     INTEGER NOT NULL,
    UNIQUE (card_id, idempotency_key)
);
