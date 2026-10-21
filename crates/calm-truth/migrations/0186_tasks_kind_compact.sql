-- Add the worker-less Planner compaction task kind; preserve the released task schema.
DROP VIEW current_tasks;
DROP TRIGGER task_attempt_recovery_predecessor;
-- foreign_keys stays ON in the migrator transaction. Stage and remove the
-- referencing table before DROP TABLE tasks so its CASCADE cannot erase data.
CREATE TABLE task_ref_index_saved AS SELECT * FROM task_ref_index;
DROP TABLE task_ref_index;
DROP TRIGGER task_ref_index_cleanup_terminal;
CREATE TABLE tasks_next (
  id              TEXT PRIMARY KEY,
  track_id         TEXT NOT NULL,
  key             TEXT NOT NULL,
  kind            TEXT NOT NULL CHECK (kind IN ('codex', 'terminal', 'claude', 'compact')),
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
  FOREIGN KEY (id, track_id, key) REFERENCES task_attempt_allocations(attempt_id, track_id, key)
);
INSERT INTO tasks_next SELECT * FROM tasks;
DROP TABLE tasks;
ALTER TABLE tasks_next RENAME TO tasks;
CREATE INDEX tasks_track_status_idx ON tasks(track_id,status,priority DESC,created_at_ms);
CREATE INDEX idx_tasks_liveness_deadlines
  ON tasks (running_deadline_ms)
  WHERE status = 'running';
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
