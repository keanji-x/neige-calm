-- Forward migration: preserve frozen task vocabulary and SQL contracts while widening worker selection.
PRAGMA defer_foreign_keys = ON;
CREATE TEMP TABLE task_worker_fk_before AS SELECT * FROM pragma_foreign_key_check;
DROP VIEW current_tasks;
DROP TRIGGER task_attempt_recovery_predecessor;
-- foreign_keys stays ON in the migrator transaction. Stage and remove the
-- referencing table before DROP TABLE tasks so its CASCADE cannot erase data.
CREATE TEMP TABLE task_ref_index_saved AS SELECT * FROM task_ref_index;
DROP TABLE task_ref_index;
DROP TRIGGER task_ref_index_cleanup_terminal;
CREATE TABLE tasks_next (
  id              TEXT PRIMARY KEY,
  track_id         TEXT NOT NULL,
  key             TEXT NOT NULL,
  kind            TEXT NOT NULL CHECK (kind IN ('codex', 'terminal', 'claude', 'compact', 'agent')),
  goal            TEXT NOT NULL,
  context_json    TEXT NOT NULL CHECK (json_valid(context_json)),
  acceptance_criteria TEXT NULL,
  cwd             TEXT NULL,
  depends_on_json TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(depends_on_json)),
  priority        INTEGER NOT NULL DEFAULT 0,
  gate_json       TEXT NULL CHECK (gate_json IS NULL OR json_valid(gate_json)),
  status          TEXT NOT NULL DEFAULT 'pending' CHECK (status IN (
    'pending', 'dispatched', 'running', 'verifying', 'done', 'failed', 'canceled'
  )),
  status_detail   TEXT NULL,
  worker_card_id  TEXT NULL,
  gate_result_json TEXT NULL CHECK (gate_result_json IS NULL OR json_valid(gate_result_json)),
  gate_attempt    INTEGER NOT NULL DEFAULT 0,
  gate_pid        INTEGER NULL,
  gate_pid_starttime INTEGER NULL,
  gate_pid_boot_id TEXT NULL,
  running_deadline_ms INTEGER NULL,
  created_at_ms   INTEGER NOT NULL,
  updated_at_ms   INTEGER NOT NULL,
  finished_at_ms  INTEGER NULL, claim_context_json TEXT NULL
  CHECK (claim_context_json IS NULL OR json_valid(claim_context_json)), context_stale_at_ms INTEGER NULL, context_closure_truncated INTEGER NOT NULL DEFAULT 0
  CHECK (context_closure_truncated IN (0, 1)), declared_by TEXT NOT NULL DEFAULT 'spec', decl_ready INTEGER NOT NULL DEFAULT 0
  CHECK (decl_ready IN (0, 1)), decl_released_by_user INTEGER NOT NULL DEFAULT 0
  CHECK (decl_released_by_user IN (0, 1)), context_verify_failures INTEGER NOT NULL DEFAULT 0, spawn TEXT NOT NULL DEFAULT 'in-wave', child_track_id TEXT NULL, access TEXT NOT NULL DEFAULT 'read_write'
  CHECK (access IN ('read_only','read_write')), head TEXT NULL
  CHECK (head IS NULL OR (length(head) = 40 AND head NOT GLOB '*[^0-9a-f]*')), base TEXT NULL
  CHECK (base IS NULL OR (length(base) = 40 AND base NOT GLOB '*[^0-9a-f]*')), start TEXT NOT NULL DEFAULT 'checkout'
  CHECK (start IN ('checkout','upstream')), running_started_at_ms INTEGER, result_contract TEXT, child_track_json TEXT,
  provider TEXT NULL,
  model TEXT NULL,
  reasoning_effort TEXT NULL,
  FOREIGN KEY (id, track_id, key) REFERENCES task_attempt_allocations(attempt_id, track_id, key)
);
INSERT INTO tasks_next SELECT *, NULL, NULL, NULL FROM tasks;
DROP TABLE tasks;
ALTER TABLE tasks_next RENAME TO tasks;
CREATE INDEX tasks_track_status_idx ON tasks(track_id,status,priority DESC,created_at_ms);
CREATE INDEX idx_tasks_liveness_deadlines
  ON tasks (running_deadline_ms)
  WHERE status = 'running';
CREATE INDEX idx_tasks_child_track_id ON tasks(child_track_id) WHERE child_track_id IS NOT NULL;
CREATE INDEX task_attempt_history_idx ON tasks(track_id,key);
CREATE TABLE task_ref_index (
  task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
  dst_track_id TEXT NOT NULL,
  block_id TEXT NOT NULL,
  PRIMARY KEY(task_id,dst_track_id,block_id)
);
CREATE INDEX task_ref_index_destination_idx ON task_ref_index(dst_track_id,block_id);
INSERT INTO task_ref_index SELECT * FROM task_ref_index_saved;
DROP TABLE task_ref_index_saved;
CREATE TRIGGER task_ref_index_cleanup_terminal AFTER UPDATE OF status ON tasks
WHEN NEW.status IN ('done','failed','canceled')
BEGIN DELETE FROM task_ref_index WHERE task_id=NEW.id; END;

CREATE INDEX idx_tasks_worker_card_id ON tasks(worker_card_id) WHERE worker_card_id IS NOT NULL;
CREATE VIEW current_tasks AS
SELECT t.* FROM tasks t JOIN current_task_attempt_allocations a ON a.attempt_id=t.id;
CREATE TRIGGER tasks_register_initial_attempt BEFORE INSERT ON tasks
WHEN NOT EXISTS(SELECT 1 FROM task_attempt_allocations WHERE attempt_id=NEW.id)
BEGIN
  INSERT INTO task_attempt_allocations(attempt_id,track_id,key,generation,origin_json,created_at_ms)
  VALUES(NEW.id,NEW.track_id,NEW.key,1,'{"kind":"initial"}',NEW.created_at_ms);
END;
CREATE TRIGGER task_attempt_recovery_predecessor BEFORE INSERT ON task_attempt_allocations
WHEN json_extract(NEW.origin_json,'$.kind')='recovery'
BEGIN
  SELECT CASE WHEN NOT EXISTS(
    SELECT 1 FROM current_task_attempt_allocations a JOIN tasks t ON t.id=a.attempt_id
    WHERE a.attempt_id=json_extract(NEW.origin_json,'$.previous_attempt_id')
      AND a.track_id=NEW.track_id AND a.key=NEW.key AND a.generation=NEW.generation-1
      AND t.status='failed'
  ) THEN RAISE(ABORT,'recovery predecessor must be the current failed execution') END;
END;

CREATE TEMP TABLE task_worker_saved_acp_normal_joins AS SELECT * FROM acp_normal_joins;
CREATE TEMP TABLE task_worker_saved_acp_submissions AS SELECT * FROM acp_submissions;
CREATE TEMP TABLE task_worker_saved_acp_managed_sessions AS SELECT * FROM acp_managed_sessions;
CREATE TEMP TABLE task_worker_saved_worker_session_backends AS SELECT * FROM worker_session_backends;
CREATE TEMP TABLE task_worker_saved_report_read_ledger_blocks AS SELECT * FROM report_read_ledger_blocks;
CREATE TEMP TABLE task_worker_saved_report_read_ledger AS SELECT * FROM report_read_ledger;
CREATE TEMP TABLE task_worker_saved_conversation_abandonments AS SELECT * FROM conversation_abandonments;
CREATE TEMP TABLE task_worker_saved_claude_session_launches AS SELECT * FROM claude_session_launches;
DROP TABLE acp_normal_joins;
DROP TABLE acp_submissions;
DROP TABLE acp_managed_sessions;
DROP TABLE worker_session_backends;
DROP TABLE report_read_ledger_blocks;
DROP TABLE report_read_ledger;
DROP TABLE conversation_abandonments;
DROP TABLE claude_session_launches;
CREATE TEMP TABLE task_worker_refs_cards AS SELECT id,session_id FROM cards WHERE session_id IS NOT NULL;
CREATE TEMP TABLE task_worker_refs_tracks AS SELECT id,root_session_id FROM tracks WHERE root_session_id IS NOT NULL;
CREATE TEMP TABLE task_worker_refs_worker_flow_items AS SELECT id,worker_session_id FROM worker_flow_items WHERE worker_session_id IS NOT NULL;
CREATE TABLE worker_sessions_new (
  id TEXT PRIMARY KEY,
  track_id TEXT NOT NULL REFERENCES "tracks"(id),
  provider TEXT NOT NULL CHECK (length(provider) > 0),
  mode TEXT NOT NULL CHECK (mode IN ('ephemeral','resumable')),
  contract TEXT NOT NULL CHECK (contract IN ('planner','assistant','executor','validator')),
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
, last_activity_ms   INTEGER, last_thread_status TEXT, card_id TEXT, queue_harvested_at_ms INTEGER, last_turn_completed_ms INTEGER NULL, provider_execution_json TEXT, failure_epoch INTEGER NOT NULL DEFAULT 0
    CHECK (typeof(failure_epoch) = 'integer' AND failure_epoch >= 0),
  CHECK (provider != 'opencode' OR (contract IN ('planner','assistant','executor') AND mode = 'resumable')),
  CHECK (contract != 'assistant' OR (provider != 'terminal' AND mode = 'resumable'))
);
INSERT INTO worker_sessions_new SELECT * FROM worker_sessions;
UPDATE cards SET session_id=NULL WHERE session_id IS NOT NULL;
UPDATE tracks SET root_session_id=NULL WHERE root_session_id IS NOT NULL;
UPDATE worker_flow_items SET worker_session_id=NULL WHERE worker_session_id IS NOT NULL;
UPDATE worker_sessions SET parent_session_id=NULL, requester_session_id=NULL;
DROP TRIGGER system_assistant_entry_scope;
DROP TRIGGER system_assistant_receipt_scope;
DROP TRIGGER conversation_retry_relation_scope;
DROP TRIGGER conversation_submission_human_retirement_receipt;
DROP TRIGGER assistant_creation_fact_scope;
DROP TRIGGER conversation_human_track_creation_fact_scope;
DROP TABLE worker_sessions;
ALTER TABLE worker_sessions_new RENAME TO worker_sessions;
UPDATE cards SET session_id=(SELECT session_id FROM task_worker_refs_cards s WHERE s.id=cards.id) WHERE id IN (SELECT id FROM task_worker_refs_cards);
DROP TABLE task_worker_refs_cards;
UPDATE tracks SET root_session_id=(SELECT root_session_id FROM task_worker_refs_tracks s WHERE s.id=tracks.id) WHERE id IN (SELECT id FROM task_worker_refs_tracks);
DROP TABLE task_worker_refs_tracks;
UPDATE worker_flow_items SET worker_session_id=(SELECT worker_session_id FROM task_worker_refs_worker_flow_items s WHERE s.id=worker_flow_items.id) WHERE id IN (SELECT id FROM task_worker_refs_worker_flow_items);
DROP TABLE task_worker_refs_worker_flow_items;
CREATE TABLE claude_session_launches (
    worker_session_id TEXT PRIMARY KEY NOT NULL REFERENCES worker_sessions(id) ON DELETE CASCADE,
    card_id TEXT NOT NULL,
    track_id TEXT NOT NULL,
    thread_id TEXT NOT NULL,
    execution_id TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL
);
CREATE TABLE conversation_abandonments (
    card_id TEXT NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
    principal TEXT NOT NULL,
    request_key TEXT NOT NULL,
    request_json TEXT NOT NULL CHECK(json_valid(request_json)),
    old_worker TEXT NOT NULL REFERENCES worker_sessions(id) ON DELETE CASCADE,
    new_worker TEXT NOT NULL REFERENCES worker_sessions(id) ON DELETE CASCADE,
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY(card_id,principal,request_key),
    UNIQUE(old_worker)
);
CREATE TABLE report_read_ledger (
    worker_session_id TEXT NOT NULL REFERENCES worker_sessions(id) ON DELETE CASCADE,
    report_card_id TEXT NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
    doc_rev INTEGER NOT NULL CHECK (doc_rev >= 0),
    whole INTEGER NOT NULL CHECK (whole IN (0, 1)),
    sections_json TEXT NOT NULL CHECK (json_valid(sections_json)),
    PRIMARY KEY (worker_session_id, report_card_id)
);
CREATE TABLE report_read_ledger_blocks (
    worker_session_id TEXT NOT NULL,
    report_card_id TEXT NOT NULL,
    block_id TEXT NOT NULL,
    rev INTEGER NOT NULL CHECK (rev BETWEEN 0 AND 4294967295),
    history_ref TEXT NOT NULL CHECK (json_valid(history_ref)),
    PRIMARY KEY (worker_session_id, report_card_id, block_id),
    FOREIGN KEY (worker_session_id, report_card_id)
        REFERENCES report_read_ledger(worker_session_id, report_card_id) ON DELETE CASCADE
);
CREATE TABLE worker_session_backends (
    worker_session_id TEXT PRIMARY KEY NOT NULL REFERENCES worker_sessions(id) ON DELETE CASCADE,
    provider TEXT NOT NULL,
    backend_id TEXT NOT NULL,
    FOREIGN KEY (provider, backend_id) REFERENCES agent_backend_registry(provider, backend_id)
);
CREATE TABLE acp_managed_sessions (
  worker_session_id TEXT PRIMARY KEY REFERENCES worker_sessions(id) ON DELETE CASCADE,
  registration_digest TEXT NOT NULL
);
CREATE TABLE "acp_submissions" (
  worker_session_id TEXT NOT NULL REFERENCES worker_sessions(id) ON DELETE CASCADE,
  client_id TEXT NOT NULL,
  turn_id TEXT NOT NULL UNIQUE,
  thread_id TEXT NOT NULL,
  native_session_id TEXT NOT NULL,
  input_json TEXT NOT NULL CHECK(json_valid(input_json)),
  state TEXT NOT NULL CHECK(state IN ('sending','completed','unknown')),
  outcome_json TEXT CHECK(outcome_json IS NULL OR json_valid(outcome_json)),
  created_at_ms INTEGER NOT NULL, kept_cleanup_json TEXT, shared_restart_owner_json TEXT
  CHECK(shared_restart_owner_json IS NULL OR json_valid(shared_restart_owner_json)), restart_released_at_ms INTEGER
  CHECK(restart_released_at_ms IS NULL OR
    (shared_restart_owner_json IS NOT NULL AND state='unknown' AND kept_cleanup_json IS NULL
     AND json_extract(input_json,'$.version') IS 'canonical_v1')),
  PRIMARY KEY(worker_session_id, client_id),
  CHECK((state = 'completed') = (outcome_json IS NOT NULL))
) WITHOUT ROWID;
CREATE TABLE acp_normal_joins (
    worker_session_id TEXT NOT NULL REFERENCES worker_sessions(id) ON DELETE CASCADE,
    client_id TEXT NOT NULL,
    evidence_json TEXT NOT NULL CHECK(json_valid(evidence_json)),
    PRIMARY KEY(worker_session_id, client_id),
    FOREIGN KEY(worker_session_id,client_id) REFERENCES acp_submissions(worker_session_id,client_id) ON DELETE CASCADE
);
INSERT INTO claude_session_launches SELECT * FROM task_worker_saved_claude_session_launches;
DROP TABLE task_worker_saved_claude_session_launches;
INSERT INTO conversation_abandonments SELECT * FROM task_worker_saved_conversation_abandonments;
DROP TABLE task_worker_saved_conversation_abandonments;
INSERT INTO report_read_ledger SELECT * FROM task_worker_saved_report_read_ledger;
DROP TABLE task_worker_saved_report_read_ledger;
INSERT INTO report_read_ledger_blocks SELECT * FROM task_worker_saved_report_read_ledger_blocks;
DROP TABLE task_worker_saved_report_read_ledger_blocks;
INSERT INTO worker_session_backends SELECT * FROM task_worker_saved_worker_session_backends;
DROP TABLE task_worker_saved_worker_session_backends;
INSERT INTO acp_managed_sessions SELECT * FROM task_worker_saved_acp_managed_sessions;
DROP TABLE task_worker_saved_acp_managed_sessions;
INSERT INTO acp_submissions SELECT * FROM task_worker_saved_acp_submissions;
DROP TABLE task_worker_saved_acp_submissions;
INSERT INTO acp_normal_joins SELECT * FROM task_worker_saved_acp_normal_joins;
DROP TABLE task_worker_saved_acp_normal_joins;
CREATE TRIGGER acp_cleanup_immutable BEFORE UPDATE ON acp_submissions
WHEN OLD.kept_cleanup_json IS NOT NULL OR NEW.kept_cleanup_json IS NOT NULL
BEGIN
  SELECT CASE WHEN (OLD.kept_cleanup_json IS NOT NULL AND
      NEW.kept_cleanup_json IS NOT OLD.kept_cleanup_json)
    OR NEW.worker_session_id IS NOT OLD.worker_session_id
    OR NEW.client_id IS NOT OLD.client_id
    OR NEW.turn_id IS NOT OLD.turn_id
    OR NEW.thread_id IS NOT OLD.thread_id
    OR NEW.native_session_id IS NOT OLD.native_session_id
    OR NEW.input_json IS NOT OLD.input_json
    OR NEW.created_at_ms IS NOT OLD.created_at_ms
    OR NEW.state IS NOT OLD.state
    OR NEW.outcome_json IS NOT OLD.outcome_json
    THEN RAISE(ABORT, 'ACP released Unknown receipt is immutable') END;
END;
CREATE TRIGGER acp_cleanup_no_insert BEFORE INSERT ON acp_submissions
WHEN NEW.kept_cleanup_json IS NOT NULL
BEGIN
  SELECT RAISE(ABORT, 'ACP cleanup requires an existing Unknown receipt');
END;
CREATE TRIGGER acp_cleanup_no_insert_collision BEFORE INSERT ON acp_submissions
WHEN EXISTS(SELECT 1 FROM acp_submissions
  WHERE kept_cleanup_json IS NOT NULL AND (
    (worker_session_id = NEW.worker_session_id AND client_id = NEW.client_id)
    OR turn_id = NEW.turn_id))
BEGIN
  SELECT RAISE(ABORT, 'ACP released Unknown receipt is immutable');
END;
CREATE TRIGGER acp_cleanup_no_update_collision BEFORE UPDATE ON acp_submissions
WHEN EXISTS(SELECT 1 FROM acp_submissions
  WHERE kept_cleanup_json IS NOT NULL
    AND NOT (worker_session_id = OLD.worker_session_id AND client_id = OLD.client_id) AND (
    (worker_session_id = NEW.worker_session_id AND client_id = NEW.client_id)
    OR turn_id = NEW.turn_id))
BEGIN
  SELECT RAISE(ABORT, 'ACP released Unknown receipt is immutable');
END;
CREATE TRIGGER acp_cleanup_scope BEFORE UPDATE ON acp_submissions
WHEN NEW.kept_cleanup_json IS NOT NULL
BEGIN
  SELECT CASE WHEN NEW.state IS NOT 'unknown'
    OR json_extract(NEW.input_json, '$.version') IS NOT 'canonical_v1'
    OR NOT json_valid(NEW.kept_cleanup_json)
    THEN RAISE(ABORT, 'ACP cleanup requires canonical Unknown evidence') END;
  SELECT CASE WHEN json_type(NEW.kept_cleanup_json) IS NOT 'object'
    OR (SELECT count(*) FROM json_each(NEW.kept_cleanup_json)) != 12
    OR json_extract(NEW.kept_cleanup_json, '$.version') IS NOT 'v1'
    OR json_extract(NEW.kept_cleanup_json, '$.worker_session_id') IS NOT NEW.worker_session_id
    OR json_extract(NEW.kept_cleanup_json, '$.client_id') IS NOT NEW.client_id
    OR json_extract(NEW.kept_cleanup_json, '$.turn_id') IS NOT NEW.turn_id
    OR json_extract(NEW.kept_cleanup_json, '$.thread_id') IS NOT NEW.thread_id
    OR json_extract(NEW.kept_cleanup_json, '$.native_session_id') IS NOT NEW.native_session_id
    OR json_extract(NEW.kept_cleanup_json, '$.association') IS NOT json_extract(NEW.input_json, '$.association')
    OR json_type(NEW.kept_cleanup_json, '$.registration_digest') IS NOT 'text'
    OR length(json_extract(NEW.kept_cleanup_json, '$.registration_digest')) = 0
    OR json_type(NEW.kept_cleanup_json, '$.keep_key') IS NOT 'text'
    OR json_type(NEW.kept_cleanup_json, '$.keep_principal') IS NOT 'text'
    OR json_type(NEW.kept_cleanup_json, '$.keep_fingerprint') IS NOT 'text'
    OR json_extract(NEW.kept_cleanup_json, '$.observation') IS NOT 'stopped_revoked_normally_joined'
    THEN RAISE(ABORT, 'ACP cleanup evidence shape or association changed') END;
END;
CREATE UNIQUE INDEX acp_one_unresolved_submission ON acp_submissions(worker_session_id)
  WHERE state = 'sending' OR (state = 'unknown' AND kept_cleanup_json IS NULL AND restart_released_at_ms IS NULL);
CREATE TRIGGER acp_restart_release_collision BEFORE UPDATE OF worker_session_id,client_id,turn_id ON acp_submissions
WHEN EXISTS(
  SELECT 1 FROM acp_submissions WHERE restart_released_at_ms IS NOT NULL AND
    ((worker_session_id=NEW.worker_session_id AND client_id=NEW.client_id) OR turn_id=NEW.turn_id)
)
BEGIN
  SELECT RAISE(ABORT, 'ACP restart release requires its original receipt');
END;
CREATE TRIGGER acp_restart_release_insert BEFORE INSERT ON acp_submissions
WHEN NEW.restart_released_at_ms IS NOT NULL OR EXISTS(
  SELECT 1 FROM acp_submissions WHERE restart_released_at_ms IS NOT NULL AND
    ((worker_session_id=NEW.worker_session_id AND client_id=NEW.client_id) OR turn_id=NEW.turn_id)
)
BEGIN
  SELECT RAISE(ABORT, 'ACP restart release requires its original receipt');
END;
CREATE TRIGGER acp_restart_release_update BEFORE UPDATE ON acp_submissions
WHEN NEW.shared_restart_owner_json IS NOT OLD.shared_restart_owner_json
  OR OLD.restart_released_at_ms IS NOT NULL
BEGIN
  SELECT RAISE(ABORT, 'ACP restart provenance and released receipts are immutable');
END;
CREATE INDEX idx_worker_sessions_card_state
  ON worker_sessions(card_id, state);
CREATE INDEX report_read_ledger_report ON report_read_ledger(report_card_id);
CREATE TRIGGER worker_session_backend_thread BEFORE UPDATE OF thread_id, provider ON worker_sessions
WHEN (EXISTS (
    SELECT 1 FROM worker_session_backends b
    WHERE b.worker_session_id = NEW.id AND b.provider != NEW.provider
)) OR (NEW.provider = 'codex' AND NEW.thread_id IS NOT NULL AND EXISTS (
    SELECT 1 FROM worker_sessions other
    LEFT JOIN worker_session_backends ob ON ob.worker_session_id = other.id
    WHERE other.id != NEW.id AND other.provider = 'codex' AND other.thread_id = NEW.thread_id
    AND COALESCE(ob.backend_id, 'default') != COALESCE(
        (SELECT backend_id FROM worker_session_backends WHERE worker_session_id = NEW.id), 'default')
))
BEGIN SELECT RAISE(ABORT, 'codex thread cannot cross backend scopes'); END;
CREATE TRIGGER worker_session_backend_thread_insert BEFORE INSERT ON worker_sessions
WHEN NEW.provider = 'codex' AND NEW.thread_id IS NOT NULL AND EXISTS (
    SELECT 1 FROM worker_sessions other
    JOIN worker_session_backends b ON b.worker_session_id = other.id
    WHERE other.provider = 'codex' AND other.thread_id = NEW.thread_id AND b.backend_id != 'default'
)
BEGIN SELECT RAISE(ABORT, 'codex thread cannot cross backend scopes'); END;
CREATE TRIGGER worker_session_backends_insert BEFORE INSERT ON worker_session_backends
WHEN EXISTS (SELECT 1 FROM worker_session_backends WHERE worker_session_id = NEW.worker_session_id) OR NOT EXISTS (
    SELECT 1 FROM worker_sessions s WHERE s.id = NEW.worker_session_id
    AND s.provider = NEW.provider AND s.thread_id IS NULL
)
BEGIN SELECT RAISE(ABORT, 'session backend must precede thread creation and match provider'); END;
CREATE TRIGGER worker_session_backends_no_update BEFORE UPDATE ON worker_session_backends
BEGIN SELECT RAISE(ABORT, 'session backend is immutable'); END;
CREATE TRIGGER worker_session_failure_epoch_insert AFTER INSERT ON worker_sessions
WHEN NEW.state = 'failed'
BEGIN
    UPDATE worker_sessions SET failure_epoch = 1 WHERE id = NEW.id;
END;
CREATE TRIGGER worker_session_failure_epoch_transition AFTER UPDATE OF state ON worker_sessions
WHEN OLD.state != 'failed' AND NEW.state = 'failed'
BEGIN
    UPDATE worker_sessions SET failure_epoch = OLD.failure_epoch + 1 WHERE id = NEW.id;
END;
CREATE INDEX ws_card_id_idx
    ON worker_sessions(card_id) WHERE card_id IS NOT NULL;
CREATE UNIQUE INDEX ws_one_active_per_card
   ON worker_sessions(card_id)
WHERE state IN ('starting','running','idle','turn_pending');
CREATE INDEX ws_provider_session_idx
    ON worker_sessions(provider, agent_session_id) WHERE agent_session_id IS NOT NULL;
CREATE INDEX ws_provider_thread_idx
    ON worker_sessions(provider, thread_id) WHERE thread_id IS NOT NULL;
CREATE INDEX ws_requester_idx ON worker_sessions(requester_session_id)
  WHERE requester_session_id IS NOT NULL;
CREATE INDEX ws_terminal_run_idx
    ON worker_sessions(terminal_run_id) WHERE terminal_run_id IS NOT NULL;
CREATE UNIQUE INDEX ws_token_idx ON worker_sessions(mcp_token_hash)
  WHERE mcp_token_hash IS NOT NULL;
CREATE INDEX ws_track_idx ON worker_sessions(track_id, created_at_ms, id);
CREATE TRIGGER system_assistant_entry_scope BEFORE INSERT ON conversation_system_assistant_entries
WHEN NOT EXISTS (
    SELECT 1 FROM cards c JOIN worker_sessions ws ON ws.id=c.session_id AND ws.card_id=c.id AND ws.track_id=c.track_id
    WHERE c.id=NEW.card_id AND c.track_id=NEW.track_id AND c.role='assistant'
      AND ws.id=NEW.original_worker AND ws.contract='assistant' AND ws.provider='codex' AND ws.mode='resumable' AND ws.state='idle'
      AND json_extract(c.payload,'$.harness_profile')='assistant' AND json_extract(c.payload,'$.assistant_provider')='codex'
      AND NOT EXISTS (SELECT 1 FROM conversation_assistant_creations h WHERE h.card_id=c.id)
      AND NOT EXISTS (SELECT 1 FROM conversation_presentations p WHERE p.card_id=c.id)
      AND ws.active_turn_id IS NULL AND ws.mcp_token_hash IS NULL
      AND ws.thread_id IS NULL AND ws.agent_session_id IS NULL AND ws.spawn_op_id IS NULL
      AND json_extract(ws.handle_state_json,'$.schema_version')=5
      AND json_extract(ws.handle_state_json,'$.push_watermark')=NEW.creation_floor
      AND NEW.creation_floor=COALESCE((SELECT seq FROM sqlite_sequence WHERE name='events'),0)
      AND json_extract(ws.handle_state_json,'$.mode')='harness'
      AND json_extract(ws.handle_state_json,'$.phase')='idle'
      AND json_array_length(ws.handle_state_json,'$.canonical_inputs.pending')=0
      AND json_extract(ws.handle_state_json,'$.canonical_inputs.next_queue_position')=0
      AND json_type(ws.handle_state_json,'$.canonical_inputs.active')='null'
      AND json_array_length(ws.handle_state_json,'$.canonical_inputs.attempts')=0
      AND json_array_length(ws.handle_state_json,'$.canonical_inputs.receipts')=0
)
BEGIN SELECT RAISE(ABORT,'System Assistant entry must bind its empty original carrier'); END;
CREATE TRIGGER system_assistant_receipt_scope BEFORE INSERT ON conversation_system_assistant_receipts
WHEN NOT EXISTS (
    SELECT 1 FROM conversation_system_assistant_entries e
    JOIN cards c ON c.id=e.card_id AND c.track_id=e.track_id AND c.session_id=e.original_worker
    JOIN tracks t ON t.id=c.track_id
    JOIN worker_sessions ws ON ws.id=e.original_worker AND ws.card_id=c.id AND ws.track_id=t.id
    JOIN conversation_presentations p ON p.card_id=e.card_id AND p.source_kind='system_observation' AND p.observation_id=NEW.observation_id AND p.erased_by_operation_id IS NULL
    JOIN json_each(ws.handle_state_json,'$.canonical_inputs.pending') pending
    JOIN events v ON v.id=NEW.event_id AND v.kind='assistant.input_requested' AND v.scope_kind='card'
    WHERE c.role='assistant' AND ws.contract='assistant' AND ws.provider='codex' AND ws.mode='resumable'
      AND ws.state IN ('starting','running','idle','turn_pending')
      AND json_extract(c.payload,'$.harness_profile')='assistant'
      AND json_extract(c.payload,'$.assistant_provider')='codex'
      AND e.card_id=NEW.card_id AND e.source=NEW.source AND v.scope_card=e.card_id AND v.scope_track=e.track_id
      AND json_extract(v.actor,'$.kind') IN ('Kernel','KernelDispatcher')
      AND json_extract(v.payload,'$.card_id')=e.card_id AND json_extract(v.payload,'$.track_id')=e.track_id
      AND v.scope_area=t.area_id
      AND json_extract(v.payload,'$.source')=e.source
      AND json_extract(v.payload,'$.original_worker')=e.original_worker
      AND json_extract(v.payload,'$.subject')=NEW.subject AND json_extract(v.payload,'$.version')=1
      AND v.id>e.creation_floor
      AND json_extract(NEW.receipt_json,'$.version')='system-assistant-v1'
      AND json_extract(NEW.receipt_json,'$.cardId')=e.card_id AND json_extract(NEW.receipt_json,'$.trackId')=e.track_id
      AND json_extract(NEW.receipt_json,'$.originalWorker')=e.original_worker
      AND json_extract(NEW.receipt_json,'$.eventId')=v.id AND json_extract(NEW.receipt_json,'$.observationId')=NEW.observation_id
      AND json_extract(NEW.receipt_json,'$.subject')=NEW.subject AND length(NEW.subject)>0
      AND json_extract(NEW.receipt_json,'$.source')=e.source
      AND json_extract(NEW.receipt_json,'$.presentationSequence')=p.sequence
      AND length(json_extract(NEW.receipt_json,'$.payloadDigest'))=64
      AND length(json_extract(NEW.receipt_json,'$.eventFingerprint'))=64
      AND json_extract(pending.value,'$.kind')='system'
      AND json_extract(pending.value,'$.observation_id')=NEW.observation_id
      AND json_extract(pending.value,'$.envelope_id')=NEW.event_id
      AND json_extract(pending.value,'$.queued_at_ms')=json_extract(NEW.receipt_json,'$.queuedAtMs')
      AND ((NEW.bootstrap=1 AND NEW.subject='bootstrap'
          AND ((e.source='today-summary' AND json_extract(NEW.receipt_json,'$.acceptance.kind')='bootstrap')
            OR (e.source='worker-watcher' AND json_extract(NEW.receipt_json,'$.acceptance.kind')='watcher-bootstrap'))
          AND json_extract(NEW.receipt_json,'$.acceptance.creation_floor')=e.creation_floor
          AND json_extract(NEW.receipt_json,'$.payloadDigest')=e.bootstrap_digest)
        OR (NEW.bootstrap=0 AND NEW.subject!='bootstrap'
          AND ((e.source='today-summary' AND json_extract(NEW.receipt_json,'$.acceptance.kind')='input')
            OR (e.source='worker-watcher' AND json_extract(NEW.receipt_json,'$.acceptance.kind')='watcher-input'))))
)
BEGIN SELECT RAISE(ABORT,'System Assistant receipt must bind its original targeted event'); END;
CREATE TRIGGER conversation_retry_relation_scope
BEFORE INSERT ON conversation_input_retry_relations
WHEN EXISTS(SELECT 1 FROM conversation_input_retry_relations old
    WHERE old.retry_intent_id=NEW.retry_intent_id
       OR (old.carrier_id=NEW.carrier_id AND (old.predecessor_attempt_id=NEW.predecessor_attempt_id
                                            OR old.resolution_key=NEW.resolution_key)))
 OR NOT EXISTS(SELECT 1 FROM (SELECT
    NEW.retry_intent_id AS retry_intent_id,
    NEW.card_id AS card_id,
    NEW.carrier_id AS carrier_id,
    NEW.predecessor_submission_id AS predecessor_submission_id,
    NEW.root_submission_id AS root_submission_id,
    NEW.predecessor_attempt_id AS predecessor_attempt_id,
    NEW.resolution_key AS resolution_key,
    NEW.principal_user_id AS principal_user_id,
    NEW.fingerprint AS fingerprint,
    NEW.queue_position AS queue_position,
    NEW.created_at_ms AS created_at_ms) r JOIN worker_sessions ws
    JOIN conversation_input_submissions predecessor ON predecessor.id=r.predecessor_submission_id
    JOIN conversation_turns original ON original.id=predecessor.logical_turn_id
    JOIN conversation_input_submissions root ON root.id=r.root_submission_id
    JOIN json_each(CASE WHEN json_valid(ws.handle_state_json) THEN ws.handle_state_json ELSE '{}' END,
                   '$.canonical_inputs.receipts') receipt
    JOIN json_each(ws.handle_state_json,'$.canonical_inputs.attempts') attempt
    JOIN json_each(ws.handle_state_json,'$.canonical_inputs.pending') pending
    WHERE json_extract(pending.value,'$.kind')='explicit_successor'
      AND json_extract(pending.value,'$.retry_intent_id')=r.retry_intent_id
      AND json_extract(pending.value,'$.queue_position')=r.queue_position
      AND json_extract(pending.value,'$.queued_at_ms')=r.created_at_ms
      AND ws.id=r.carrier_id AND ws.card_id=r.card_id
      AND predecessor.card_id=r.card_id AND original.initiating_runtime_id=r.carrier_id
      AND root.card_id=r.card_id AND root.render_plan_root_submission_id IS NULL
      AND root.render_plan_json IS NOT NULL
      AND r.root_submission_id=COALESCE(predecessor.render_plan_root_submission_id,predecessor.id)
      AND json_extract(ws.handle_state_json,'$.schema_version')=5
      AND json_extract(receipt.value,'$.key')=r.resolution_key
      AND json_extract(receipt.value,'$.principal')=r.principal_user_id
      AND json_extract(receipt.value,'$.fingerprint')=r.fingerprint
      AND json_extract(receipt.value,'$.decision.kind')='retry'
      AND json_extract(receipt.value,'$.decision.predecessor.carrier_id')=r.carrier_id
      AND json_extract(receipt.value,'$.decision.predecessor.submission_id')=r.predecessor_submission_id
      AND json_extract(receipt.value,'$.decision.predecessor.attempt_id')=r.predecessor_attempt_id
      AND json_extract(receipt.value,'$.decision.retry_intent_id')=r.retry_intent_id
      AND json_extract(attempt.value,'$.attempt.submission_id')=r.predecessor_submission_id
      AND json_extract(attempt.value,'$.attempt.attempt_id')=r.predecessor_attempt_id
      AND json_type(receipt.value,'$.decision.predecessor.revision')='text'
      AND json_type(receipt.value,'$.decision.revision')='text'
      AND CAST(json_extract(receipt.value,'$.decision.predecessor.revision') AS INTEGER)>=0
      AND CAST(CAST(json_extract(receipt.value,'$.decision.predecessor.revision') AS INTEGER) AS TEXT)
          =json_extract(receipt.value,'$.decision.predecessor.revision')
      AND CAST(CAST(json_extract(receipt.value,'$.decision.revision') AS INTEGER) AS TEXT)
          =json_extract(receipt.value,'$.decision.revision')
      AND json_type(attempt.value,'$.revision')='integer'
      AND CAST(json_extract(receipt.value,'$.decision.predecessor.revision') AS INTEGER)+1
          =CAST(json_extract(receipt.value,'$.decision.revision') AS INTEGER)
      AND json_extract(attempt.value,'$.revision')>=CAST(json_extract(receipt.value,'$.decision.revision') AS INTEGER))
BEGIN
    SELECT RAISE(ABORT, 'Retry relation requires its exact original-carrier receipt and reserved identities');
END;
CREATE TRIGGER conversation_submission_human_retirement_receipt
BEFORE UPDATE ON conversation_submission_inputs
WHEN NEW.retirement_reason='human_resolution' AND NOT EXISTS (
    SELECT 1 FROM conversation_input_retry_relations r
    WHERE r.retry_intent_id=NEW.retired_by_retry_intent_id
      AND r.predecessor_submission_id=OLD.submission_id
      AND EXISTS(SELECT 1 FROM worker_sessions ws
    JOIN conversation_input_submissions predecessor ON predecessor.id=r.predecessor_submission_id
    JOIN conversation_turns original ON original.id=predecessor.logical_turn_id
    JOIN conversation_input_submissions root ON root.id=r.root_submission_id
    JOIN json_each(CASE WHEN json_valid(ws.handle_state_json) THEN ws.handle_state_json ELSE '{}' END,
                   '$.canonical_inputs.receipts') receipt
    JOIN json_each(ws.handle_state_json,'$.canonical_inputs.attempts') attempt
    WHERE ws.id=r.carrier_id AND ws.card_id=r.card_id
      AND predecessor.card_id=r.card_id AND original.initiating_runtime_id=r.carrier_id
      AND root.card_id=r.card_id AND root.render_plan_root_submission_id IS NULL
      AND root.render_plan_json IS NOT NULL
      AND r.root_submission_id=COALESCE(predecessor.render_plan_root_submission_id,predecessor.id)
      AND json_extract(ws.handle_state_json,'$.schema_version')=5
      AND json_extract(receipt.value,'$.key')=r.resolution_key
      AND json_extract(receipt.value,'$.principal')=r.principal_user_id
      AND json_extract(receipt.value,'$.fingerprint')=r.fingerprint
      AND json_extract(receipt.value,'$.decision.kind')='retry'
      AND json_extract(receipt.value,'$.decision.predecessor.carrier_id')=r.carrier_id
      AND json_extract(receipt.value,'$.decision.predecessor.submission_id')=r.predecessor_submission_id
      AND json_extract(receipt.value,'$.decision.predecessor.attempt_id')=r.predecessor_attempt_id
      AND json_extract(receipt.value,'$.decision.retry_intent_id')=r.retry_intent_id
      AND json_extract(attempt.value,'$.attempt.submission_id')=r.predecessor_submission_id
      AND json_extract(attempt.value,'$.attempt.attempt_id')=r.predecessor_attempt_id
      AND json_type(receipt.value,'$.decision.predecessor.revision')='text'
      AND json_type(receipt.value,'$.decision.revision')='text'
      AND CAST(json_extract(receipt.value,'$.decision.predecessor.revision') AS INTEGER)>=0
      AND CAST(CAST(json_extract(receipt.value,'$.decision.predecessor.revision') AS INTEGER) AS TEXT)
          =json_extract(receipt.value,'$.decision.predecessor.revision')
      AND CAST(CAST(json_extract(receipt.value,'$.decision.revision') AS INTEGER) AS TEXT)
          =json_extract(receipt.value,'$.decision.revision')
      AND json_type(attempt.value,'$.revision')='integer'
      AND CAST(json_extract(receipt.value,'$.decision.predecessor.revision') AS INTEGER)+1
          =CAST(json_extract(receipt.value,'$.decision.revision') AS INTEGER)
      AND json_extract(attempt.value,'$.revision')>=CAST(json_extract(receipt.value,'$.decision.revision') AS INTEGER))
)
BEGIN
    SELECT RAISE(ABORT, 'Human retirement requires its exact linked retry receipt');
END;
CREATE TRIGGER assistant_creation_fact_scope
BEFORE INSERT ON conversation_assistant_creations
WHEN NOT EXISTS (
    SELECT 1 FROM cards c
    JOIN worker_sessions ws ON ws.id=c.session_id AND ws.card_id=c.id AND ws.track_id=c.track_id
    JOIN conversation_messages m ON m.id=NEW.message_id AND m.card_id=c.id
    JOIN conversation_message_revisions mr ON mr.message_id=m.id AND mr.revision=1
    JOIN conversation_presentations p ON p.card_id=c.id AND p.message_id=m.id AND p.source_kind='human_message'
    WHERE c.id=NEW.card_id AND c.track_id=NEW.track_id AND c.role='assistant'
      AND json_extract(c.payload,'$.harness_profile')='assistant'
      AND ws.provider=json_extract(c.payload,'$.assistant_provider')
      AND ws.id=NEW.origin_runtime_id AND ws.mode='resumable' AND ws.contract='assistant' AND ws.state='idle'
      AND ws.thread_id IS NULL AND ws.agent_session_id IS NULL AND ws.active_turn_id IS NULL
      AND ws.spawn_op_id IS NULL AND ws.mcp_token_hash IS NULL
      AND json_extract(ws.handle_state_json,'$.schema_version')=5
      AND json_extract(ws.handle_state_json,'$.mode')='harness'
      AND json_extract(ws.handle_state_json,'$.phase')='idle'
      AND json_array_length(ws.handle_state_json,'$.canonical_inputs.pending')=1
      AND json_extract(ws.handle_state_json,'$.canonical_inputs.next_queue_position')=1
      AND json_extract(ws.handle_state_json,'$.canonical_inputs.pending[0].kind')='human'
      AND json_extract(ws.handle_state_json,'$.canonical_inputs.pending[0].message.message_id')=NEW.message_id
      AND json_extract(ws.handle_state_json,'$.canonical_inputs.pending[0].message.revision')=1
      AND json_type(ws.handle_state_json,'$.canonical_inputs.active')='null'
      AND json_array_length(ws.handle_state_json,'$.canonical_inputs.attempts')=0
      AND json_array_length(ws.handle_state_json,'$.canonical_inputs.receipts')=0
      AND ((m.author_kind='authenticated_human' AND m.author_user_id=NEW.source_user_id)
        OR (m.author_kind='link_guest' AND EXISTS (
            SELECT 1 FROM share_links sl WHERE sl.id=m.author_share_link_id
              AND sl.approver_user_id=NEW.source_user_id
        )))
      AND m.current_revision=1 AND m.withdrawn_at_ms IS NULL
      AND json_extract(mr.payload_json,'$.state')='present'
      AND p.sequence=NEW.presentation_sequence
      AND json_extract(NEW.receipt_json,'$.version')='initial-input-v1'
      AND json_extract(NEW.receipt_json,'$.entryId')=NEW.message_id
      AND json_extract(NEW.receipt_json,'$.entryRev')=1
      AND json_extract(NEW.receipt_json,'$.initialRuntimeId')=ws.id
      AND json_extract(NEW.receipt_json,'$.queuedAtMs')=m.created_at_ms
      AND NOT EXISTS (SELECT 1 FROM conversation_assistant_creations old
          WHERE old.card_id=NEW.card_id OR old.origin_runtime_id=NEW.origin_runtime_id OR old.message_id=NEW.message_id)
)
BEGIN
    SELECT RAISE(ABORT, 'Assistant creation must bind its dormant carrier and original acceptance');
END;
CREATE TRIGGER conversation_human_track_creation_fact_scope
BEFORE INSERT ON conversation_human_track_creations
WHEN NOT EXISTS (
    SELECT 1 FROM track_create_idempotency b
    JOIN tracks t ON t.id=b.track_id AND t.area_id=b.area_id
    JOIN cards c ON c.id=b.planner_card_id AND c.track_id=t.id AND c.role='planner'
    JOIN cards r ON r.id=b.report_card_id AND r.track_id=t.id AND r.role='reportcard'
    JOIN worker_sessions ws ON ws.id=c.session_id AND ws.card_id=c.id AND ws.track_id=t.id
    JOIN conversation_messages m ON m.id=NEW.message_id AND m.card_id=c.id
    JOIN conversation_message_revisions mr ON mr.message_id=m.id AND mr.revision=NEW.message_revision
    JOIN conversation_presentations p ON p.card_id=c.id AND p.source_kind='human_message' AND p.message_id=m.id
    WHERE b.area_id=NEW.area_id AND b.idempotency_key=NEW.idempotency_key
      AND b.request_fingerprint_version=1
      AND ws.id=NEW.origin_runtime_id AND ws.mode='resumable' AND ws.contract='planner' AND ws.state='idle'
      AND ws.thread_id IS NULL AND ws.agent_session_id IS NULL AND ws.active_turn_id IS NULL
      AND ws.mcp_token_hash IS NULL
      AND CASE WHEN json_valid(ws.handle_state_json) THEN
        json_extract(ws.handle_state_json,'$.schema_version')=5
        AND json_extract(ws.handle_state_json,'$.mode')='harness'
        AND json_extract(ws.handle_state_json,'$.phase')='idle'
        AND json_array_length(ws.handle_state_json,'$.canonical_inputs.pending')=1
        AND json_extract(ws.handle_state_json,'$.canonical_inputs.next_queue_position')=1
        AND json_type(ws.handle_state_json,'$.canonical_inputs.active')='null'
        AND json_array_length(ws.handle_state_json,'$.canonical_inputs.attempts')=0
        AND json_type(ws.handle_state_json,'$.canonical_inputs.receipts')='array'
        AND json_array_length(ws.handle_state_json,'$.canonical_inputs.receipts')=0
        AND json_extract(ws.handle_state_json,'$.canonical_inputs.pending[0].kind')='human'
        AND json_extract(ws.handle_state_json,'$.canonical_inputs.pending[0].message.message_id')=NEW.message_id
        AND json_extract(ws.handle_state_json,'$.canonical_inputs.pending[0].message.revision')=NEW.message_revision
      ELSE 0 END IS TRUE
      AND ((m.author_kind='authenticated_human' AND m.author_user_id=NEW.source_user_id)
        OR (m.author_kind='link_guest' AND EXISTS (
            SELECT 1 FROM share_links sl WHERE sl.id=m.author_share_link_id
              AND sl.approver_user_id=NEW.source_user_id
        ))) AND m.current_revision=1
      AND m.withdrawn_at_ms IS NULL AND json_extract(mr.payload_json,'$.state')='present'
      AND p.sequence=NEW.presentation_sequence
      -- Preserve the original one-creation-per-resource relation and refuse
      -- INSERT OR REPLACE even when recursive DELETE triggers are disabled.
      AND NOT EXISTS (
          SELECT 1 FROM conversation_human_track_creations old
          JOIN track_create_idempotency original
            ON original.area_id=old.area_id AND original.idempotency_key=old.idempotency_key
          WHERE (old.area_id=NEW.area_id AND old.idempotency_key=NEW.idempotency_key)
             OR original.track_id=b.track_id OR original.planner_card_id=b.planner_card_id
             OR old.origin_runtime_id=NEW.origin_runtime_id OR old.message_id=NEW.message_id
      )
)
BEGIN
    SELECT RAISE(ABORT, 'Human creation acceptance must bind its actual dormant allocation and first input');
END;
CREATE TEMP TABLE task_worker_fk_assert (invalid INTEGER CHECK(invalid=0));
INSERT INTO task_worker_fk_assert SELECT count(*) FROM (SELECT * FROM pragma_foreign_key_check EXCEPT SELECT * FROM task_worker_fk_before);
DROP TABLE task_worker_fk_assert;
DROP TABLE task_worker_fk_before;
