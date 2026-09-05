-- #1449 — the marker that makes "an undelivered human sentence is handed to the
-- successor exactly once" a construction rather than an ordering argument.
--
-- A runtime that leaves the active set has its still-pending user messages
-- either inherited by its successor or harvested by the next mint; both paths
-- stamp this column in the SAME transaction that takes the queue, so a second
-- restart reads the stamp and takes nothing. NULL means "this queue has not
-- left the undelivered set yet".
ALTER TABLE worker_sessions ADD COLUMN queue_harvested_at_ms INTEGER;

-- Backfill, deliberately: a legacy `superseded` row may well carry a non-empty
-- `pending_queue` — that stranding IS #1449 — and without this the first
-- restart after the upgrade would replay a sentence from days ago into a brand
-- new thread. A stale replay is worse than a loss that already happened and is
-- already recorded. It also shrinks the steady-state scan set to almost nothing.
UPDATE worker_sessions
   SET queue_harvested_at_ms = 1788566400000
 WHERE queue_harvested_at_ms IS NULL;
