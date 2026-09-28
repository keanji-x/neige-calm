-- #1830 S2 — how the attempt a delivery commits ended. Every attempt (completed, failed or
-- stopped) is committed in its track's checkout; the release that ends the attempt writes the
-- first delivery row with this value, and a retry row copies its predecessor's. The column is
-- nullable only because rows settled before this migration have none.
ALTER TABLE task_git_deliveries ADD COLUMN outcome TEXT NULL
  CHECK (outcome IS NULL OR outcome IN ('completed','failed','canceled','spawn-failed','interrupted'));

-- The 0113 immutability trigger names its columns, so it does not cover this one.
CREATE TRIGGER task_git_delivery_outcome_immutable BEFORE UPDATE OF outcome ON task_git_deliveries
BEGIN SELECT RAISE(ABORT, 'git delivery outcome is immutable'); END;
