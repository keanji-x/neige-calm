-- Gate-result wire kind version re-stamp (issue #644 PR-C).
--
-- `task.gate_result` joins the WS event union together with the
-- `SYNC_EVENT_VERSION` 3 → 4 bump, so unlike migration 0043 (where
-- `plan.updated` / `task.dispatched` shipped BEFORE their bump) no rows
-- should exist below version 4. The re-stamp is kept anyway as a
-- defensive mirror of the 0043 procedure: if any build window ever
-- persisted the kind at an older stamp, a v3 client replaying it would
-- treat the frame as in-range, advance its replay cursor, then silently
-- fail zod on the unknown discriminator — permanently skipping the
-- gate-result invalidation. Re-stamping to 4 makes the per-frame
-- future-protocol gate (`web/src/api/events.ts`) drop such frames
-- without advancing the cursor.

UPDATE events
SET event_version = 4
WHERE kind = 'task.gate_result'
  AND event_version < 4;
