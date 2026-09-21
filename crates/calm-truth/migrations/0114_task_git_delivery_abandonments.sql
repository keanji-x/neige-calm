-- #1727 S4 slice 3 — the Planner's abandonment of a failed Git delivery.
--
-- One row per `calm.task.delivery{action:"abandon"}`, keyed by the delivery it gives up
-- (`delivery_id` PK: "abandoning an abandoned delivery" cannot exist). `task_outcome` is what the
-- abandonment transaction did to the tasks row — `failed` (a gated `verifying` row was flipped and
-- the budget released), `done_unchanged` (an ungated `done` row was left alone), `already_terminal`
-- (a gated row the gate had already flipped; nothing was written) — and `task_status` the row
-- status observed in that same transaction. The implication CHECK is NULL-safe in form even though
-- both columns are NOT NULL (the deliveries table's settlement CHECK is the precedent).
--
-- Both foreign keys cascade: Track and Area deletion issue one `DELETE FROM tracks` and check the
-- FKs at statement end, so a non-cascading `delivery_id` FK would fail the whole delete
-- (D2 cascade list). `UNIQUE(producer_attempt_id, request_idempotency_key)` is the uniqueness of
-- the request key; the action's replay lookup adds the caller's `track_id`.
CREATE TABLE task_git_delivery_abandonments (
  delivery_id TEXT PRIMARY KEY NOT NULL REFERENCES task_git_deliveries(delivery_id) ON DELETE CASCADE,
  track_id TEXT NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
  producer_attempt_id TEXT NOT NULL,
  request_idempotency_key TEXT NOT NULL,
  reason TEXT NULL,
  task_outcome TEXT NOT NULL CHECK (task_outcome IN ('failed','done_unchanged','already_terminal')),
  task_status TEXT NOT NULL CHECK (task_status IN ('done','failed')),
  created_at_ms INTEGER NOT NULL,
  UNIQUE(producer_attempt_id, request_idempotency_key),
  CHECK ((task_outcome IS NOT NULL AND task_outcome = 'failed' AND task_status IS NOT NULL AND task_status = 'failed') OR (task_outcome IS NOT NULL AND task_outcome = 'done_unchanged' AND task_status IS NOT NULL AND task_status = 'done') OR (task_outcome IS NOT NULL AND task_outcome = 'already_terminal' AND task_status IS NOT NULL AND task_status IN ('done','failed')))
);
CREATE TRIGGER task_git_delivery_abandonment_immutable BEFORE UPDATE ON task_git_delivery_abandonments
BEGIN SELECT RAISE(ABORT, 'git delivery abandonment is immutable'); END;
