-- Issue #985 PR3a-i — durable task-context freeze state and reverse lookup.
ALTER TABLE tasks ADD COLUMN claim_context_json TEXT NULL
  CHECK (claim_context_json IS NULL OR json_valid(claim_context_json));
ALTER TABLE tasks ADD COLUMN context_stale_at_ms INTEGER NULL;

-- Existing in-flight work predates context freezing.  Its context is known
-- to be the legacy empty set, not missing; preserve that distinction across
-- the first boot of the upgraded binary.
UPDATE tasks
SET claim_context_json = '[]'
WHERE status NOT IN ('done', 'failed', 'canceled');

CREATE TABLE task_ref_index (
  task_id       TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
  dst_wave_id   TEXT NOT NULL,
  block_id      TEXT NOT NULL,
  PRIMARY KEY (task_id, dst_wave_id, block_id)
);
CREATE INDEX task_ref_index_destination_idx
  ON task_ref_index(dst_wave_id, block_id);

-- Defensive wire-version restamp: no released build should have written
-- these kinds before SYNC_EVENT_VERSION 13, but preserve replay safety if a
-- prerelease build did.
UPDATE events
SET event_version = 13
WHERE kind IN ('task.context_frozen', 'task.context_advanced')
  AND event_version < 13;
