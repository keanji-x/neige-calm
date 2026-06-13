-- Sync engine event "home scope" — PR2 of #136.
--
-- Every event now declares the cove/wave/card it belongs to so future PRs can
--   * PR3: filter authorization decisions (enforce_role) by card scope,
--   * PR5: route SubscribeFilter / Dispatcher queues by wave scope,
--   * PR8: long-poll `wait_for_events` against a scoped cursor.
--
-- Schema:
--
--   scope_kind  TEXT NOT NULL DEFAULT 'system'   -- 'system' | 'cove' | 'wave' | 'card'
--   scope_cove  TEXT NULL                        -- populated for cove/wave/card scopes
--   scope_wave  TEXT NULL                        -- populated for wave/card scopes
--   scope_card  TEXT NULL                        -- populated for card scope
--
-- Old rows: every existing row backfills to `scope_kind = 'system'` with NULL
-- ancestor cols via the column default. **We do NOT best-effort backfill from
-- payload joins** — those joins are brittle (cards may have been deleted by
-- the time the migration runs, and the resulting joins would be silently
-- wrong rather than visibly NULL), and the WS-replay path falls back to
-- `EventScope::System` for any row with NULL `scope_*`. NULL-tolerant by
-- design.
--
-- Indexes are partial — only on non-NULL rows — so the index size stays
-- proportional to the post-PR2 events and doesn't bloat with NULLs from the
-- pre-PR2 history. `idx_events_scope_card` is intentionally omitted: PR5's
-- dispatcher routes by wave, not by card, and a per-card index would slow
-- inserts without a current consumer.

ALTER TABLE events ADD COLUMN scope_kind TEXT NOT NULL DEFAULT 'system';
ALTER TABLE events ADD COLUMN scope_cove TEXT;
ALTER TABLE events ADD COLUMN scope_wave TEXT;
ALTER TABLE events ADD COLUMN scope_card TEXT;
CREATE INDEX idx_events_scope_wave ON events(scope_wave) WHERE scope_wave IS NOT NULL;
CREATE INDEX idx_events_scope_cove ON events(scope_cove) WHERE scope_cove IS NOT NULL;
