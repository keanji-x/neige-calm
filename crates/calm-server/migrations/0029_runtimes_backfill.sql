-- Backfill runtime rows for live cards that predate the dedicated runtimes
-- table. Each pass is idempotent so test resets and partially-upgraded DBs
-- can safely re-run the migration body.

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
  'starting',
  t.id,
  NULL,
  NULL,
  NULL,
  NULL,
  NULL,
  NULL,
  CAST(strftime('%s','now') AS INTEGER) * 1000,
  CAST(strftime('%s','now') AS INTEGER) * 1000,
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
  'running',
  t.id,
  ct.thread_id,
  NULL,
  NULL,
  NULL,
  NULL,
  NULL,
  CAST(strftime('%s','now') AS INTEGER) * 1000,
  CAST(strftime('%s','now') AS INTEGER) * 1000,
  NULL
FROM card_codex_threads ct
JOIN cards c ON c.id = ct.card_id
JOIN terminals t ON t.card_id = c.id
WHERE c.kind = 'codex'
  AND COALESCE(json_extract(c.payload, '$.codex_source'), 'legacy') != 'shared'
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
  'starting',
  t.id,
  NULL,
  json_extract(c.payload, '$.claude_session_id'),
  NULL,
  NULL,
  NULL,
  NULL,
  CAST(strftime('%s','now') AS INTEGER) * 1000,
  CAST(strftime('%s','now') AS INTEGER) * 1000,
  NULL
FROM cards c
JOIN terminals t ON t.card_id = c.id
WHERE c.kind = 'claude'
  AND json_extract(c.payload, '$.claude_session_id') IS NOT NULL
  AND trim(json_extract(c.payload, '$.claude_session_id')) != ''
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
  CAST(strftime('%s','now') AS INTEGER) * 1000,
  CAST(strftime('%s','now') AS INTEGER) * 1000,
  NULL
FROM cards c
LEFT JOIN card_codex_threads ct ON ct.card_id = c.id
WHERE c.kind = 'codex'
  AND c.role = 'spec'
  AND json_extract(c.payload, '$.codex_source') = 'shared'
  AND NOT EXISTS (
    SELECT 1 FROM runtimes r WHERE r.card_id = c.id
  );
