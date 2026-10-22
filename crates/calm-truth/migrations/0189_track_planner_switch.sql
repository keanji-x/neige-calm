-- Planner tenure belongs to the Track, independently of carrier rotations.
ALTER TABLE tracks ADD COLUMN planner_revision INTEGER NOT NULL DEFAULT 0 CHECK(planner_revision >= 0);
ALTER TABLE tracks ADD COLUMN planner_event_baseline INTEGER NOT NULL DEFAULT 0 CHECK(planner_event_baseline >= 0);

-- Immutable switching facts. No conversation content, queues or credentials.
CREATE TABLE track_planner_changes (
    track_id TEXT NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    revision INTEGER NOT NULL CHECK(revision > 0),
    user_id TEXT NOT NULL REFERENCES application_users(id),
    from_card_id TEXT NOT NULL,
    to_card_id TEXT NOT NULL,
    from_old_worker TEXT NOT NULL,
    from_new_worker TEXT NOT NULL,
    to_old_worker TEXT NOT NULL,
    to_new_worker TEXT NOT NULL,
    from_old_binding TEXT NOT NULL,
    from_new_binding TEXT NOT NULL,
    to_old_binding TEXT NOT NULL,
    to_new_binding TEXT NOT NULL,
    from_watermark INTEGER NOT NULL,
    to_watermark INTEGER NOT NULL,
    from_history_boundary INTEGER NOT NULL,
    to_history_boundary INTEGER NOT NULL,
    event_baseline INTEGER NOT NULL CHECK(event_baseline >= 0),
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY(track_id, revision)
);
