-- Issue #644 — wave-scoped task plan. Source of truth for plan-then-schedule.
CREATE TABLE tasks (
  id              TEXT PRIMARY KEY,           -- "{wave_id}:{key}", see below
  wave_id         TEXT NOT NULL,
  key             TEXT NOT NULL,              -- spec-chosen, stable, short
  kind            TEXT NOT NULL CHECK (kind IN ('codex', 'terminal')),
  goal            TEXT NOT NULL,              -- codex: goal text; terminal: cmd
  context_json    TEXT NOT NULL CHECK (json_valid(context_json)),
  acceptance_criteria TEXT NULL,
  cwd             TEXT NULL,                  -- terminal worker / gate cwd override
  depends_on_json TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(depends_on_json)),
  priority        INTEGER NOT NULL DEFAULT 0, -- higher schedules first
  gate_json       TEXT NULL CHECK (gate_json IS NULL OR json_valid(gate_json)),
  status          TEXT NOT NULL DEFAULT 'pending' CHECK (status IN (
    'pending', 'dispatched', 'running', 'verifying', 'done', 'failed', 'canceled'
  )),
  status_detail   TEXT NULL,                  -- machine-short reason, e.g. 'gate-red', 'worker-reported', 'spawn-failed'
  worker_card_id  TEXT NULL,                  -- stamped at dispatched→running AND in the report tx (§3)
  gate_result_json TEXT NULL CHECK (gate_result_json IS NULL OR json_valid(gate_result_json)),
  gate_attempt    INTEGER NOT NULL DEFAULT 0, -- attempts *prepared* (bumped in prepare_tx, §6.2)
  gate_pid        INTEGER NULL,               -- pgid of the live gate wrapper (§6.2)
  gate_pid_starttime INTEGER NULL,            -- /proc/<pid>/stat field 22 at spawn; same-boot pid-reuse guard
  gate_pid_boot_id TEXT NULL,                 -- /proc/sys/kernel/random/boot_id at spawn; cross-reboot guard (spec_appserver.rs:145 pattern)
  created_at_ms   INTEGER NOT NULL,
  updated_at_ms   INTEGER NOT NULL,
  finished_at_ms  INTEGER NULL,
  UNIQUE (wave_id, key)
);
CREATE INDEX tasks_wave_status_idx ON tasks(wave_id, status, priority DESC, created_at_ms);

-- Per-wave scheduler budget + gate policy (see design §5.3, §6.6). NULL budget =
-- kernel default. Gate policy: DB DEFAULT 1 makes every wave created after
-- this migration default ON without touching the create path — NewWave /
-- wave_create_tx insert a fixed column list (model.rs, db/sqlite.rs) and
-- never name this column, so the DEFAULT applies; the backfill UPDATE
-- (runs in the same migration, after the ALTER stamps existing rows with 1)
-- resets pre-#644 waves to 0 so in-flight waves keep their behavior.
ALTER TABLE waves ADD COLUMN task_budget INTEGER NULL;
ALTER TABLE waves ADD COLUMN require_task_gates INTEGER NOT NULL DEFAULT 1;
UPDATE waves SET require_task_gates = 0;
