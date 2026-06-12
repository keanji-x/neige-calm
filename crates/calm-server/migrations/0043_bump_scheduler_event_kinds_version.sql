-- Scheduler wire kinds version re-stamp (issue #644).
--
-- `plan.updated` (PR-A, #656) and `task.dispatched` (PR-B) were added to
-- the WS event union while `SYNC_EVENT_VERSION` was still 2, so any rows
-- persisted by those builds carry `event_version = 2`. A v2 client
-- replaying them would treat them as in-range (eventVersion=2 ≤ its
-- cached gate of 2), advance its replay cursor, then silently fail zod
-- on the unknown discriminator — permanently skipping the plan/dispatch
-- invalidation. Re-stamping them to 3 makes the per-frame
-- future-protocol gate (`web/src/api/events.ts` "Per-frame eventVersion
-- gate") drop them without advancing — preserving the cursor for after
-- the user refreshes onto the matching v4 bundle. Mirrors migration
-- 0038's `event_version` bump for the #581 rename.

UPDATE events
SET event_version = 3
WHERE kind IN ('plan.updated', 'task.dispatched')
  AND event_version < 3;
