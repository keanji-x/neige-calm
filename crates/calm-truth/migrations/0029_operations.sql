CREATE TABLE operations (
  id TEXT PRIMARY KEY,
  operation_key TEXT NOT NULL UNIQUE,
  kind TEXT NOT NULL,
  idempotency_key TEXT NULL,
  payload_hash TEXT NOT NULL,
  target_type TEXT NOT NULL,
  target_id TEXT NULL,
  target_json TEXT NOT NULL CHECK (json_valid(target_json)),
  payload_json TEXT NOT NULL CHECK (json_valid(payload_json)),
  tx_output_json TEXT NULL CHECK (tx_output_json IS NULL OR json_valid(tx_output_json)),
  phase TEXT NOT NULL CHECK (phase IN (
    'pending',
    'tx_committed',
    'app_server_interact',
    'spawn_started',
    'spawn_succeeded',
    'succeeded',
    'compensating',
    'failed',
    'stuck'
  )),
  phase_detail_json TEXT NULL CHECK (phase_detail_json IS NULL OR json_valid(phase_detail_json)),
  attempt INTEGER NOT NULL DEFAULT 0,
  last_error TEXT NULL,
  compensation_state TEXT NULL CHECK (
    compensation_state IS NULL OR json_valid(compensation_state)
  ),
  lease_owner TEXT NULL,
  lease_until_ms INTEGER NULL,
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL,
  completed_at_ms INTEGER NULL
);

CREATE UNIQUE INDEX operations_kind_idempotency_key_unique
  ON operations(kind, idempotency_key)
  WHERE idempotency_key IS NOT NULL;

CREATE INDEX operations_drive_scan_idx
  ON operations(phase, lease_until_ms, updated_at_ms);

CREATE INDEX operations_target_idx
  ON operations(kind, target_type, target_id);
