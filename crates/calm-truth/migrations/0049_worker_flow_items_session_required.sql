-- Pivot worker_flow_items.worker_session_id from "agent session string" to
-- a real FK against worker_sessions(id). After #679 PR3b-i dual-write,
-- worker_sessions(id) == runtimes(id), so the source-side change in
-- session_from_runtime now writes the correct value.
--
-- This migration recreates the table to add the worker_sessions FK. Existing
-- pre-PR5 rows stored the agent session string in worker_session_id; translate
-- that value through runtimes.thread_id/session_id and preserve orphaned rows.

PRAGMA foreign_keys = OFF;

CREATE TABLE worker_flow_items_new (
  id                INTEGER PRIMARY KEY AUTOINCREMENT,
  card_id           TEXT REFERENCES cards(id) ON DELETE SET NULL,
  runtime_id        TEXT,
  wave_id           TEXT,
  worker_session_id TEXT
                    REFERENCES worker_sessions(id) ON DELETE SET NULL,  -- nullable on purpose: row must survive session+card delete (#695)
  kind              TEXT NOT NULL,
  payload           TEXT NOT NULL,
  created_at_ms     INTEGER NOT NULL
);

-- Defensive copy of any existing rows. If a row still carries the old agent
-- session string, resolve it to the runtime id used by worker_sessions(id).
-- Rows with no runtime/session match keep their payload and receive NULL FKs.
INSERT INTO worker_flow_items_new
    (id, card_id, runtime_id, wave_id, worker_session_id, kind, payload, created_at_ms)
SELECT w.id,
       w.card_id,
       COALESCE(w.runtime_id, r.id),
       w.wave_id,
       CASE
         WHEN EXISTS (SELECT 1 FROM worker_sessions ws2 WHERE ws2.id = COALESCE(w.runtime_id, r.id))
           THEN COALESCE(w.runtime_id, r.id)
         ELSE NULL
       END AS worker_session_id,
       w.kind,
       w.payload,
       w.created_at_ms
FROM worker_flow_items w
LEFT JOIN runtimes r
       ON r.thread_id = w.worker_session_id
       OR r.session_id = w.worker_session_id;

DROP TABLE worker_flow_items;
ALTER TABLE worker_flow_items_new RENAME TO worker_flow_items;

CREATE INDEX idx_worker_flow_items_card
  ON worker_flow_items(card_id, id);

CREATE INDEX idx_worker_flow_items_session
  ON worker_flow_items(worker_session_id, id);

CREATE INDEX idx_worker_flow_items_runtime
  ON worker_flow_items(runtime_id, id);

PRAGMA foreign_keys = ON;
