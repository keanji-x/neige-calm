-- #2343: register OpenCode managed Planner identity without rewriting released migrations.
-- Preserve inbound references across SQLite's implicit DELETE during DROP TABLE.
-- SQLx runs this file in one transaction; disabling foreign_keys there has no effect.
PRAGMA defer_foreign_keys = ON;
CREATE TEMP TABLE opencode_cards_session_refs AS SELECT id, session_id FROM cards WHERE session_id IS NOT NULL;
CREATE TEMP TABLE opencode_tracks_session_refs AS SELECT id, root_session_id FROM tracks WHERE root_session_id IS NOT NULL;
CREATE TEMP TABLE opencode_flow_session_refs AS SELECT id, worker_session_id FROM worker_flow_items WHERE worker_session_id IS NOT NULL;
CREATE TABLE worker_sessions_new (
  id TEXT PRIMARY KEY,
  track_id TEXT NOT NULL REFERENCES "tracks"(id),
  provider TEXT NOT NULL CHECK (provider IN ('codex','claude','opencode','terminal')),
  mode TEXT NOT NULL CHECK (mode IN ('ephemeral','resumable')),
  contract TEXT NOT NULL CHECK (contract IN ('planner','executor','validator')),
  parent_session_id TEXT NULL REFERENCES worker_sessions_new(id),
  requester_session_id TEXT NULL REFERENCES worker_sessions_new(id),
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
, last_activity_ms   INTEGER, last_thread_status TEXT, card_id TEXT, queue_harvested_at_ms INTEGER, last_turn_completed_ms INTEGER NULL,
  CHECK (provider != 'opencode' OR (contract = 'planner' AND mode = 'resumable'))
);
INSERT INTO worker_sessions_new (id,track_id,provider,mode,contract,parent_session_id,requester_session_id,state,mcp_token_hash,thread_id,agent_session_id,active_turn_id,terminal_run_id,handle_state_json,liveness,liveness_probed_at_ms,exit_code,exit_interpretation,spawn_op_id,created_at_ms,updated_at_ms,completed_at_ms,last_activity_ms,last_thread_status,card_id,queue_harvested_at_ms,last_turn_completed_ms) SELECT id,track_id,provider,mode,contract,parent_session_id,requester_session_id,state,mcp_token_hash,thread_id,agent_session_id,active_turn_id,terminal_run_id,handle_state_json,liveness,liveness_probed_at_ms,exit_code,exit_interpretation,spawn_op_id,created_at_ms,updated_at_ms,completed_at_ms,last_activity_ms,last_thread_status,card_id,queue_harvested_at_ms,last_turn_completed_ms FROM worker_sessions;
-- Clear references before DROP so deferred NO ACTION counters do not survive the rebuild.
UPDATE cards SET session_id = NULL WHERE session_id IS NOT NULL;
UPDATE tracks SET root_session_id = NULL WHERE root_session_id IS NOT NULL;
UPDATE worker_flow_items SET worker_session_id = NULL WHERE worker_session_id IS NOT NULL;
UPDATE worker_sessions SET parent_session_id = NULL, requester_session_id = NULL;
DROP TABLE worker_sessions;
ALTER TABLE worker_sessions_new RENAME TO worker_sessions;
UPDATE cards SET session_id = (SELECT session_id FROM opencode_cards_session_refs saved WHERE saved.id = cards.id)
 WHERE id IN (SELECT id FROM opencode_cards_session_refs);
UPDATE tracks SET root_session_id = (SELECT root_session_id FROM opencode_tracks_session_refs saved WHERE saved.id = tracks.id)
 WHERE id IN (SELECT id FROM opencode_tracks_session_refs);
UPDATE worker_flow_items SET worker_session_id = (SELECT worker_session_id FROM opencode_flow_session_refs saved WHERE saved.id = worker_flow_items.id)
 WHERE id IN (SELECT id FROM opencode_flow_session_refs);
DROP TABLE opencode_cards_session_refs;
DROP TABLE opencode_tracks_session_refs;
DROP TABLE opencode_flow_session_refs;
CREATE UNIQUE INDEX ws_token_idx ON worker_sessions(mcp_token_hash)
  WHERE mcp_token_hash IS NOT NULL;
CREATE INDEX ws_requester_idx ON worker_sessions(requester_session_id)
  WHERE requester_session_id IS NOT NULL;
CREATE INDEX ws_provider_thread_idx
    ON worker_sessions(provider, thread_id) WHERE thread_id IS NOT NULL;
CREATE INDEX ws_provider_session_idx
    ON worker_sessions(provider, agent_session_id) WHERE agent_session_id IS NOT NULL;
CREATE INDEX ws_terminal_run_idx
    ON worker_sessions(terminal_run_id) WHERE terminal_run_id IS NOT NULL;
CREATE INDEX ws_card_id_idx
    ON worker_sessions(card_id) WHERE card_id IS NOT NULL;
CREATE UNIQUE INDEX ws_one_active_per_card
   ON worker_sessions(card_id)
WHERE state IN ('starting','running','idle','turn_pending');
CREATE INDEX ws_track_idx ON worker_sessions(track_id, created_at_ms, id);
CREATE INDEX idx_worker_sessions_card_state
  ON worker_sessions(card_id, state);
