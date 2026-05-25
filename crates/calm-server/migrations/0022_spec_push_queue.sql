-- #318 INV-3 (R2-B1) — durable spec-push enqueue queue.
--
-- The dispatcher's `Inner::push_to_spec` returns `Ok(PushOutcome::Enqueued)`
-- when an observation is buffered mid-turn (codex silently drops a second
-- `turn/start` issued while a turn is active, so we must wait for
-- `turn/completed` and flush the queue then). Before this migration, the
-- queue lived only as `Arc<Mutex<VecDeque<QueuedObservation>>>` in
-- `spec_appserver.rs::PushQueue` — a kernel crash between `Ok(Enqueued)`
-- and the consumer task's `turn/completed`-triggered `flush_push_queue`
-- LOST the observation; only the events-log replay (gated by the
-- dispatcher cooperatively withholding the `push_watermark` on `Enqueued`,
-- PR #315 PR4 B1) re-delivered it on the next boot.
--
-- INV-3 says that's incidental durability — the queue must hold its OWN.
-- This table is the durable backing store: every successful
-- `push_observation` returning `Ok(Enqueued)` inserts a row BEFORE
-- returning, so a crash before the flush leaves a row a fresh process
-- can recover (boot-takeover's `register_and_catch_up` rehydrates the
-- in-memory queue from these rows).
--
-- Lifecycle:
--   * INSERT inside `SpecPusher::push_observation` (Enqueue arm),
--     persist-first. The returned row id is recorded in the in-memory
--     `QueuedObservation` so the consumer task's `flush_push_queue` can
--     DELETE the right rows after a successful coalesced `turn/start`.
--   * DELETE in `flush_push_queue` (and the `StartTurnNow` winner's
--     drain) once `turn/start` resolves successfully.
--   * `FOREIGN KEY (card_id) REFERENCES cards(id) ON DELETE CASCADE`
--     so deleting the spec card (e.g. wave teardown) auto-drops any
--     pending rows — symmetric with `terminals`, `card_mcp_tokens`,
--     etc.
--   * Boot replay: `register_and_catch_up` calls
--     `Repo::spec_card_queued_observations(card_id)` to read all
--     pending rows in id order and reinflates the in-memory queue
--     before the catch-up replay starts pushing.
--
-- `envelope_id` is the `events.id` from the originating push (the
-- field already stamped on the in-memory `QueuedObservation`). The
-- flush path coalesces drained items into one `turn/start` and
-- reports `max(envelope_id)` back to the dispatcher via the
-- `WatermarkSink` callback so the durable `push_watermark` advances
-- past every item the coalesced turn just delivered (#313 B1).
--
-- This is OUT-OF-SYNC-DOMAIN: server-private operational state
-- (like `terminal.pid`, `card.payload.push_watermark`, plugin tokens
-- KV). No `Event::*` variant — nothing on the bus subscribes to push
-- queue rows, the dispatcher's filter doesn't watch them, and the
-- repo writes route through `RepoOutOfDomain` (no `CardUpdated`
-- emitted on enqueue/dequeue).

CREATE TABLE spec_push_queue (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    card_id     TEXT    NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
    envelope_id INTEGER NOT NULL,
    text        TEXT    NOT NULL,
    enqueued_at INTEGER NOT NULL
);

-- FIFO retrieval per card: `WHERE card_id = ? ORDER BY id ASC`. The
-- primary key already orders by id globally; the composite index lets
-- per-card scans hit only the matching rows. Boot replay is the
-- read-heavy site (one scan per recovering spec card) — steady state
-- writes more than reads (every push that hits the Enqueue arm
-- inserts; flush deletes in batches).
CREATE INDEX idx_spec_push_queue_card_id_id
    ON spec_push_queue(card_id, id);
