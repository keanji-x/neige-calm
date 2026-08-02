-- Issue #985 PR3b-i — task projection attribution and per-wave policy.
ALTER TABLE tasks ADD COLUMN declared_by TEXT NOT NULL DEFAULT 'spec';
ALTER TABLE tasks ADD COLUMN origin TEXT NOT NULL DEFAULT 'legacy';

-- Explicit backfill mirrors the claim_context_json migration shape and keeps
-- the data transition visible independently of SQLite's ADD COLUMN behavior.
UPDATE tasks SET declared_by = 'spec' WHERE declared_by IS NULL;
UPDATE tasks SET origin = 'legacy' WHERE origin IS NULL;

-- These policy columns deliberately stay off the Wave row model. NULL means
-- the kernel default; new waves start with the conservative ceiling of 32.
ALTER TABLE waves ADD COLUMN spec_task_ceiling INTEGER NULL DEFAULT 32;
ALTER TABLE waves ADD COLUMN automation_policy TEXT NULL;
