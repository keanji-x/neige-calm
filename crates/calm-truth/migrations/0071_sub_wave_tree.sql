-- #985 slice 6 PR-A. This migration is deliberately additive: rebuilding
-- either referenced table can fire ON DELETE actions and would also require
-- reproducing every historical partial index and CHECK constraint.
ALTER TABLE waves ADD COLUMN parent_wave_id TEXT NULL REFERENCES waves(id)
  CHECK (parent_wave_id IS NULL OR parent_wave_id <> id);

CREATE INDEX idx_waves_parent_wave_id
  ON waves(parent_wave_id) WHERE parent_wave_id IS NOT NULL;

ALTER TABLE tasks ADD COLUMN spawn TEXT NOT NULL DEFAULT 'in-wave';
-- `spawn` is claim-frozen input and therefore belongs in TASK_COLUMNS.
ALTER TABLE tasks ADD COLUMN child_wave_id TEXT NULL;
-- `child_wave_id` is stamped after claim (#1030 exception) and intentionally
-- stays out of TASK_COLUMNS; its readers are directional.
CREATE UNIQUE INDEX idx_tasks_child_wave_id
  ON tasks(child_wave_id) WHERE child_wave_id IS NOT NULL;

-- Spell out the backfill instead of relying only on the ADD COLUMN default.
UPDATE tasks SET spawn = 'in-wave' WHERE spawn IS NULL OR spawn = '';
