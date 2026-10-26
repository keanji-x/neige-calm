-- Typed-terminal classification reads alias history, including retired executions.
CREATE INDEX workspace_executions_alias_history ON workspace_executions(alias);
