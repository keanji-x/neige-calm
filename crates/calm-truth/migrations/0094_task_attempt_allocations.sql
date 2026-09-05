-- #1501: author identity stays (track_id,key); tasks.id names one execution.
-- Allocations survive deletions of pending projection rows. The largest allocated
-- generation remains current even while no executable row exists for that ID.
CREATE TABLE task_attempt_allocations (
  attempt_id TEXT PRIMARY KEY NOT NULL CHECK(length(attempt_id)>0),
  track_id TEXT NOT NULL,
  key TEXT NOT NULL,
  generation INTEGER NOT NULL CHECK(typeof(generation)='integer' AND generation>=1),
  origin_json TEXT NOT NULL CHECK(json_valid(origin_json)),
  created_at_ms INTEGER NOT NULL,
  UNIQUE(track_id,key,generation),
  UNIQUE(attempt_id,track_id,key),
  CHECK ((CASE json_extract(origin_json,'$.kind')
    WHEN 'initial' THEN generation=1 AND json_remove(origin_json,'$.kind')='{}'
    WHEN 'recovery' THEN generation>1
      AND json_type(origin_json,'$.previous_attempt_id')='text'
      AND length(json_extract(origin_json,'$.previous_attempt_id'))>0
      AND json_type(origin_json,'$.idempotency_key')='text'
      AND length(trim(json_extract(origin_json,'$.idempotency_key')))>0
      AND json_type(origin_json,'$.request_fingerprint')='text'
      AND length(json_extract(origin_json,'$.request_fingerprint'))>0
      AND json_type(origin_json,'$.reason')='text'
      AND length(trim(json_extract(origin_json,'$.reason')))>0
      AND json_type(origin_json,'$.actor')='object'
      AND json_extract(origin_json,'$.constraint.version')='v1'
      AND json_extract(origin_json,'$.constraint.spawn')='in-wave'
      AND json_extract(origin_json,'$.constraint.declared_by') IN ('spec','user')
      AND json_type(origin_json,'$.constraint.refs')='array'
      AND json_array_length(origin_json,'$.constraint.refs')>0
    ELSE 0 END) IS TRUE)
);
CREATE UNIQUE INDEX task_attempt_request_idx ON task_attempt_allocations(
  track_id,json_extract(origin_json,'$.idempotency_key'))
  WHERE json_extract(origin_json,'$.kind')='recovery';
CREATE UNIQUE INDEX task_attempt_successor_idx ON task_attempt_allocations(
  json_extract(origin_json,'$.previous_attempt_id'))
  WHERE json_extract(origin_json,'$.kind')='recovery';

-- Preserve every old execution ID, including historical nonconventional IDs.
INSERT INTO task_attempt_allocations(attempt_id,track_id,key,generation,origin_json,created_at_ms)
SELECT id,track_id,key,1,'{"kind":"initial"}',created_at_ms FROM tasks;

-- foreign_keys stays ON in the migrator transaction. Stage and remove the
-- referencing table before DROP TABLE tasks so its CASCADE cannot erase data.
CREATE TABLE task_ref_index_saved AS SELECT * FROM task_ref_index;
DROP TABLE task_ref_index;
DROP TRIGGER task_ref_index_cleanup_terminal;
CREATE TABLE tasks_next (
  id              TEXT PRIMARY KEY,
  track_id         TEXT NOT NULL,
  key             TEXT NOT NULL,
  kind            TEXT NOT NULL CHECK (kind IN ('codex', 'terminal', 'claude')),
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
  CHECK (decl_released_by_user IN (0, 1)), context_verify_failures INTEGER NOT NULL DEFAULT 0, spawn TEXT NOT NULL DEFAULT 'in-wave', child_track_id TEXT NULL,
  FOREIGN KEY (id, track_id, key) REFERENCES task_attempt_allocations(attempt_id, track_id, key)
);
INSERT INTO tasks_next(id,track_id,key,kind,goal,context_json,acceptance_criteria,cwd,depends_on_json,priority,gate_json,status,status_detail,worker_card_id,gate_result_json,gate_attempt,gate_pid,gate_pid_starttime,gate_pid_boot_id,running_deadline_ms,created_at_ms,updated_at_ms,finished_at_ms,claim_context_json,context_stale_at_ms,context_closure_truncated,declared_by,decl_ready,decl_released_by_user,context_verify_failures,spawn,child_track_id) SELECT id,track_id,key,kind,goal,context_json,acceptance_criteria,cwd,depends_on_json,priority,gate_json,status,status_detail,worker_card_id,gate_result_json,gate_attempt,gate_pid,gate_pid_starttime,gate_pid_boot_id,running_deadline_ms,created_at_ms,updated_at_ms,finished_at_ms,claim_context_json,context_stale_at_ms,context_closure_truncated,declared_by,decl_ready,decl_released_by_user,context_verify_failures,spawn,child_track_id FROM tasks;

DROP TABLE tasks;
ALTER TABLE tasks_next RENAME TO tasks;
CREATE INDEX tasks_track_status_idx ON tasks(track_id,status,priority DESC,created_at_ms);
CREATE INDEX idx_tasks_liveness_deadlines ON tasks(running_deadline_ms) WHERE status='running';
CREATE UNIQUE INDEX idx_tasks_child_track_id ON tasks(child_track_id) WHERE child_track_id IS NOT NULL;
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

CREATE VIEW current_task_attempt_allocations AS
SELECT a.* FROM task_attempt_allocations a
WHERE NOT EXISTS(SELECT 1 FROM task_attempt_allocations newer
  WHERE newer.track_id=a.track_id AND newer.key=a.key AND newer.generation>a.generation);
CREATE VIEW current_tasks AS
SELECT t.* FROM tasks t JOIN current_task_attempt_allocations a ON a.attempt_id=t.id;

-- Covers projection, initial imports, and every real INSERT producer. Existing
-- allocations are never replaced; a second identity for the same generation is
-- rejected by the unique constraint, rather than implicitly authorizing recovery.
CREATE TRIGGER tasks_register_initial_attempt BEFORE INSERT ON tasks
WHEN NOT EXISTS(SELECT 1 FROM task_attempt_allocations WHERE attempt_id=NEW.id)
BEGIN
  INSERT INTO task_attempt_allocations(attempt_id,track_id,key,generation,origin_json,created_at_ms)
  VALUES(NEW.id,NEW.track_id,NEW.key,1,'{"kind":"initial"}',NEW.created_at_ms);
END;
CREATE TRIGGER task_attempt_allocations_immutable BEFORE UPDATE ON task_attempt_allocations
BEGIN SELECT RAISE(ABORT,'task attempt allocations are immutable'); END;
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
