-- #1501: retain legacy parked-resource evidence and admit the isolated kind's own receipt.
-- Only a CHECK changes. Keep every row/column/index and the permanent keyed-row fence.
-- FK enforcement remains ON: detach only the nullable references during this transaction.
CREATE TEMP TABLE operation_session_refs_0099 AS
SELECT id, spawn_op_id FROM worker_sessions WHERE spawn_op_id IS NOT NULL;
UPDATE worker_sessions SET spawn_op_id = NULL WHERE spawn_op_id IS NOT NULL;

CREATE TABLE operations_0099 (
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
    'parked',
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
  completed_at_ms INTEGER NULL,
  spawn_artifacts_json TEXT NULL CHECK (
    spawn_artifacts_json IS NULL OR json_valid(spawn_artifacts_json)
  ),
  parked_at_ms INTEGER NULL,
  parked_deadline_ms INTEGER NULL,
  CHECK (phase <> 'parked' OR (
    parked_at_ms IS NOT NULL
    AND parked_deadline_ms IS NOT NULL
    AND (
      (kind <> 'codex-isolated-worker' AND spawn_artifacts_json IS NOT NULL)
      OR (kind = 'codex-isolated-worker' AND spawn_artifacts_json IS NULL AND COALESCE(
        target_type = 'card'
        AND target_id IS NOT NULL
        AND idempotency_key IS NOT NULL
        AND json_extract(tx_output_json, '$.data.isolated_execution.version') = 'isolated-run-v1'
        AND json_extract(tx_output_json, '$.data.isolated_execution.request.identity.run_id') = id
        AND json_extract(tx_output_json, '$.data.isolated_execution.request.identity.attempt_id') = idempotency_key
        AND json_extract(tx_output_json, '$.data.isolated_execution.request.identity.card_id') = target_id
        AND json_extract(tx_output_json, '$.data.isolated_execution.provider.state') = 'prepared'
        AND json_extract(tx_output_json, '$.data.isolated_execution.provider.record.endpoint.version') = 2
        AND json_extract(tx_output_json, '$.data.isolated_execution.provider.record.endpoint.boundary.attempt_id') = idempotency_key
        AND json_type(tx_output_json, '$.data.isolated_execution.provider.record.phase.TurnActive') = 'object',
        0
      ))
    )
  ))
);

INSERT INTO operations_0099 SELECT * FROM operations;
DROP TABLE operations;
ALTER TABLE operations_0099 RENAME TO operations;

UPDATE worker_sessions SET spawn_op_id = (
  SELECT saved.spawn_op_id FROM operation_session_refs_0099 saved WHERE saved.id = worker_sessions.id
) WHERE id IN (SELECT id FROM operation_session_refs_0099);
DROP TABLE operation_session_refs_0099;

CREATE UNIQUE INDEX operations_kind_idempotency_key_unique
  ON operations(kind, idempotency_key)
  WHERE idempotency_key IS NOT NULL;

CREATE INDEX operations_drive_scan_idx
  ON operations(phase, lease_until_ms, updated_at_ms);

CREATE INDEX operations_target_idx
  ON operations(kind, target_type, target_id);

CREATE TRIGGER operations_keyed_rows_are_permanent
BEFORE DELETE ON operations
WHEN OLD.idempotency_key IS NOT NULL
BEGIN
  SELECT RAISE(ABORT, 'refusing to delete an operations row that carries an idempotency_key: that row is the submit() dedup wall, and deleting it lets the next byte-identical retry re-run the operation and deliver its message a second time. A retention pass may delete rows WHERE idempotency_key IS NULL; keyed rows are permanent. See docs/design-1428-idempotency-retention.md section 3.');
END;
