ALTER TABLE tasks ADD COLUMN running_deadline_ms INTEGER;

CREATE INDEX idx_tasks_liveness_deadlines
  ON tasks (running_deadline_ms)
  WHERE status = 'running';
