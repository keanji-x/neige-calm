-- Permanent credential-scope identity is separate from mutable process state.
CREATE TABLE agent_backend_registry (
    provider TEXT NOT NULL,
    backend_id TEXT NOT NULL,
    protocol TEXT NOT NULL,
    configured_home BLOB NOT NULL,
    canonical_home BLOB NOT NULL UNIQUE,
    PRIMARY KEY (provider, backend_id)
);
CREATE TRIGGER agent_backend_registry_no_update BEFORE UPDATE ON agent_backend_registry
BEGIN SELECT RAISE(ABORT, 'backend identity is immutable'); END;
CREATE TRIGGER agent_backend_registry_no_delete BEFORE DELETE ON agent_backend_registry
BEGIN SELECT RAISE(ABORT, 'backend identity is permanent'); END;

CREATE TABLE codex_backend_daemons (
    backend_id TEXT PRIMARY KEY NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('idle', 'starting', 'running', 'restarting', 'failed')),
    pid INTEGER,
    pgid INTEGER,
    sock_path BLOB,
    codex_home_path BLOB,
    paths_are_legacy_display INTEGER NOT NULL CHECK (paths_are_legacy_display IN (0, 1)),
    process_start_time INTEGER,
    boot_id TEXT,
    started_at INTEGER,
    updated_at INTEGER NOT NULL,
    restart_count INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    daemon_env_signature TEXT
);
INSERT INTO codex_backend_daemons
SELECT 'default', state, pid, pgid, CAST(sock_path AS BLOB), CAST(codex_home_path AS BLOB), 1, process_start_time,
       boot_id, started_at, updated_at, restart_count, last_error, daemon_env_signature
FROM shared_codex_daemon;
DROP TABLE shared_codex_daemon;
