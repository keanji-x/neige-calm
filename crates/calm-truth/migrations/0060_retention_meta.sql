-- #854 slice 2 — durable bookkeeping for the events retention pruner.
--
-- One row today: `events_prune_watermark` = the highest `events.id` the
-- pruner has EVER deleted. The WS replay guard (`ws::events::run_replay`)
-- sends `_snapshot_required` to any client whose cursor is below this
-- watermark, because a pruned row may sit anywhere in `(since, watermark]`
-- — interior holes that the `MIN(id)` check alone can never detect
-- (structural events are permanent, so `MIN(id)` never advances past the
-- first structural row).
--
-- Kept separate from `settings` (user-facing KV the Settings page writes
-- through) — this is kernel-internal state, INTEGER-typed, and must never
-- surface in the settings API.
CREATE TABLE retention_meta (
    key   TEXT    PRIMARY KEY,
    value INTEGER NOT NULL
);
