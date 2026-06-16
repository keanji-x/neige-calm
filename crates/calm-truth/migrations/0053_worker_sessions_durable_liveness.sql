-- #741 §1.2 — durable codex worker-liveness signal (schema substrate, inert).
--
-- Two push-fed, worker_sessions-ONLY columns (no `runtimes` mirror, like
-- `liveness`): the activity feeder (741-2) stamps them via
-- `session_record_activity_tx` without bumping `updated_at_ms`, so they carry
-- zero dual-write parity cost. Nullable, no default → NULL for existing rows.
ALTER TABLE worker_sessions ADD COLUMN last_activity_ms   INTEGER;
ALTER TABLE worker_sessions ADD COLUMN last_thread_status TEXT;  -- idle|active|waitingOnUserInput|waitingOnApproval|systemError|notLoaded
