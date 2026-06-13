-- Dispatcher request event rename (issue #581).
--
-- `codex.job_requested` / `terminal.job_requested` → `*.worker_requested`.
-- The kind string is the wire-level discriminator that web clients gate
-- on; rewriting these rows in place lets the dispatcher subscribe
-- (`crates/calm-server/src/dispatcher.rs`) and the wave-file projector
-- (`crates/calm-server/src/wave_fs_view.rs`) read a single canonical
-- name without scattering legacy-alias filters across every reader.
--
-- We also bump `event_version` to 2 on the same rows. A v1 client
-- replaying these rewritten rows would otherwise treat them as in-range
-- (eventVersion=1 ≤ its cached gate of 1), advance its replay cursor,
-- then silently fail zod on the new discriminator. Bumping the version
-- makes the per-frame future-protocol gate
-- (`web/src/api/events.ts` "Per-frame eventVersion gate") drop them
-- without advancing — preserving the cursor for after the user
-- refreshes onto the matching v3 bundle.

UPDATE events
SET kind = 'codex.worker_requested', event_version = 2
WHERE kind = 'codex.job_requested';

UPDATE events
SET kind = 'terminal.worker_requested', event_version = 2
WHERE kind = 'terminal.job_requested';
