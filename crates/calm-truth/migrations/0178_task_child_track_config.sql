-- Child planner overrides follow the pending declaration and freeze with the claimed task.
-- Existing rows retain inheritance; operation outputs freeze the merged creation selection.
ALTER TABLE tasks ADD COLUMN child_track_json TEXT;
