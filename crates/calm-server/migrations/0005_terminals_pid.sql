-- Scope C: orphan terminal cleanup. The sweeper needs a SIGTERM target
-- after a graceful ClientMsg::Kill fails; persist the daemon PID alongside
-- the existing row so the cleanup path can fall back to `nix::sys::signal::kill`
-- without holding an in-memory handle to the spawned `tokio::process::Child`.
--
-- Existing rows pre-Scope-C land with NULL (no PID was ever captured); the
-- sweeper handles NULL by skipping SIGTERM and going straight to socket-Kill
-- + row delete.
ALTER TABLE terminals ADD COLUMN pid INTEGER;
