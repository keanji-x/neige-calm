-- Planner messages are attempt-owned records; terminal input carries only their notice.
CREATE TABLE task_planner_messages (
    message_id TEXT PRIMARY KEY NOT NULL,
    attempt_id TEXT NOT NULL REFERENCES task_attempt_allocations(attempt_id) ON DELETE CASCADE,
    seq INTEGER NOT NULL CHECK(seq > 0),
    text TEXT NOT NULL CHECK(length(CAST(text AS BLOB)) BETWEEN 1 AND 8000),
    idempotency_key TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    read_at_ms INTEGER,
    UNIQUE(attempt_id, seq),
    UNIQUE(attempt_id, idempotency_key)
);
CREATE TRIGGER task_planner_messages_immutable
BEFORE UPDATE OF message_id, attempt_id, seq, text, idempotency_key, created_at_ms
ON task_planner_messages
BEGIN
    SELECT RAISE(ABORT, 'planner message records are immutable');
END;
-- Deleting the owning attempt (track/area deletion) removes its audit records.
CREATE TRIGGER task_planner_messages_no_delete
BEFORE DELETE ON task_planner_messages
WHEN EXISTS (SELECT 1 FROM task_attempt_allocations WHERE attempt_id = OLD.attempt_id)
BEGIN
    SELECT RAISE(ABORT, 'planner message records are append-only');
END;
CREATE TRIGGER task_planner_messages_read_once
BEFORE UPDATE OF read_at_ms ON task_planner_messages
WHEN OLD.read_at_ms IS NOT NULL OR NEW.read_at_ms IS NULL
BEGIN
    SELECT RAISE(ABORT, 'planner message read timestamp is write-once');
END;
