-- #1722 S1b — a stable database identity, a completion-time column, and the
-- transcript index the activity projector and the conversation list read.
--
-- `database_identity` is a one-row table. The row is minted on the first
-- open after this migration (`INSERT OR IGNORE ... VALUES (1, <uuid>, <now>)`,
-- then `SELECT id`) and never rewritten: a later boot's `INSERT OR IGNORE`
-- collides on the primary key and is ignored, so every boot reads the first
-- id back. `CHECK (singleton = 1)` is what makes it one row — without it a
-- second key would insert cleanly and "the" identity would be whichever row
-- a reader happened to pick. Distinct from `dbInstanceId` (fresh per process)
-- on purpose: this one names the database, that one names the boot.
--
-- Not `settings` (user-writable through `PUT /api/settings`, empty string
-- deletes) and not `retention_meta` (integer pruning bookkeeping).
CREATE TABLE database_identity (
    singleton    INTEGER PRIMARY KEY CHECK (singleton = 1),
    id           TEXT    NOT NULL,
    minted_at_ms INTEGER NOT NULL
);

-- The liveness feeder stamps the last non-interrupted `turn/completed` here
-- (a later slice writes it; this migration only adds the column). NULL means
-- "no completed turn on this session yet".
ALTER TABLE worker_sessions ADD COLUMN last_turn_completed_ms INTEGER NULL;

-- The transcript table had one index, `(card_id, id)`. The projector's
-- per-track evidence reads and the two `last_turn_completed` subqueries all
-- filter on `card_id` and `method` and take MAX(created_at_ms); this composite
-- index turns each into one index range per card instead of a scan over the
-- card's whole history.
CREATE INDEX idx_transcript_card_method_created_at
    ON harness_items(card_id, method, created_at_ms);
