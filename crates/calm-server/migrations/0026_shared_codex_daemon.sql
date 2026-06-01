CREATE TABLE shared_codex_daemon (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    state TEXT NOT NULL CHECK (state IN ('idle', 'starting', 'running', 'restarting', 'failed')),
    pid INTEGER,
    pgid INTEGER,
    sock_path TEXT,
    codex_home_path TEXT,
    process_start_time INTEGER,
    boot_id TEXT,
    started_at INTEGER,
    updated_at INTEGER NOT NULL,
    restart_count INTEGER NOT NULL DEFAULT 0,
    last_error TEXT
);

INSERT INTO shared_codex_daemon (id, state, updated_at, restart_count)
VALUES (1, 'idle', strftime('%s','now') * 1000, 0);
