-- Scheduler inspection reminders are separate from execution and lease authority.
CREATE TABLE task_followups (
    task_id TEXT PRIMARY KEY REFERENCES tasks(id) ON DELETE CASCADE,
    phase TEXT NOT NULL,
    phase_updated_at_ms INTEGER NOT NULL,
    gate_attempt INTEGER NOT NULL,
    notice_id TEXT NOT NULL,
    next_review_at_ms INTEGER NOT NULL,
    decision_json TEXT
);
