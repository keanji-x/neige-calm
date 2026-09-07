-- #1501: the persisted isolated-execution settlement hint adds a wire kind.
-- Existing clients must reload before consuming the new event version.
UPDATE events SET event_version = 18 WHERE kind = 'task.execution_settled';
