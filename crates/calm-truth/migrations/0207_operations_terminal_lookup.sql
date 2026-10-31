-- Legacy terminal disposal must seek one identity without parsing worker history
-- while holding the writer. Keep every phase: even terminal phases may retain
-- unresolved launch obligations. New pipe operations have no terminal identity.
CREATE INDEX operations_terminal_lookup_idx
ON operations(json_extract(tx_output_json,'$.data.terminal_id'), id)
WHERE target_type='card'
  AND kind IN ('terminal-worker','claude-worker','codex-worker')
  AND json_extract(tx_output_json,'$.data.terminal_id') IS NOT NULL;
