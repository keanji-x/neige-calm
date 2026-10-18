-- Execution intent survives domain deletion; deliberately no foreign keys.
CREATE TABLE workspace_executions (
    execution_id TEXT PRIMARY KEY,
    track_id TEXT NOT NULL,
    alias TEXT NOT NULL,
    intent_json TEXT NOT NULL CHECK (json_valid(intent_json)),
    secrets_json TEXT CHECK (secrets_json IS NULL OR json_valid(secrets_json)),
    desired TEXT NOT NULL CHECK (desired IN ('run', 'stop', 'retire')),
    settled_at_ms INTEGER,
    settlement_json TEXT CHECK (settlement_json IS NULL OR json_valid(settlement_json)),
    CHECK ((settled_at_ms IS NULL) = (settlement_json IS NULL)),
    CHECK (secrets_json IS NOT NULL OR settled_at_ms IS NOT NULL)
);
CREATE UNIQUE INDEX workspace_executions_current_alias
    ON workspace_executions(alias) WHERE desired <> 'retire';
CREATE INDEX workspace_executions_unsettled
    ON workspace_executions(track_id) WHERE settled_at_ms IS NULL;

CREATE TRIGGER workspace_executions_immutable
BEFORE UPDATE ON workspace_executions
WHEN NEW.execution_id IS NOT OLD.execution_id
  OR NEW.track_id IS NOT OLD.track_id
  OR NEW.alias IS NOT OLD.alias
  OR NEW.intent_json IS NOT OLD.intent_json
BEGIN
    SELECT RAISE(ABORT, 'immutable execution intent');
END;

CREATE TRIGGER workspace_executions_desired_monotonic
BEFORE UPDATE ON workspace_executions
WHEN (OLD.desired = 'retire' AND NEW.desired <> 'retire')
  OR (OLD.desired = 'stop' AND NEW.desired = 'run')
BEGIN
    SELECT RAISE(ABORT, 'execution desire cannot decrease');
END;

CREATE TRIGGER workspace_executions_settlement_once
BEFORE UPDATE ON workspace_executions
WHEN OLD.settled_at_ms IS NOT NULL
 AND (NEW.settled_at_ms IS NOT OLD.settled_at_ms
      OR NEW.settlement_json IS NOT OLD.settlement_json)
BEGIN
    SELECT RAISE(ABORT, 'execution settlement is write-once');
END;

CREATE TRIGGER workspace_executions_secrets_clear_only
BEFORE UPDATE ON workspace_executions
WHEN NEW.secrets_json IS NOT OLD.secrets_json AND NEW.secrets_json IS NOT NULL
BEGIN
    SELECT RAISE(ABORT, 'execution secrets may only be cleared');
END;
