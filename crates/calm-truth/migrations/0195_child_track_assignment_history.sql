-- Returned children retain every parent assignment. Only live bindings are exclusive;
-- task_bind_child_track_tx enforces that invariant under the writer transaction.
DROP INDEX idx_tasks_child_track_id;
CREATE INDEX idx_tasks_child_track_id ON tasks(child_track_id) WHERE child_track_id IS NOT NULL;
