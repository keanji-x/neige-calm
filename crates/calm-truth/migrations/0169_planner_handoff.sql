-- Pending activation is inert; released migrations remain byte-frozen.
PRAGMA defer_foreign_keys = ON;
CREATE TEMP TABLE assistant_managed_sessions_saved AS SELECT * FROM acp_managed_sessions;
CREATE TEMP TABLE assistant_submissions_saved AS SELECT * FROM acp_submissions;
DROP TABLE acp_submissions;
DROP TABLE acp_managed_sessions;

CREATE TEMP TABLE opencode_cards_session_refs AS SELECT id, session_id FROM cards WHERE session_id IS NOT NULL;
CREATE TEMP TABLE opencode_tracks_session_refs AS SELECT id, root_session_id FROM tracks WHERE root_session_id IS NOT NULL;
CREATE TEMP TABLE opencode_flow_session_refs AS SELECT id, worker_session_id FROM worker_flow_items WHERE worker_session_id IS NOT NULL;
CREATE TABLE worker_sessions_new (
  id TEXT PRIMARY KEY,
  track_id TEXT NOT NULL REFERENCES "tracks"(id),
  provider TEXT NOT NULL CHECK (length(provider) > 0),
  mode TEXT NOT NULL CHECK (mode IN ('ephemeral','resumable')),
  contract TEXT NOT NULL CHECK (contract IN ('planner','assistant','executor','validator')),
  parent_session_id TEXT NULL REFERENCES worker_sessions_new(id),
  requester_session_id TEXT NULL REFERENCES worker_sessions_new(id),
  state TEXT NOT NULL CHECK (state IN (
    'pending_activation',
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
  CHECK (state != 'pending_activation' OR (contract='planner' AND mode='resumable'
    AND mcp_token_hash IS NULL AND thread_id IS NULL AND agent_session_id IS NULL
    AND active_turn_id IS NULL AND terminal_run_id IS NULL)),
  CHECK (provider != 'opencode' OR (contract IN ('planner','assistant') AND mode = 'resumable')),
  CHECK (contract != 'assistant' OR (provider != 'terminal' AND mode = 'resumable'))
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

-- #2343: write once before prompt dispatch. Unknown submissions fence their session.
-- Only this backend can register managed ownership; provider identity alone is insufficient.
CREATE TABLE acp_managed_sessions (
  worker_session_id TEXT PRIMARY KEY REFERENCES worker_sessions(id) ON DELETE CASCADE,
  registration_digest TEXT NOT NULL
);
CREATE TABLE acp_submissions (
  worker_session_id TEXT NOT NULL REFERENCES worker_sessions(id) ON DELETE CASCADE,
  client_id TEXT NOT NULL,
  turn_id TEXT NOT NULL UNIQUE,
  thread_id TEXT NOT NULL,
  native_session_id TEXT NOT NULL,
  input_json TEXT NOT NULL CHECK(json_valid(input_json)),
  state TEXT NOT NULL CHECK(state IN ('sending','completed','unknown')),
  outcome_json TEXT CHECK(outcome_json IS NULL OR json_valid(outcome_json)),
  created_at_ms INTEGER NOT NULL,
  PRIMARY KEY(worker_session_id, client_id),
  CHECK((state = 'completed') = (outcome_json IS NOT NULL))
);
CREATE UNIQUE INDEX acp_one_unresolved_submission ON acp_submissions(worker_session_id)
  WHERE state IN ('sending','unknown');

INSERT INTO acp_managed_sessions SELECT * FROM assistant_managed_sessions_saved;
INSERT INTO acp_submissions SELECT * FROM assistant_submissions_saved;
DROP TABLE assistant_managed_sessions_saved;
DROP TABLE assistant_submissions_saved;

-- Retired Planner identity is inert, never an Assistant or a ReportCard.
DROP TRIGGER cards_role_validate_insert;
DROP TRIGGER cards_role_validate_update;
CREATE TRIGGER cards_role_validate_insert BEFORE INSERT ON cards
WHEN NEW.role NOT IN ('worker', 'planner', 'reportcard', 'assistant', 'planner_history')
BEGIN SELECT RAISE(ABORT, 'invalid cards.role'); END;
CREATE TRIGGER cards_role_validate_update BEFORE UPDATE OF role ON cards
WHEN NEW.role NOT IN ('worker', 'planner', 'reportcard', 'assistant', 'planner_history')
BEGIN SELECT RAISE(ABORT, 'invalid cards.role'); END;

-- Generic start/recovery cannot replace a pending identity or activate it.
CREATE TRIGGER pending_activation_no_replacement BEFORE INSERT ON worker_sessions
WHEN EXISTS (SELECT 1 FROM cards c JOIN worker_sessions ws ON ws.id=c.session_id
             WHERE c.id=NEW.card_id AND ws.state='pending_activation')
BEGIN SELECT RAISE(ABORT, 'Planner awaits explicit activation'); END;
CREATE TRIGGER pending_activation_no_generic_start BEFORE UPDATE ON worker_sessions
WHEN OLD.state='pending_activation' AND
 (NEW.state != OLD.state OR NEW.thread_id IS NOT NULL OR NEW.agent_session_id IS NOT NULL
  OR NEW.mcp_token_hash IS NOT NULL OR NEW.active_turn_id IS NOT NULL OR NEW.terminal_run_id IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'Planner awaits explicit activation'); END;

-- Final persisted fences cover requests admitted before the role cutover.
-- Content stays immutable; FK cleanup does not mint authority. Parent deletion keeps its contract.
CREATE TRIGGER planner_history_card_update BEFORE UPDATE OF role,kind,sort,payload,title,deletable ON cards
WHEN OLD.role = 'planner_history'
BEGIN SELECT RAISE(ABORT, 'Planner history is read-only'); END;
CREATE TRIGGER planner_history_card_delete BEFORE DELETE ON cards
WHEN OLD.role = 'planner_history'
 AND EXISTS (SELECT 1 FROM tracks WHERE id=OLD.track_id)
BEGIN SELECT RAISE(ABORT, 'Planner history is read-only'); END;
CREATE TRIGGER planner_history_session_insert BEFORE INSERT ON worker_sessions
WHEN EXISTS (SELECT 1 FROM cards WHERE id=NEW.card_id AND role='planner_history')
BEGIN SELECT RAISE(ABORT, 'Planner history has no startup authority'); END;
CREATE TRIGGER planner_history_session_reactivate BEFORE UPDATE OF state,mcp_token_hash ON worker_sessions
WHEN EXISTS (SELECT 1 FROM cards WHERE id=NEW.card_id AND role='planner_history')
 AND (NEW.state IN ('starting','running','idle','turn_pending') OR NEW.mcp_token_hash IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'Planner history has no session authority'); END;
CREATE TRIGGER planner_history_transcript_insert BEFORE INSERT ON harness_items
WHEN EXISTS (SELECT 1 FROM cards WHERE id=NEW.card_id AND role='planner_history')
BEGIN SELECT RAISE(ABORT, 'Planner history transcript is read-only'); END;
CREATE TRIGGER planner_history_transcript_update BEFORE UPDATE ON harness_items
WHEN EXISTS (SELECT 1 FROM cards WHERE id=OLD.card_id AND role='planner_history')
BEGIN SELECT RAISE(ABORT, 'Planner history transcript is read-only'); END;
CREATE TRIGGER planner_history_transcript_delete BEFORE DELETE ON harness_items
WHEN EXISTS (SELECT 1 FROM cards WHERE id=OLD.card_id AND role='planner_history')
BEGIN SELECT RAISE(ABORT, 'Planner history transcript is read-only'); END;

CREATE TRIGGER pending_activation_current_link BEFORE UPDATE OF session_id ON cards
WHEN NEW.session_id IS NOT OLD.session_id
 AND EXISTS (SELECT 1 FROM worker_sessions WHERE id=OLD.session_id AND state='pending_activation')
BEGIN SELECT RAISE(ABORT, 'Planner awaits explicit activation'); END;
