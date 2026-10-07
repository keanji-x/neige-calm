-- The Planner re-runs a failed gate on the same candidate with task.regate_requested (#2405).
-- Existing databases have no such rows; new events are stamped by the Rust constant.
UPDATE events SET event_version = 28 WHERE kind = 'task.regate_requested';
