-- Issue #1147 S1 (design D1 / D9) — the wave workspace becomes a typed field,
-- and `waves.cwd` is REPLACED by it rather than shadowed by it.
--
-- No already-applied migration is edited (sqlx checksums whole files; editing
-- an applied one bricks startup with VersionMismatch). This file has never
-- been released, so it is free to evolve.
--
--   * `workspace_kind TEXT NOT NULL DEFAULT 'attached'`
--       'managed'  — server-created under the managed root, exclusively
--                    owned, recyclable from S5 on. Not mintable until S2, so
--                    no row can legally carry it yet.
--       'attached' — a repository the user pointed at. Never deleted, never
--                    `git init`-ed, never renamed by the server.
--     Typed rather than inferred from a path prefix because the only thing the
--     distinction buys is permission to destroy a directory; deriving that
--     from string matching is how you delete a user's repo.
--
--   * `workspace_path TEXT NOT NULL DEFAULT ''` — the absolute path.
--   * `workspace_frozen_at INTEGER NULL` — one-shot, monotonic. NOT NULL ⇒
--     path and kind are immutable.
--
-- ---------------------------------------------------------------------------
-- Why `cwd` is dropped instead of kept as a projection
-- ---------------------------------------------------------------------------
--
-- The first draft of this slice kept `cwd` as a second copy of
-- `workspace_path`, "so existing readers don't have to change", with an
-- invariant that only one function may write both. That invariant has no
-- enforcement mechanism in this codebase: writers of `waves` are scattered raw
-- `sqlx` statements, so the only way to police a two-column-must-agree rule is
-- a source-text scanner — and three successive rounds of red-teaming walked
-- past three successive scanners (schema-qualified `main.waves`, `UPDATE OR
-- REPLACE`, `include_str!` of a `.sql` file, `#[path]` modules outside the
-- scanned tree). Each round produced a cleverer guess at Rust source text, and
-- each round was defeated, because a text scanner cannot decide what code does.
--
-- Deleting the column deletes the problem class. One column, one writer, no
-- possible disagreement, nothing to police. The readers change — that is the
-- actual cost, and it is a one-time compile-and-grep, paid below.
--
-- `cwd` survives on the WIRE as a serialization alias computed from
-- `workspace.path`, so old clients and the event goldens see no change.
--
-- Backfill (design D9). Every pre-#1147 wave is `attached` and points at its
-- own `cwd`; all but the system cove's are frozen at `created_at`:
--
--   * attached — managed roots did not exist before this migration, so every
--     existing directory was created by somebody other than the server.
--   * path = cwd — cwd is the only path these rows ever had. Rows whose cwd
--     is '' (the migration-0018 backfill default) stay '' rather than being
--     invented; they are already broken and S2's new-wave path is what fixes
--     new work, per D9.
--   * frozen_at = created_at — these waves physically ran in that directory,
--     so treating them as already frozen is the truthful reading. It is also
--     the fail-safe one: an unfrozen `attached` row is exactly the state in
--     which a future PATCH branch that forgot to check `kind` would relocate
--     a real user repository, and "clean worktree, one commit" is a perfectly
--     ordinary state for a freshly `git init`-ed user project.
--     **Except in the system cove** — see the `CASE` below. That cove's wave
--     is kernel-owned and gets re-pointed by `today_launchpad_ensure_tx`, so
--     freezing it here would falsify D1's monotonicity on the next `ensure`.
--
-- The `:4140` waves whose cwd is `/home/kenji` / `/tmp` / `/` (the #1131
-- fallout) are backfilled the same way on purpose. They are not repaired
-- here: repairing them means choosing a new directory for a wave that has
-- already run, which is a product decision, not a migration.

ALTER TABLE waves ADD COLUMN workspace_kind TEXT NOT NULL DEFAULT 'attached';
ALTER TABLE waves ADD COLUMN workspace_path TEXT NOT NULL DEFAULT '';
ALTER TABLE waves ADD COLUMN workspace_frozen_at INTEGER NULL;

UPDATE waves
SET workspace_kind = 'attached',
    workspace_path = cwd,
    -- The system cove's waves stay UNFROZEN. That cove holds the kernel-owned
    -- Today/launchpad wave, and `today_launchpad_ensure_tx`'s adopt branch
    -- re-points its path (a legacy `Today` wave is adopted, renamed to
    -- `purpose='launchpad'` and re-aimed at the caller's cwd). Freezing it here
    -- would mean the very next `ensure` either re-points a frozen workspace or
    -- clears an existing stamp — both of which falsify D1's "one-shot and
    -- monotonic" reading of `frozen_at`, in opposite directions.
    --
    -- Leaving it NULL is the whole point of the D9 exception: unfrozen means
    -- "still re-pointable", which is exactly true of this one wave. The bound
    -- on the exception — `frozen_at IS NULL` implies a system-cove wave — is
    -- asserted in `only_system_cove_waves_may_be_unfrozen`.
    workspace_frozen_at = CASE
        WHEN (SELECT c.kind FROM coves AS c WHERE c.id = waves.cove_id) = 'system'
        THEN NULL
        ELSE created_at
    END;

-- `cwd` is now redundant with `workspace_path`, and a redundant column is a
-- drift surface with no owner. SQLite has supported `ALTER TABLE DROP COLUMN`
-- since 3.35.0 (2021-03); this workspace links libsqlite3-sys 0.30.x, well
-- past that, so no table rebuild is needed. The column carries no index, view,
-- trigger or generated-column reference (`idx_waves_cove`,
-- `idx_waves_parent_wave_id`, `idx_waves_one_launchpad`,
-- `idx_waves_one_chat_per_cove` are the only `waves` indexes and none names
-- it), which are the conditions under which SQLite refuses the drop.
ALTER TABLE waves DROP COLUMN cwd;
