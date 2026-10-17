-- Accepted authority/selection is immutable and separate from public operation output.
-- Zero explicitly identifies released operations which predate admission.
ALTER TABLE operations ADD COLUMN execution_admitted INTEGER NOT NULL DEFAULT 0
CHECK (execution_admitted IN (0, 1));

ALTER TABLE operations ADD COLUMN execution_request_json TEXT
CHECK (execution_request_json IS NULL OR json_valid(execution_request_json));

-- Released terminals have no invented operation provenance. New Terminal
-- owning prepare transactions bind their actual original launch explicitly.
ALTER TABLE terminals ADD COLUMN launch_operation_id TEXT REFERENCES operations(id) ON DELETE RESTRICT;
CREATE UNIQUE INDEX terminal_original_launch ON terminals(launch_operation_id)
WHERE launch_operation_id IS NOT NULL;

CREATE TRIGGER terminal_original_launch_is_immutable
BEFORE UPDATE OF launch_operation_id ON terminals
WHEN OLD.launch_operation_id IS NOT NULL AND NEW.launch_operation_id IS NOT OLD.launch_operation_id
BEGIN
    SELECT RAISE(ABORT, 'original terminal launch cannot be replaced');
END;

CREATE TRIGGER operation_execution_request_is_immutable
BEFORE UPDATE OF execution_request_json ON operations
WHEN OLD.execution_request_json IS NOT NULL AND NEW.execution_request_json IS NOT OLD.execution_request_json
BEGIN
    SELECT RAISE(ABORT, 'original executor request cannot be replaced');
END;

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

-- One workspace-owned source registration intent per logical selection. This
-- is neither an execution journal nor physical continuity reconstructed from
-- stored stat facts: reuse always obtains the executor's actual retained FDs.
CREATE TABLE workspace_execution_sources (
    workspace_id TEXT PRIMARY KEY REFERENCES workspace_resources(id) ON DELETE RESTRICT,
    selection_version INTEGER NOT NULL CHECK (selection_version > 0),
    registration_intent_json TEXT NOT NULL CHECK (json_valid(registration_intent_json))
);
