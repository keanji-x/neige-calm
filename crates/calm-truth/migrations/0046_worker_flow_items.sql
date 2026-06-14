CREATE TABLE IF NOT EXISTS worker_flow_items (
  id                INTEGER PRIMARY KEY AUTOINCREMENT,
  card_id           TEXT REFERENCES cards(id) ON DELETE SET NULL,  -- NULLABLE on purpose: row must survive card delete (#695)
  runtime_id        TEXT,
  wave_id           TEXT,
  worker_session_id TEXT,                                          -- nullable; PR5 migration 0049 adds the worker_sessions FK with ON DELETE SET NULL
  kind              TEXT NOT NULL,                                  -- WorkerFlowItem discriminant
  payload           TEXT NOT NULL,                                  -- JSON of the WorkerFlowItem (+ FlowEnvelope/provider_extra/raw_ref)
  created_at_ms     INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_worker_flow_items_card
  ON worker_flow_items(card_id, id);

CREATE INDEX IF NOT EXISTS idx_worker_flow_items_session
  ON worker_flow_items(worker_session_id, id);
