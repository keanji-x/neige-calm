CREATE TABLE runtimes (
  id TEXT PRIMARY KEY,
  card_id TEXT NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
  kind TEXT NOT NULL CHECK (kind IN (
    'terminal',
    'codex',
    'claude',
    'shared-spec'
  )),
  agent_provider TEXT NULL,
  status TEXT NOT NULL CHECK (status IN (
    'starting',
    'running',
    'idle',
    'turn_pending',
    'failed',
    'exited',
    'superseded'
  )),
  terminal_run_id TEXT NULL REFERENCES terminals(id) ON DELETE SET NULL,
  thread_id TEXT NULL,
  session_id TEXT NULL,
  active_turn_id TEXT NULL,
  handle_state_json TEXT NULL CHECK (
    handle_state_json IS NULL OR json_valid(handle_state_json)
  ),
  lease_owner TEXT NULL,
  lease_until_ms INTEGER NULL,
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL,
  completed_at_ms INTEGER NULL,
  CHECK (updated_at_ms >= created_at_ms),
  CHECK (completed_at_ms IS NULL OR completed_at_ms >= created_at_ms),
  CHECK (
    (lease_owner IS NULL AND lease_until_ms IS NULL)
    OR (lease_owner IS NOT NULL AND lease_until_ms IS NOT NULL)
  ),
  CHECK (
    (kind = 'terminal' AND agent_provider IS NULL) OR
    (kind = 'codex' AND agent_provider IS NOT NULL AND agent_provider = 'codex') OR
    (kind = 'claude' AND agent_provider IS NOT NULL AND agent_provider = 'claude') OR
    (kind = 'shared-spec' AND agent_provider IS NOT NULL AND agent_provider = 'codex')
  )
);

CREATE INDEX runtimes_active_per_card_idx
  ON runtimes(card_id, status, updated_at_ms)
  WHERE status IN (
    'starting',
    'running',
    'idle',
    'turn_pending'
  );

CREATE UNIQUE INDEX runtimes_one_active_per_card
  ON runtimes(card_id)
  WHERE status IN (
    'starting',
    'running',
    'idle',
    'turn_pending'
  );

CREATE INDEX runtimes_terminal_run_idx
  ON runtimes(terminal_run_id)
  WHERE terminal_run_id IS NOT NULL;

CREATE INDEX runtimes_thread_idx
  ON runtimes(agent_provider, thread_id)
  WHERE thread_id IS NOT NULL;

CREATE INDEX runtimes_session_idx
  ON runtimes(agent_provider, session_id)
  WHERE session_id IS NOT NULL;

CREATE INDEX runtimes_recover_scan_idx
  ON runtimes(status, lease_until_ms, updated_at_ms)
  WHERE status IN (
    'starting',
    'running',
    'idle',
    'turn_pending'
  );
