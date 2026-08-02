-- Issue #985 PR3b-i — task projection attribution and per-wave policy.
ALTER TABLE tasks ADD COLUMN declared_by TEXT NULL;
ALTER TABLE tasks ADD COLUMN origin TEXT NULL;

-- These policy columns deliberately stay off the Wave row model. NULL means
-- the kernel default; new waves start with the conservative ceiling of 32.
ALTER TABLE waves ADD COLUMN spec_task_ceiling INTEGER NULL DEFAULT 32;
ALTER TABLE waves ADD COLUMN automation_policy TEXT NULL;
