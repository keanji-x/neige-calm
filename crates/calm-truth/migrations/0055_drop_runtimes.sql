-- IRREVERSIBLE — no rollback after this migration. PR9b retires runtimes.
--
-- Migration bridge (steps 1, 3, 3.5, 3.6): close every side effect that
-- migration 0050 + the deleted backfill_worker_sessions_from_runtimes_on_boot
-- used to establish for active runtimes, for the multi-version-upgrade case
-- where a runtimes row never received a worker_sessions mirror before
-- this PR drops the table. Each is idempotent: cleanly-mirrored DBs see
-- zero row changes from these steps.

-- 1. Multi-version upgrade bridge: copy runtime rows that never received a
--    worker_sessions mirror before runtimes disappears.
INSERT INTO worker_sessions (
  id,
  wave_id,
  provider,
  mode,
  contract,
  parent_session_id,
  requester_session_id,
  state,
  mcp_token_hash,
  thread_id,
  agent_session_id,
  active_turn_id,
  terminal_run_id,
  handle_state_json,
  liveness,
  liveness_probed_at_ms,
  exit_code,
  exit_interpretation,
  spawn_op_id,
  created_at_ms,
  updated_at_ms,
  completed_at_ms,
  last_activity_ms,
  last_thread_status,
  card_id
)
SELECT
  r.id,
  c.wave_id,
  CASE r.kind
    WHEN 'terminal' THEN 'terminal'
    WHEN 'codex' THEN 'codex'
    WHEN 'claude' THEN 'claude'
    WHEN 'shared-spec' THEN 'codex'
  END,
  'resumable',
  CASE r.kind
    WHEN 'shared-spec' THEN 'planner'
    ELSE 'executor'
  END,
  NULL,
  NULL,
  r.status,
  NULL,
  r.thread_id,
  r.session_id,
  r.active_turn_id,
  r.terminal_run_id,
  r.handle_state_json,
  'unknown',
  NULL,
  NULL,
  NULL,
  NULL,
  r.created_at_ms,
  r.updated_at_ms,
  r.completed_at_ms,
  NULL,
  NULL,
  r.card_id
FROM runtimes r
JOIN cards c ON c.id = r.card_id
LEFT JOIN worker_sessions ws ON ws.id = r.id
WHERE ws.id IS NULL;

-- 2. Defensive de-dup pre-step: resolve any in-flight double-active ws rows
--    (gap state from a prior-binary deferred-spec window). Keep the newest;
--    supersede the older duplicates so the unique index can build clean.
UPDATE worker_sessions
   SET state = 'superseded',
       updated_at_ms = strftime('%s', 'now') * 1000,
       completed_at_ms = COALESCE(completed_at_ms, strftime('%s', 'now') * 1000)
 WHERE id IN (
   SELECT id FROM worker_sessions ws
    WHERE ws.state IN ('starting','running','idle','turn_pending')
      AND ws.card_id IS NOT NULL
      AND EXISTS (
        SELECT 1 FROM worker_sessions sib
         WHERE sib.card_id = ws.card_id
           AND sib.state IN ('starting','running','idle','turn_pending')
           AND (sib.updated_at_ms, sib.created_at_ms, sib.id)
             > (ws.updated_at_ms, ws.created_at_ms, ws.id)
      )
 );

-- 3. Repoint cards.session_id when it's NULL or pointing at a non-active ws,
--    provided a better target (non-superseded ws with matching card_id)
--    exists. Idempotent: cards correctly pointed at an active ws or at a
--    terminal (failed/exited) ws are untouched.
UPDATE cards
   SET session_id = (
       SELECT ws.id
         FROM worker_sessions ws
        WHERE ws.card_id = cards.id
          AND ws.state != 'superseded'
        ORDER BY CASE
                   WHEN ws.state IN ('starting','running','idle','turn_pending')
                     THEN 0
                   ELSE 1
                 END ASC,
                 ws.updated_at_ms DESC,
                 ws.created_at_ms DESC,
                 ws.id DESC
        LIMIT 1
   )
 WHERE (cards.session_id IS NULL
        OR EXISTS (
            SELECT 1 FROM worker_sessions ws
             WHERE ws.id = cards.session_id
               AND ws.state = 'superseded'
        ))
   AND EXISTS (
       SELECT 1 FROM worker_sessions ws
        WHERE ws.card_id = cards.id
          AND ws.state != 'superseded'
   );

-- 3.5. Repoint waves.root_session_id when it's NULL, points at a
--      now-nonexistent session, or points at a superseded planner -- provided
--      a better target (active planner in the same wave) exists.
--      Idempotent: waves correctly pointed at an active planner are untouched.
UPDATE waves
   SET root_session_id = (
       SELECT ws.id
         FROM worker_sessions ws
        WHERE ws.wave_id = waves.id
          AND ws.contract = 'planner'
          AND ws.state IN ('starting', 'running', 'idle', 'turn_pending')
        ORDER BY ws.updated_at_ms DESC,
                 ws.created_at_ms DESC,
                 ws.id DESC
        LIMIT 1
   )
 WHERE (waves.root_session_id IS NULL
        OR waves.root_session_id NOT IN (SELECT id FROM worker_sessions)
        OR EXISTS (
            SELECT 1 FROM worker_sessions ws
             WHERE ws.id = waves.root_session_id
               AND ws.state = 'superseded'
        ))
   AND EXISTS (
       SELECT 1 FROM worker_sessions ws
        WHERE ws.wave_id = waves.id
          AND ws.contract = 'planner'
          AND ws.state IN ('starting', 'running', 'idle', 'turn_pending')
   );

-- 3.6. Replay 0050's MCP token mirror: for any active ws with NULL
--      mcp_token_hash AND a matching unique card_mcp_tokens row, mirror
--      the token hash. Drops the runtimes JOIN (ws.card_id is now
--      populated post-PR9b-0). The uniqueness guards prevent ws_token_idx
--      violation on malformed/duplicate historical token rows.
UPDATE worker_sessions
   SET mcp_token_hash = (
       SELECT cmt.hashed_token
         FROM card_mcp_tokens cmt
        WHERE cmt.card_id = worker_sessions.card_id
          AND 1 = (
              SELECT COUNT(*)
                FROM card_mcp_tokens dup
               WHERE dup.hashed_token = cmt.hashed_token
          )
          AND NOT EXISTS (
              SELECT 1 FROM worker_sessions other
               WHERE other.id != worker_sessions.id
                 AND other.mcp_token_hash = cmt.hashed_token
          )
        LIMIT 1
   )
 WHERE worker_sessions.mcp_token_hash IS NULL
   AND worker_sessions.state IN ('starting', 'running', 'idle', 'turn_pending')
   AND worker_sessions.card_id IS NOT NULL
   AND EXISTS (
       SELECT 1 FROM card_mcp_tokens cmt
        WHERE cmt.card_id = worker_sessions.card_id
          AND 1 = (
              SELECT COUNT(*) FROM card_mcp_tokens dup
               WHERE dup.hashed_token = cmt.hashed_token
          )
          AND NOT EXISTS (
              SELECT 1 FROM worker_sessions other
               WHERE other.id != worker_sessions.id
                 AND other.mcp_token_hash = cmt.hashed_token
          )
   );

-- 4. Structural double-spawn protection on the ws side.
CREATE UNIQUE INDEX ws_one_active_per_card
   ON worker_sessions(card_id)
WHERE state IN ('starting','running','idle','turn_pending');

-- 5. Drop runtimes table indexes by name (avoid ambiguity).
DROP INDEX IF EXISTS runtimes_active_per_card_idx;
DROP INDEX IF EXISTS runtimes_one_active_per_card;
DROP INDEX IF EXISTS runtimes_terminal_run_idx;
DROP INDEX IF EXISTS runtimes_thread_idx;
DROP INDEX IF EXISTS runtimes_session_idx;
DROP INDEX IF EXISTS runtimes_recover_scan_idx;

-- 6. Drop the table.
DROP TABLE runtimes;
