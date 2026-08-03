-- #985 slice 3b'-ii. These three columns are intentionally absent from
-- TASK_COLUMNS / the public Task model. Readers stay directional:
-- decl_* -> evaluate_schedulability_tx/FrozenDeclarationRow;
-- context_verify_failures -> the context sweep's targeted SQL only.
ALTER TABLE tasks ADD COLUMN decl_ready INTEGER NOT NULL DEFAULT 0
  CHECK (decl_ready IN (0, 1));
ALTER TABLE tasks ADD COLUMN decl_released_by_user INTEGER NOT NULL DEFAULT 0
  CHECK (decl_released_by_user IN (0, 1));
ALTER TABLE tasks ADD COLUMN context_verify_failures INTEGER NOT NULL DEFAULT 0;

UPDATE tasks SET decl_ready=1
 WHERE status IN ('dispatched','running','verifying') AND origin='block' AND decl_ready=0;

-- decl_released_by_user intentionally remains 0: its frozen value cannot be
-- reconstructed from a tasks row. context_verify_failures likewise remains 0.
