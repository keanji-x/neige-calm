-- Pivot worker_flow_items.worker_session_id from "agent session string" to
-- a real FK against worker_sessions(id). After #679 PR3b-i dual-write,
-- worker_sessions(id) == runtimes(id), so the source-side change in
-- session_from_runtime now writes the correct value.
--
-- This migration recreates the table to add NOT NULL + FK. The table is
-- empty in any current deployment (no worker_flow_items rows existed before
-- the orchestrator restarted with the latest code), so no backfill is needed.

PRAGMA foreign_keys = OFF;

CREATE TABLE worker_flow_items_new (
  id                INTEGER PRIMARY KEY AUTOINCREMENT,
  card_id           TEXT REFERENCES cards(id) ON DELETE SET NULL,
  runtime_id        TEXT,
  wave_id           TEXT,
  worker_session_id TEXT NOT NULL
                    REFERENCES worker_sessions(id) ON DELETE CASCADE,
  kind              TEXT NOT NULL,
  payload           TEXT NOT NULL,
  created_at_ms     INTEGER NOT NULL
);

-- Defensive copy of any existing rows. Skips rows whose worker_session_id
-- has no matching worker_sessions row (those are leftover from the
-- now-pivoted semantic; pre-PR5 only the dev box has them, and a hard
-- truncate is the explicit user-acknowledged outcome of the restart).
INSERT INTO worker_flow_items_new
    (id, card_id, runtime_id, wave_id, worker_session_id, kind, payload, created_at_ms)
SELECT w.id, w.card_id, w.runtime_id, w.wave_id, w.worker_session_id, w.kind, w.payload, w.created_at_ms
FROM worker_flow_items w
WHERE w.worker_session_id IS NOT NULL
  AND EXISTS (SELECT 1 FROM worker_sessions ws WHERE ws.id = w.worker_session_id);

DROP TABLE worker_flow_items;
ALTER TABLE worker_flow_items_new RENAME TO worker_flow_items;

CREATE INDEX idx_worker_flow_items_card
  ON worker_flow_items(card_id, id);

CREATE INDEX idx_worker_flow_items_session
  ON worker_flow_items(worker_session_id, id);

CREATE INDEX idx_worker_flow_items_runtime
  ON worker_flow_items(runtime_id, id);

PRAGMA foreign_keys = ON;
