-- Issue #1316 S2 — one concept (track), one spelling.
--
-- Second slice of the vocabulary convergence, same shape as 0080 (which did
-- `cove` -> `area`) and, before it, #1209 PR-2: value-preserving DDL plus
-- explicit rewrites of the persisted STRINGS, with the release gated as
-- breaking by WEB_COMPAT_VERSION / productMajor rather than by a
-- compatibility read.
--
-- Earlier migrations that spell `waves` / `wave_id` / `scope_wave` are NOT
-- edited and must never be: sqlx checksums each whole file including comments,
-- so editing an applied one bricks startup with VersionMismatch. Each ran
-- against the schema of its own point in history, strictly before this one.
--
-- WHAT THIS MIGRATION DELIBERATELY DOES NOT RENAME
--
-- Three vocabularies keep the retired spelling because they are DATA embedded
-- in agent-authored report documents, not names:
--
--   * `neige://wave/...` — the report link scheme, parsed by
--     `calm_types::report_links`. It lives inside stored report bodies, and the
--     report persist boundary writes `cards.payload` (JSON text) and
--     `cards.body_crdt` (automerge `AutoCommit::save()` bytes) TOGETHER. A SQL
--     string REPLACE would fix the JSON and desync the CRDT blob; rewriting it
--     properly means loading and re-saving every automerge document, which is a
--     content edit with its own history, not a migration.
--   * `spawn: "in-wave" | "sub-wave"` and the `root_wave_id` / `tree_waves`
--     argument keys — report-block DSL vocabulary, written by agents into the
--     same documents, with the same dual representation. `tasks.spawn` keeps
--     its `'in-wave'` DEFAULT for the same reason (and because SQLite cannot
--     alter a column default without a full table rebuild, which is not worth
--     buying a cosmetic change).
--   * `wave-conversation:` — SHA-256 input whose digest IS a persisted
--     `cards.id` and a persisted `operations` idempotency key. 0080 froze the
--     `cove-chat-conversation:` half for exactly this reason.

-- ---------------------------------------------------------------------------
-- 1. Tables and columns. `ALTER TABLE ... RENAME` is value-preserving; SQLite
--    rewrites references inside foreign keys, triggers and index DEFINITIONS
--    automatically. Only index NAMES need section 2.
-- ---------------------------------------------------------------------------
ALTER TABLE waves RENAME TO tracks;
ALTER TABLE wave_vcs_objects RENAME TO track_vcs_objects;
ALTER TABLE wave_vcs_commits RENAME TO track_vcs_commits;
ALTER TABLE wave_vcs_refs RENAME TO track_vcs_refs;

ALTER TABLE tracks RENAME COLUMN parent_wave_id TO parent_track_id;
ALTER TABLE cards RENAME COLUMN wave_id TO track_id;
ALTER TABLE events RENAME COLUMN scope_wave TO scope_track;
ALTER TABLE harness_items RENAME COLUMN wave_id TO track_id;
ALTER TABLE track_vcs_commits RENAME COLUMN wave_id TO track_id;
ALTER TABLE track_vcs_refs RENAME COLUMN wave_id TO track_id;
ALTER TABLE worker_sessions RENAME COLUMN wave_id TO track_id;
ALTER TABLE worker_flow_items RENAME COLUMN wave_id TO track_id;
ALTER TABLE workspace_leases RENAME COLUMN wave_id TO track_id;
ALTER TABLE tasks RENAME COLUMN wave_id TO track_id;
ALTER TABLE tasks RENAME COLUMN child_wave_id TO child_track_id;
ALTER TABLE task_ref_index RENAME COLUMN dst_wave_id TO dst_track_id;

-- ---------------------------------------------------------------------------
-- 2. Index names. SQLite has no `ALTER INDEX ... RENAME`. Every definition
--    below is reproduced from `sqlite_master` of a database migrated through
--    0080 — read off the real schema rather than reconstructed from the
--    migration history, because several of these indexes were themselves
--    renamed by 0080 and the history no longer describes the live set.
--
--    Unlike 0080, no predicate here tests a value this migration rewrites
--    (`role = 'spec'`, `role = 'reportcard'`, `purpose = 'launchpad'`,
--    `purpose = 'area-chat'` are all untouched), so section 3's ordering
--    constraint does not apply and these can all run first.
--
--    `task_ref_index_destination_idx` keeps its name — it has no `wave` in it,
--    and its column reference was already rewritten by section 1.
-- ---------------------------------------------------------------------------
DROP INDEX idx_cards_wave;
CREATE INDEX idx_cards_track ON cards(track_id, sort);

DROP INDEX idx_events_scope_wave;
CREATE INDEX idx_events_scope_track ON events(scope_track) WHERE scope_track IS NOT NULL;

DROP INDEX idx_cards_one_spec_per_wave;
CREATE UNIQUE INDEX idx_cards_one_spec_per_track ON cards(track_id) WHERE role = 'spec';

DROP INDEX idx_cards_one_report_per_wave;
CREATE UNIQUE INDEX idx_cards_one_report_per_track ON cards(track_id) WHERE role = 'reportcard';

DROP INDEX idx_wave_vcs_commits_wave_id;
CREATE INDEX idx_track_vcs_commits_track_id ON track_vcs_commits(track_id);

DROP INDEX idx_wave_vcs_commits_event_id;
CREATE INDEX idx_track_vcs_commits_event_id ON track_vcs_commits(event_id) WHERE event_id IS NOT NULL;

DROP INDEX idx_wave_vcs_objects_created_at;
CREATE INDEX idx_track_vcs_objects_created_at ON track_vcs_objects(created_at);

DROP INDEX idx_wave_vcs_commits_tree_hash;
CREATE INDEX idx_track_vcs_commits_tree_hash ON track_vcs_commits(tree_hash);

DROP INDEX ws_wave_idx;
CREATE INDEX ws_track_idx ON worker_sessions(track_id, created_at_ms, id);

DROP INDEX tasks_wave_status_idx;
CREATE INDEX tasks_track_status_idx ON tasks(track_id, status, priority DESC, created_at_ms);

DROP INDEX idx_waves_one_launchpad;
CREATE UNIQUE INDEX idx_tracks_one_launchpad ON tracks(purpose) WHERE purpose = 'launchpad';

DROP INDEX idx_waves_parent_wave_id;
CREATE INDEX idx_tracks_parent_track_id ON tracks(parent_track_id) WHERE parent_track_id IS NOT NULL;

DROP INDEX idx_tasks_child_wave_id;
CREATE UNIQUE INDEX idx_tasks_child_track_id ON tasks(child_track_id) WHERE child_track_id IS NOT NULL;

DROP INDEX idx_waves_area;
CREATE INDEX idx_tracks_area ON tracks(area_id, sort);

DROP INDEX idx_waves_one_chat_per_area;
CREATE UNIQUE INDEX idx_tracks_one_chat_per_area ON tracks(area_id) WHERE purpose = 'area-chat';

-- ---------------------------------------------------------------------------
-- 3. Persisted STRING VALUES. Each is read by code this slice already renamed,
--    so leaving any of them would be a silent runtime miss, not a compile
--    error.
--
--    `events.scope_kind` — `calm_types::EventScope::kind()`, pinned by
--    `event.rs::scope_kind_strings_pinned` whose comment states outright that
--    these strings are persisted and changing them is a wire break. This
--    UPDATE is what makes that break survivable.
--
--    `overlays.entity_kind` — `"wave" | "card"`, written by `card_fsm` and by
--    the plugin host. It is part of `overlays`' UNIQUE
--    `(plugin_id, entity_kind, entity_id, kind)`, so a collision would need a
--    pre-existing `'track'` row; nothing has ever written one, so the UPDATE
--    cannot violate it. It is also a plugin-facing contract key
--    (`plugin_host/events.rs`); #1268 established that renaming such a key is
--    acceptable here because there are no third-party plugins — that cost's
--    payer is the empty set.
--
--    `cards.kind` — the `wave-report` card kind. No index predicate references
--    it (the one-report-per-track index gates on `role`, not `kind`).
-- ---------------------------------------------------------------------------
UPDATE events   SET scope_kind  = 'track'        WHERE scope_kind  = 'wave';
UPDATE overlays SET entity_kind = 'track'        WHERE entity_kind = 'wave';
UPDATE cards    SET kind        = 'track-report' WHERE kind        = 'wave-report';

-- ---------------------------------------------------------------------------
-- 4. Event kinds, with the version bump that makes old clients drop the
--    rewritten rows instead of mis-reading them. Same reasoning as 0038 and
--    0080: the kind string is the wire-level discriminator the frontend gates
--    on, so bumping `event_version` to 15 (matching the SYNC_EVENT_VERSION
--    this slice raises 14 -> 15) makes the per-frame future-protocol gate drop
--    them WITHOUT advancing the replay cursor.
-- ---------------------------------------------------------------------------
UPDATE events SET kind = 'track.updated',           event_version = 15 WHERE kind = 'wave.updated';
UPDATE events SET kind = 'track.deleted',           event_version = 15 WHERE kind = 'wave.deleted';
UPDATE events SET kind = 'track.lifecycle_changed', event_version = 15 WHERE kind = 'wave.lifecycle_changed';
UPDATE events SET kind = 'track.report_edited',     event_version = 15 WHERE kind = 'wave.report_edited';
