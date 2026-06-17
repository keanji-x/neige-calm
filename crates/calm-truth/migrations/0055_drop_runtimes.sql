-- IRREVERSIBLE — no rollback after this migration. PR9b retires runtimes.

-- 1. Defensive de-dup pre-step: resolve any in-flight double-active ws rows
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

-- 2. Structural double-spawn protection on the ws side.
CREATE UNIQUE INDEX ws_one_active_per_card
   ON worker_sessions(card_id)
WHERE state IN ('starting','running','idle','turn_pending');

-- 3. Drop runtimes table indexes by name (avoid ambiguity).
DROP INDEX IF EXISTS runtimes_active_per_card_idx;
DROP INDEX IF EXISTS runtimes_one_active_per_card;
DROP INDEX IF EXISTS runtimes_terminal_run_idx;
DROP INDEX IF EXISTS runtimes_thread_idx;
DROP INDEX IF EXISTS runtimes_session_idx;
DROP INDEX IF EXISTS runtimes_recover_scan_idx;

-- 4. Drop the table.
DROP TABLE runtimes;
