-- #1917 — a codex or claude task may declare `access: "read_only"`: it shares the track's
-- checkout with other read-only tasks instead of waiting for them.
--
-- `tasks.access` is the declared access, frozen with the row like `spawn`.
-- `workspace_leases.access_mode` is the access of the attempt that took the lease. A read-only
-- lease records no base and no `delivery_policy`, so its release writes no delivery row.
-- Every existing row takes `read_write`, so nothing changes for it.
--
-- `workspace_leases_active_path_idx` keeps one active writer lease per path; read-only leases
-- of the same path may be held beside each other. `current_tasks` selects `t.*`, which SQLite
-- expands when a statement is prepared, so the view needs no change.
ALTER TABLE tasks ADD COLUMN access TEXT NOT NULL DEFAULT 'read_write'
  CHECK (access IN ('read_only','read_write'));
ALTER TABLE workspace_leases ADD COLUMN access_mode TEXT NOT NULL DEFAULT 'read_write'
  CHECK (access_mode IN ('read_only','read_write'));
DROP INDEX workspace_leases_active_path_idx;
CREATE UNIQUE INDEX workspace_leases_active_path_idx
  ON workspace_leases(path)
  WHERE state IN ('held','releasing') AND access_mode = 'read_write';
