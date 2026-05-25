-- Issue #306 — Persist child exit info at the daemon → kernel boundary.
--
-- Two new columns on `terminals`, written exactly once by the kernel as
-- the daemon's child wait resolves, then read by the WS upgrade path
-- and by the REST card-details responses. Together they let the
-- frontend render an exit-code badge (warp-style) on the card header
-- without conflating "child exited cleanly" with "daemon socket gone".
--
-- Semantics:
--
--   * `exit_code IS NOT NULL` — daemon recorded a numeric exit code
--     (process returned via exit() or main return).
--
--   * `signal_killed = 1` — child was killed by a signal (SIGTERM,
--     SIGKILL, SIGSEGV, …). `exit_code` SHOULD be NULL in that case;
--     the two states are mutually exclusive at the writer.
--
--   * Both NULL/0 — either the daemon hasn't exited yet (terminal
--     still live), or the daemon died without writing the sidecar
--     file (the future "DaemonLost" state; not surfaced in v1).
--
-- This shape mirrors warp's `RemoteServerExitStatus { code, signal_killed }`
-- and zellij's `Option<i32>` exit code, both researched as prior art
-- before settling on this minimal schema.

ALTER TABLE terminals ADD COLUMN exit_code INTEGER;
ALTER TABLE terminals ADD COLUMN signal_killed INTEGER NOT NULL DEFAULT 0;
