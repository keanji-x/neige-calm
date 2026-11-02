-- One-shot persistent responsibility. Owners define relevance and the expiry action.
-- No FK: owner identifiers are opaque to this shared primitive.
CREATE TABLE progress_obligations (
    execution_id TEXT NOT NULL,
    purpose TEXT NOT NULL,
    deadline_at_ms INTEGER NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('pending', 'notified', 'released')),
    PRIMARY KEY (execution_id, purpose)
);
CREATE INDEX progress_obligations_due ON progress_obligations(purpose, deadline_at_ms)
    WHERE state = 'pending';
