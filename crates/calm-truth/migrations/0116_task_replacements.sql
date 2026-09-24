-- #1785 slice 2 — `calm.task.replace` receipts.
--
-- One row per accepted replacement: the Planner stopped (or found terminal) one predecessor
-- attempt and the kernel appended a successor declaration `<root>.<n>` in the same transaction.
-- The row is the replay answer (`prior_status`, `stop`, the carry source or why there is none)
-- and the carry plan a successor lease prepare reads by `(track_id, successor_key)`. The
-- candidate the carry names is the immutable `task_candidates` row; nothing is copied from it.
-- At most one successor per predecessor attempt, one receipt per request key, one receipt per
-- successor key. Rows never change.
CREATE TABLE task_replacements (
  receipt_id TEXT PRIMARY KEY NOT NULL CHECK (length(receipt_id) > 0),
  track_id TEXT NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
  predecessor_attempt_id TEXT NOT NULL UNIQUE CHECK (length(predecessor_attempt_id) > 0),
  predecessor_key TEXT NOT NULL CHECK (length(predecessor_key) > 0),
  successor_key TEXT NOT NULL CHECK (length(successor_key) > 0),
  request_idempotency_key TEXT NOT NULL CHECK (length(trim(request_idempotency_key)) > 0),
  request_fingerprint TEXT NOT NULL CHECK (length(request_fingerprint) > 0),
  reason TEXT NOT NULL CHECK (length(trim(reason)) > 0),
  prior_status TEXT NOT NULL
    CHECK (prior_status IN ('pending','running','done','failed','canceled')),
  stop TEXT NOT NULL CHECK (stop IN ('canceled_now','already_terminal')),
  source_attempt_id TEXT NULL,
  source_candidate_id TEXT NULL,
  carry_none_reason TEXT NULL,
  created_at_ms INTEGER NOT NULL,
  UNIQUE(track_id, request_idempotency_key),
  UNIQUE(track_id, successor_key),
  CHECK ((stop = 'canceled_now') = (prior_status IN ('pending','running'))),
  CHECK ((source_attempt_id IS NOT NULL AND source_candidate_id IS NOT NULL
          AND carry_none_reason IS NULL)
      OR (source_attempt_id IS NULL AND source_candidate_id IS NULL
          AND carry_none_reason IN ('requested_none','no_candidate')))
);
CREATE TRIGGER task_replacement_immutable BEFORE UPDATE ON task_replacements
BEGIN SELECT RAISE(ABORT, 'task replacement receipts are immutable'); END;
