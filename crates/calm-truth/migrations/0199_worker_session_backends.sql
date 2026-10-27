-- No backfill: historical sessions retain the implicit default backend.
CREATE TABLE worker_session_backends (
    worker_session_id TEXT PRIMARY KEY NOT NULL REFERENCES worker_sessions(id) ON DELETE CASCADE,
    provider TEXT NOT NULL,
    backend_id TEXT NOT NULL,
    FOREIGN KEY (provider, backend_id) REFERENCES agent_backend_registry(provider, backend_id)
);
CREATE TRIGGER worker_session_backends_no_update BEFORE UPDATE ON worker_session_backends
BEGIN SELECT RAISE(ABORT, 'session backend is immutable'); END;
CREATE TRIGGER worker_session_backends_insert BEFORE INSERT ON worker_session_backends
WHEN EXISTS (SELECT 1 FROM worker_session_backends WHERE worker_session_id = NEW.worker_session_id) OR NOT EXISTS (
    SELECT 1 FROM worker_sessions s WHERE s.id = NEW.worker_session_id
    AND s.provider = NEW.provider AND s.thread_id IS NULL
)
BEGIN SELECT RAISE(ABORT, 'session backend must precede thread creation and match provider'); END;
CREATE TRIGGER worker_session_backend_thread BEFORE UPDATE OF thread_id, provider ON worker_sessions
WHEN (EXISTS (
    SELECT 1 FROM worker_session_backends b
    WHERE b.worker_session_id = NEW.id AND b.provider != NEW.provider
)) OR (NEW.provider = 'codex' AND NEW.thread_id IS NOT NULL AND EXISTS (
    SELECT 1 FROM worker_sessions other
    LEFT JOIN worker_session_backends ob ON ob.worker_session_id = other.id
    WHERE other.id != NEW.id AND other.provider = 'codex' AND other.thread_id = NEW.thread_id
    AND COALESCE(ob.backend_id, 'default') != COALESCE(
        (SELECT backend_id FROM worker_session_backends WHERE worker_session_id = NEW.id), 'default')
))
BEGIN SELECT RAISE(ABORT, 'codex thread cannot cross backend scopes'); END;
-- Direct insertion of an unpooled session must obey the same namespace fence.
CREATE TRIGGER worker_session_backend_thread_insert BEFORE INSERT ON worker_sessions
WHEN NEW.provider = 'codex' AND NEW.thread_id IS NOT NULL AND EXISTS (
    SELECT 1 FROM worker_sessions other
    JOIN worker_session_backends b ON b.worker_session_id = other.id
    WHERE other.provider = 'codex' AND other.thread_id = NEW.thread_id AND b.backend_id != 'default'
)
BEGIN SELECT RAISE(ABORT, 'codex thread cannot cross backend scopes'); END;
