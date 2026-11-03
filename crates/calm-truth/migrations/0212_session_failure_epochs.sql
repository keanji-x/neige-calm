-- Internal immutable failure-episode identity; snapshots/timestamps are mutable.
-- No task policy belongs here: all session state writers share this fact.
ALTER TABLE worker_sessions ADD COLUMN failure_epoch INTEGER NOT NULL DEFAULT 0
    CHECK (typeof(failure_epoch) = 'integer' AND failure_epoch >= 0);
UPDATE worker_sessions SET failure_epoch = 1 WHERE state = 'failed';

CREATE TRIGGER worker_session_failure_epoch_insert AFTER INSERT ON worker_sessions
WHEN NEW.state = 'failed'
BEGIN
    UPDATE worker_sessions SET failure_epoch = 1 WHERE id = NEW.id;
END;

CREATE TRIGGER worker_session_failure_epoch_transition AFTER UPDATE OF state ON worker_sessions
WHEN OLD.state != 'failed' AND NEW.state = 'failed'
BEGIN
    UPDATE worker_sessions SET failure_epoch = OLD.failure_epoch + 1 WHERE id = NEW.id;
END;
