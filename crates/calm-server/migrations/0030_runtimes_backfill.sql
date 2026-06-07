-- Backfill runtime rows for live cards that predate the dedicated runtimes
-- table. Each pass is idempotent so test resets and partially-upgraded DBs
-- can safely re-run the migration body.

-- PR2a addendum: complete stale-active runtime rows whose backing terminal
-- has already exited. Covers the "deploy PR1, terminal exits, then upgrade
-- to PR2a" path where PR1 dual-write left rows in `starting` but no PR1
-- completion path existed yet. Signalled exits -> 'failed'; clean exits ->
-- 'exited'. Limited to terminal-backed runtimes.
UPDATE runtimes
SET
  status = CASE
    WHEN (SELECT COALESCE(t.signal_killed, 0) FROM terminals t WHERE t.id = runtimes.terminal_run_id) = 1
      THEN 'failed'
    ELSE 'exited'
  END,
  updated_at_ms = CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER),
  completed_at_ms = CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)
WHERE
  terminal_run_id IS NOT NULL
  AND status IN ('starting', 'running', 'idle', 'turn_pending')
  AND EXISTS (
    SELECT 1 FROM terminals t
    WHERE t.id = runtimes.terminal_run_id
      AND (t.exit_code IS NOT NULL OR COALESCE(t.signal_killed, 0) = 1)
  );

INSERT INTO runtimes (
  id, card_id, kind, agent_provider, status, terminal_run_id,
  thread_id, session_id, active_turn_id, handle_state_json,
  lease_owner, lease_until_ms, created_at_ms, updated_at_ms,
  completed_at_ms
)
SELECT
  lower(hex(randomblob(16))),
  c.id,
  'terminal',
  NULL,
  'running',
  t.id,
  NULL,
  NULL,
  NULL,
  NULL,
  NULL,
  NULL,
  CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER),
  CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER),
  NULL
FROM cards c
JOIN terminals t ON t.card_id = c.id
WHERE c.kind = 'terminal'
  AND t.exit_code IS NULL
  AND COALESCE(t.signal_killed, 0) = 0
  AND NOT EXISTS (
    SELECT 1 FROM runtimes r WHERE r.card_id = c.id
  );

INSERT INTO runtimes (
  id, card_id, kind, agent_provider, status, terminal_run_id,
  thread_id, session_id, active_turn_id, handle_state_json,
  lease_owner, lease_until_ms, created_at_ms, updated_at_ms,
  completed_at_ms
)
SELECT
  lower(hex(randomblob(16))),
  c.id,
  'codex',
  'codex',
  CASE WHEN ct.thread_id IS NULL THEN 'turn_pending' ELSE 'running' END,
  t.id,
  ct.thread_id,
  NULL,
  NULL,
  NULL,
  NULL,
  NULL,
  CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER),
  CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER),
  NULL
FROM cards c
LEFT JOIN card_codex_threads ct ON ct.card_id = c.id
JOIN terminals t ON t.card_id = c.id
WHERE c.kind = 'codex'
  AND c.role != 'spec'
  AND t.exit_code IS NULL
  AND COALESCE(t.signal_killed, 0) = 0
  AND NOT EXISTS (
    SELECT 1 FROM runtimes r WHERE r.card_id = c.id
  );

INSERT INTO runtimes (
  id, card_id, kind, agent_provider, status, terminal_run_id,
  thread_id, session_id, active_turn_id, handle_state_json,
  lease_owner, lease_until_ms, created_at_ms, updated_at_ms,
  completed_at_ms
)
SELECT
  lower(hex(randomblob(16))),
  c.id,
  'claude',
  'claude',
  'running',
  t.id,
  NULL,
  json_extract(c.payload, '$.claude_session_id'),
  NULL,
  NULL,
  NULL,
  NULL,
  CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER),
  CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER),
  NULL
FROM cards c
JOIN terminals t ON t.card_id = c.id
WHERE c.kind = 'claude'
  AND t.exit_code IS NULL
  AND COALESCE(t.signal_killed, 0) = 0
  AND NOT EXISTS (
    SELECT 1 FROM runtimes r WHERE r.card_id = c.id
  );

INSERT INTO runtimes (
  id, card_id, kind, agent_provider, status, terminal_run_id,
  thread_id, session_id, active_turn_id, handle_state_json,
  lease_owner, lease_until_ms, created_at_ms, updated_at_ms,
  completed_at_ms
)
SELECT
  lower(hex(randomblob(16))),
  c.id,
  'shared-spec',
  'codex',
  CASE WHEN ct.thread_id IS NULL THEN 'turn_pending' ELSE 'running' END,
  NULL,
  ct.thread_id,
  NULL,
  NULL,
  NULL,
  NULL,
  NULL,
  CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER),
  CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER),
  NULL
FROM cards c
LEFT JOIN card_codex_threads ct ON ct.card_id = c.id
WHERE c.kind = 'codex'
  AND c.role = 'spec'
  AND json_extract(c.payload, '$.codex_source') = 'shared'
  AND (
    ct.thread_id IS NOT NULL
    OR EXISTS (
      SELECT 1
      FROM terminals t
      WHERE t.card_id = c.id
        AND t.exit_code IS NULL
        AND COALESCE(t.signal_killed, 0) = 0
    )
  )
  AND NOT EXISTS (
    SELECT 1 FROM runtimes r WHERE r.card_id = c.id
  );
