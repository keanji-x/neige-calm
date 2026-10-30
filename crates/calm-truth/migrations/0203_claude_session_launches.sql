-- A Claude `--session-id` launch may create its native session before any init bind
-- commits. The launch is recorded before admission; the bind or an init-free settled end
-- retires it. No backfill: earlier launches remain unrecorded.
CREATE TABLE claude_session_launches (
    worker_session_id TEXT PRIMARY KEY NOT NULL REFERENCES worker_sessions(id) ON DELETE CASCADE,
    card_id TEXT NOT NULL,
    track_id TEXT NOT NULL,
    thread_id TEXT NOT NULL,
    execution_id TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL
);
