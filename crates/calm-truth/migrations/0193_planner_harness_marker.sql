-- The browser Planner marker follows the persisted role for conversation cards.
UPDATE cards SET payload = json_remove(payload, '$.planner_harness')
WHERE kind = 'codex' AND role <> 'planner';

UPDATE cards SET payload = json_set(payload, '$.planner_harness', json('true'))
WHERE kind = 'codex' AND role = 'planner';
