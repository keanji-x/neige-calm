-- Change the selected run while task-verify is observing refs outside its transaction.
UPDATE operations
SET tx_output_json = json_set(tx_output_json, '$.result.verdict.passed', json('false'))
WHERE kind = ?1 AND idempotency_key = ?2
