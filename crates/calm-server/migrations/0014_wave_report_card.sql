-- Issue #229 PR B — backfill one wave-report card per existing wave.
--
-- PR A landed `cards.deletable` + the `idx_cards_one_report_per_wave`
-- partial unique index + the `CardRole::ReportCard` variant but did not
-- mint any rows. This migration is the one-shot backfill: for every wave
-- that does not already have a report card, INSERT one with a stub
-- payload + `deletable = 0`. Idempotent — re-running on a DB where some
-- waves already have a report card (e.g. from a concurrent
-- `routes::waves::create_wave` mint on a newer binary) leaves those rows
-- alone via `WHERE NOT EXISTS`.
--
-- Payload shape is the v1 `WaveReportPayload` (see
-- `crates/calm-server/src/wave_report.rs`):
--
--   { schemaVersion: 1, summary: "", body: "# Goal\n\n_The spec agent will fill this in._\n" }
--
-- Why `sort = -1.0`: the report card lands above every legacy card
-- without rewriting their sorts (existing card sorts are >= 0 by
-- convention — `next_sort_scoped_in_tx` only mints non-negative values).
-- The layout overlay seeded below pins the report card to (0, 0, 12, 4)
-- so the RGL grid renders it at the top of the wave; `sort` is the
-- list-mode fallback ordering used when no layout overlay exists.
--
-- The fallback id generator (`lower(hex(randomblob(16)))`) is a 32-char
-- random hex string. The kernel's Rust `new_id()` uses uuid-v4-simple
-- (also a 32-char hex string), so the two ID spaces are
-- format-compatible — a frontend / API consumer cannot tell a backfilled
-- report card id from a runtime-minted one by shape. The randomblob
-- collision space is 128 bits, matching uuid-v4.

INSERT INTO cards (id, wave_id, kind, role, sort, payload, deletable, created_at, updated_at)
SELECT
    lower(hex(randomblob(16))),
    w.id,
    'wave-report',
    'reportcard',
    -1.0,
    json_object(
        'schemaVersion', 1,
        'summary', '',
        'body', '# Goal' || char(10) || char(10) || '_The spec agent will fill this in._' || char(10)
    ),
    0,
    strftime('%s','now') * 1000,
    strftime('%s','now') * 1000
FROM waves w
WHERE NOT EXISTS (
    SELECT 1 FROM cards c
    WHERE c.wave_id = w.id AND c.role = 'reportcard'
);

-- Seed the layout overlay so each freshly minted report card lands at
-- (x=0, y=0, w=12, h=4). Two cases per wave:
--
--   (a) No layout overlay yet (most existing waves) — INSERT a new
--       row with a positions map carrying only the report card. The
--       frontend's `reconcile()` in `web/src/WaveGrid.tsx` auto-packs
--       the wave's pre-existing cards into the remaining rows on
--       first render, then persists the merged positions back to the
--       overlay; the missing entries don't break the grid.
--
--   (b) Layout overlay already exists — UPDATE the positions object
--       to insert the report card's entry alongside the existing
--       per-card positions. `json_patch` merges objects at the top
--       level, but we need to patch the nested `positions` map, so
--       we use `json_set` on the per-key path.
--
-- A single statement can't easily do "INSERT if absent / UPDATE if
-- present" against the new report card id without a second JOIN to
-- the just-inserted row, so we split into two passes: first INSERT
-- new overlay rows for waves that don't have one, then UPDATE existing
-- rows to patch in the new card's position. Both passes select the
-- new report card id off the `cards` table via the role + wave_id
-- join (`idx_cards_one_report_per_wave` makes this a unique lookup).

-- Pass 1: waves with no existing layout overlay — create one.
INSERT INTO overlays (id, plugin_id, entity_kind, entity_id, kind, payload, updated_at)
SELECT
    lower(hex(randomblob(16))),
    'kernel',
    'view',
    w.id,
    'layout',
    json_object(
        'schemaVersion', 1,
        'positions', json_object(
            c.id, json_object('x', 0, 'y', 0, 'w', 12, 'h', 4)
        )
    ),
    strftime('%s','now') * 1000
FROM waves w
JOIN cards c ON c.wave_id = w.id AND c.role = 'reportcard'
WHERE NOT EXISTS (
    SELECT 1 FROM overlays o
    WHERE o.plugin_id = 'kernel'
      AND o.entity_kind = 'view'
      AND o.entity_id = w.id
      AND o.kind = 'layout'
);

-- Pass 2: waves with an existing layout overlay — patch the report card
-- into the positions map. `json_set('$.positions.' || c.id, ...)` adds
-- a new key without disturbing the other entries. We also bump
-- `updated_at` so a client watching the row sees a fresh stamp.
UPDATE overlays
SET payload = (
        SELECT json_set(
            overlays.payload,
            '$.positions.' || c.id,
            json_object('x', 0, 'y', 0, 'w', 12, 'h', 4)
        )
        FROM cards c
        WHERE c.wave_id = overlays.entity_id
          AND c.role = 'reportcard'
    ),
    updated_at = strftime('%s','now') * 1000
WHERE overlays.plugin_id = 'kernel'
  AND overlays.entity_kind = 'view'
  AND overlays.kind = 'layout'
  AND EXISTS (
      SELECT 1 FROM cards c
      WHERE c.wave_id = overlays.entity_id
        AND c.role = 'reportcard'
  )
  -- Idempotency: only patch when the report card isn't already in the
  -- positions map. Re-running this migration on a DB where Pass 2 has
  -- already touched a row is a no-op.
  AND NOT EXISTS (
      SELECT 1 FROM cards c
      WHERE c.wave_id = overlays.entity_id
        AND c.role = 'reportcard'
        AND json_extract(overlays.payload, '$.positions.' || c.id) IS NOT NULL
  );
