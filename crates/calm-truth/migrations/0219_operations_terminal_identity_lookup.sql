-- Terminal identity is independent of the registry of Worker transports. Keep
-- disposal indexed when new native operation kinds join the same query.
DROP INDEX operations_terminal_lookup_idx;
CREATE INDEX operations_terminal_lookup_idx
ON operations(json_extract(tx_output_json,'$.data.terminal_id'), id)
WHERE target_type='card'
  AND json_extract(tx_output_json,'$.data.terminal_id') IS NOT NULL;
