-- Issue #760 slice ④-a — optional workflow binding for waves.
--
-- Nullable and non-destructive: old waves remain unbound, and create-time
-- route validation owns whether a supplied id is currently registered by a
-- trusted plugin.
ALTER TABLE waves ADD COLUMN workflow_id TEXT NULL;
