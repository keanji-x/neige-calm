CREATE TABLE worker_sessions (
  id TEXT PRIMARY KEY,
  wave_id TEXT NOT NULL REFERENCES waves(id),
  provider TEXT NOT NULL CHECK (provider IN ('codex','claude','terminal')),
  mode TEXT NOT NULL CHECK (mode IN ('ephemeral','resumable')),
  contract TEXT NOT NULL CHECK (contract IN ('planner','executor','validator')),
  parent_session_id TEXT NULL REFERENCES worker_sessions(id),
  requester_session_id TEXT NULL REFERENCES worker_sessions(id),
  state TEXT NOT NULL CHECK (state IN (
    'starting',
    'running',
    'idle',
    'turn_pending',
    'exited',
    'failed',
    'superseded'
  )),
  mcp_token_hash TEXT NULL,
  thread_id TEXT NULL,
  agent_session_id TEXT NULL,
  active_turn_id TEXT NULL,
  terminal_run_id TEXT NULL REFERENCES terminals(id) ON DELETE SET NULL,
  handle_state_json TEXT NULL,
  liveness TEXT NOT NULL DEFAULT 'unknown' CHECK (liveness IN (
    'alive',
    'idle',
    'exited',
    'unknown'
  )),
  liveness_probed_at_ms INTEGER NULL,
  exit_code INTEGER NULL,
  exit_interpretation TEXT NULL,
  spawn_op_id TEXT NULL REFERENCES operations(id),
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL,
  completed_at_ms INTEGER NULL
);

CREATE UNIQUE INDEX ws_token_idx ON worker_sessions(mcp_token_hash)
  WHERE mcp_token_hash IS NOT NULL;

CREATE INDEX ws_wave_idx ON worker_sessions(wave_id, created_at_ms, id);
CREATE INDEX ws_requester_idx ON worker_sessions(requester_session_id)
  WHERE requester_session_id IS NOT NULL;

ALTER TABLE cards ADD COLUMN session_id TEXT NULL
  REFERENCES worker_sessions(id) ON DELETE SET NULL;

ALTER TABLE waves ADD COLUMN root_session_id TEXT NULL
  REFERENCES worker_sessions(id);
