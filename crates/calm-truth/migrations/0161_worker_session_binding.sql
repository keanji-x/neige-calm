-- #2493 S1: which worker session executed each task attempt, as an explicit fact.
-- `tasks.worker_session_id` is the binding (history: read whether or not the session still
-- lives); `worker_session_binding` is the one rule both history and authority readers select
-- from (`calm-truth/src/db/sqlite/worker_binding.rs`). A session serves at most one attempt.
-- A later migration that rebuilds `worker_sessions` must carry `tasks.worker_session_id` across
-- the DROP (as 0156 does for its inbound references): with foreign keys on, the implicit DELETE
-- of DROP TABLE would SET NULL every binding.
ALTER TABLE tasks ADD COLUMN worker_session_id TEXT REFERENCES worker_sessions(id) ON DELETE SET NULL;
CREATE UNIQUE INDEX tasks_worker_session_once
  ON tasks(worker_session_id) WHERE worker_session_id IS NOT NULL;

-- The lease belongs to an attempt; `lease_owner` stays the operation, `card_id` stays for the
-- path and the UI.
ALTER TABLE workspace_leases ADD COLUMN attempt_id TEXT;
CREATE INDEX workspace_leases_attempt_state_idx ON workspace_leases(attempt_id, state);

-- `session_active` is `WorkerSessionState::is_active_authority`, pinned by a calm-truth test.
CREATE VIEW worker_session_binding AS
SELECT ws.id AS session_id,
       ws.card_id AS card_id,
       ws.state IN ('starting','running','idle','turn_pending') AS session_active,
       t.id AS attempt_id,
       t.status AS attempt_status
  FROM worker_sessions ws
  LEFT JOIN tasks t ON t.worker_session_id = ws.id;

-- Backfill from the inference the binding replaces: the scheduler's worker-spawn operation
-- (`idempotency_key` = attempt) that started the session. Pre-scheduler workers have no
-- `tasks` row and stay unbound.
UPDATE tasks SET worker_session_id = (
    SELECT ws.id FROM operations o JOIN worker_sessions ws ON ws.spawn_op_id = o.id
     WHERE o.idempotency_key = tasks.id
       AND o.kind IN ('codex-worker','claude-worker','terminal-worker','codex-isolated-worker')
       AND json_extract(o.payload_json, '$.actor.kind') = 'KernelDispatcher')
 WHERE worker_session_id IS NULL;
UPDATE tasks SET worker_card_id = (SELECT card_id FROM worker_sessions WHERE id = tasks.worker_session_id)
 WHERE worker_card_id IS NULL AND worker_session_id IS NOT NULL;
UPDATE workspace_leases SET attempt_id = (
    SELECT t.id FROM operations o JOIN tasks t ON t.id = o.idempotency_key
     WHERE o.id = workspace_leases.lease_owner);
