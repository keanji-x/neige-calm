-- IRREVERSIBLE — no rollback after this migration. PR9b retires runtimes.

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
           AND sib.updated_at_ms > ws.updated_at_ms
      )
 );

-- 3. Repoint cards that still have no current session link after the bridge.
UPDATE cards
   SET session_id = (
       SELECT ws.id
        FROM runtimes r
        JOIN worker_sessions ws ON ws.id = r.id
        WHERE r.card_id = cards.id
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
 WHERE cards.session_id IS NULL
   AND EXISTS (
       SELECT 1
         FROM runtimes r
         JOIN worker_sessions ws ON ws.id = r.id
        WHERE r.card_id = cards.id
          AND ws.state != 'superseded'
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
