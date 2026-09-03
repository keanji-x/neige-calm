-- Issue #1316 S1 — one concept (area), one spelling.
--
-- `Cove` was a coinage no English reader could decode. The product language is
-- now `Area`, and this migration finishes the rename on the storage side, in
-- the same shape #1209 PR-2 used for `workflow_id` -> `template_id`:
-- value-preserving DDL plus explicit rewrites of the persisted STRINGS, with
-- the release gated as breaking by WEB_COMPAT_VERSION / productMajor rather
-- than by a compatibility read.
--
-- Earlier migrations that spell `coves` / `cove_id` / `scope_cove` (0001, 0007,
-- 0009, 0015, 0063, 0074, 0075, 0077 and others) are NOT edited and must never
-- be: sqlx checksums each whole file including comments, so editing an applied
-- one bricks startup with VersionMismatch. Each of them ran against the schema
-- of its own point in history, strictly before this one; replay order is
-- unchanged.
--
-- Three distinct classes of change live here, and only the first is DDL:

-- ---------------------------------------------------------------------------
-- 1. Tables and columns. `ALTER TABLE ... RENAME` is value-preserving: every
--    existing row keeps its value verbatim. SQLite rewrites references to a
--    renamed table/column inside foreign keys, triggers and index definitions
--    automatically (legacy_alter_table defaults off), which is why only the
--    index NAMES need the explicit treatment in section 2.
-- ---------------------------------------------------------------------------
ALTER TABLE coves RENAME TO areas;
ALTER TABLE cove_folders RENAME TO area_folders;
ALTER TABLE area_folders RENAME COLUMN cove_id TO area_id;
ALTER TABLE waves RENAME COLUMN cove_id TO area_id;
ALTER TABLE events RENAME COLUMN scope_cove TO scope_area;

-- ---------------------------------------------------------------------------
-- 2. Index names. SQLite has no `ALTER INDEX ... RENAME`, so each is dropped
--    and recreated. The definitions are reproduced from the migrations that
--    created them — 0001 (sort), 0009 (one_system), 0007 (scope), 0015
--    (folders) — with only the spelling changed.
--
--    `idx_waves_one_chat_per_cove` (0074) is deliberately NOT here. Its
--    partial-index PREDICATE tests a persisted VALUE, so it can only be
--    recreated after section 3 rewrites that value; it lives at the end of
--    section 3 instead.
-- ---------------------------------------------------------------------------
DROP INDEX idx_coves_sort;
CREATE INDEX idx_areas_sort ON areas(sort);

DROP INDEX idx_coves_one_system;
CREATE UNIQUE INDEX idx_areas_one_system ON areas(kind) WHERE kind = 'system';

DROP INDEX idx_waves_cove;
CREATE INDEX idx_waves_area ON waves(area_id, sort);

DROP INDEX idx_cove_folders_cove;
CREATE INDEX idx_area_folders_area ON area_folders(area_id);

DROP INDEX idx_events_scope_cove;
CREATE INDEX idx_events_scope_area ON events(scope_area) WHERE scope_area IS NOT NULL;

-- ---------------------------------------------------------------------------
-- 3. Persisted STRING VALUES. These are the ones a schema-only rename misses,
--    and each is read by code this slice has already renamed, so leaving any
--    of them would be a silent runtime miss rather than a compile error.
--
--    `events.scope_kind` — `calm_types::EventScope::kind()`. There is a test
--    (`event.rs::scope_kind_strings_pinned`) whose comment states outright
--    that these strings are persisted and changing them is a wire break; this
--    UPDATE is what makes that break survivable.
--
--    `waves.purpose` — `'cove-chat'` marks the hidden per-area chat wave.
--    `idx_waves_one_chat_per_cove` (0074) is a UNIQUE PARTIAL index whose
--    predicate is exactly this value, which is why it is rebuilt at the end of
--    this section rather than with the other five: the rewrite has to land
--    first, or the new index would be created over zero matching rows and
--    would enforce nothing. Rebuilding it after the UPDATE also means its
--    uniqueness constraint is validated against the rewritten data.
-- ---------------------------------------------------------------------------
UPDATE events SET scope_kind = 'area' WHERE scope_kind = 'cove';
UPDATE waves SET purpose = 'area-chat' WHERE purpose = 'cove-chat';

DROP INDEX IF EXISTS idx_waves_one_chat_per_cove;
CREATE UNIQUE INDEX idx_waves_one_chat_per_area
ON waves(area_id)
WHERE purpose = 'area-chat';

-- ---------------------------------------------------------------------------
-- 4. Event kinds, with the version bump that makes old clients drop the
--    rewritten rows instead of mis-reading them.
--
--    Same reasoning as 0038: the kind string is the wire-level discriminator
--    the frontend gates on. A client whose cached gate is <= 13 would treat a
--    rewritten row as in-range, advance its replay cursor, then silently fail
--    zod on the new discriminator. Bumping `event_version` to 14 (matching the
--    SYNC_EVENT_VERSION this slice raises 13 -> 14) makes the per-frame
--    future-protocol gate drop them WITHOUT advancing the cursor, preserving
--    it until the user refreshes onto the matching bundle.
-- ---------------------------------------------------------------------------
UPDATE events SET kind = 'area.updated', event_version = 14 WHERE kind = 'cove.updated';
UPDATE events SET kind = 'area.deleted', event_version = 14 WHERE kind = 'cove.deleted';
