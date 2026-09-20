-- SYNC_EVENT_VERSION bump for the new `task.git_delivery_settled` kind (candidate binding, slice two).
--
-- Rows are born stamped with the Rust constant, so this statement matches nothing on any
-- database that exists; it is here for the lockstep gate (rule two: every literal in the newest
-- stamping migration equals the constant) and to record, executably, the version this kind
-- lives at. The cost is the one every bump pays: an open tab freezes until reload.
UPDATE events SET event_version = 21 WHERE kind = 'task.git_delivery_settled';
