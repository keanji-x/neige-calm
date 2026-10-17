-- Accepted authority/selection is immutable and separate from public operation output.
-- Zero explicitly identifies released operations which predate admission.
ALTER TABLE operations ADD COLUMN execution_admitted INTEGER NOT NULL DEFAULT 0
CHECK (execution_admitted IN (0, 1));

CREATE TABLE operation_execution_grants (
    operation_id TEXT PRIMARY KEY REFERENCES operations(id) ON DELETE CASCADE,
    original_payload_hash TEXT NOT NULL,
    grant_id TEXT NOT NULL UNIQUE,
    grant_json TEXT NOT NULL CHECK (json_valid(grant_json))
);

CREATE TRIGGER operation_execution_admission_cannot_be_cleared
BEFORE UPDATE OF execution_admitted ON operations
WHEN OLD.execution_admitted = 1 AND NEW.execution_admitted <> 1
BEGIN
    SELECT RAISE(ABORT, 'original operation admission cannot be cleared');
END;

CREATE TRIGGER operation_execution_grant_is_immutable
BEFORE UPDATE ON operation_execution_grants
BEGIN
    SELECT RAISE(ABORT, 'original operation execution grant is immutable');
END;

CREATE INDEX operation_terminal_launch_subject
ON operations(json_extract(tx_output_json, '$.data.terminal_id'))
WHERE kind = 'terminal-create';
