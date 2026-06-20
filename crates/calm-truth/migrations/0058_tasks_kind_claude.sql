-- Issue #796 Phase 1 slices 2+3 — make claude a first-class task kind.
--
-- SQLite cannot ALTER a CHECK constraint in place, so rebuild the table
-- preserving the current tasks schema and widening tasks.kind.
PRAGMA foreign_keys = OFF;

CREATE TABLE tasks_new (
  id              TEXT PRIMARY KEY,
  wave_id         TEXT NOT NULL,
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
  finished_at_ms  INTEGER NULL,
  UNIQUE (wave_id, key)
);

INSERT INTO tasks_new (
  id, wave_id, key, kind, goal, context_json, acceptance_criteria, cwd,
  depends_on_json, priority, gate_json, status, status_detail, worker_card_id,
  gate_result_json, gate_attempt, gate_pid, gate_pid_starttime, gate_pid_boot_id,
  running_deadline_ms, created_at_ms, updated_at_ms, finished_at_ms
)
SELECT
  id, wave_id, key, kind, goal, context_json, acceptance_criteria, cwd,
  depends_on_json, priority, gate_json, status, status_detail, worker_card_id,
  gate_result_json, gate_attempt, gate_pid, gate_pid_starttime, gate_pid_boot_id,
  running_deadline_ms, created_at_ms, updated_at_ms, finished_at_ms
FROM tasks;

DROP TABLE tasks;
ALTER TABLE tasks_new RENAME TO tasks;
CREATE INDEX tasks_wave_status_idx ON tasks(wave_id, status, priority DESC, created_at_ms);
CREATE INDEX idx_tasks_liveness_deadlines
  ON tasks (running_deadline_ms)
  WHERE status = 'running';

PRAGMA foreign_keys = ON;
