-- Retirement reserves the logical selection without asserting that its physical
-- directory has stopped or been removed. Unconfined cleanup stays deferred.
ALTER TABLE workspace_resources ADD COLUMN location_root TEXT;
UPDATE workspace_resources SET location_root=coalesce(observed_root,root_path);
ALTER TABLE workspace_resources ADD COLUMN retirement_key TEXT;
ALTER TABLE workspace_resources ADD COLUMN retired_at_ms INTEGER;
ALTER TABLE workspace_resources ADD COLUMN retirement_root TEXT;
ALTER TABLE workspace_resources ADD COLUMN cleanup_reason TEXT
    CHECK (location_root IS NOT NULL AND ((retired_at_ms IS NULL AND cleanup_reason IS NULL)
        OR (retired_at_ms IS NOT NULL AND ownership = 'owned'
            AND retirement_root IS NOT NULL AND cleanup_reason IS NOT NULL)));
CREATE UNIQUE INDEX workspace_resources_retirement_key ON workspace_resources(retirement_key);
CREATE INDEX workspace_resources_retired ON workspace_resources(retired_at_ms)
    WHERE retired_at_ms IS NOT NULL;
