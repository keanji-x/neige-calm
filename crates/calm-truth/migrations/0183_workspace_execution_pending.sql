-- Cover the complete recovery set, including settled Retire rows retaining secrets.
CREATE INDEX workspace_executions_pending ON workspace_executions(execution_id)
    WHERE settled_at_ms IS NULL OR (desired = 'retire' AND secrets_json IS NOT NULL);
