-- Issue #388 Phase 3b — calm-server now runs the terminal renderer
-- in-process and talks directly to calm-proc-supervisor for PTY I/O.
--
-- The existing `daemon_handle` column is intentionally retained for
-- forward-compatible rollout and 3c cleanup; production code stops
-- writing it in this phase. The `exit_code` / `signal_killed` columns
-- already landed in migration 0020, so this migration is schema-stable
-- documentation for the phase boundary.

