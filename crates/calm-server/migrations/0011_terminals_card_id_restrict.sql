-- Issue #197 — eager-teardown lifecycle for card/wave delete.
--
-- Before: `terminals.card_id REFERENCES cards(id) ON DELETE CASCADE`.
-- That made the route handlers' `card_delete_tx` / `wave_delete_tx` quietly
-- nuke the terminal row underneath, leaving the daemon process + unix socket
-- behind as a leak the sweeper had to catch on a 30-60s cadence. The sweeper
-- doc was outright wrong about what it cleaned up (the row was already gone
-- by the time it ran), and the daemon lingered with no audit signal for
-- the gap.
--
-- After: `ON DELETE RESTRICT`. The schema now refuses a card delete that
-- would orphan a terminal row, forcing the route handlers
-- (`routes/cards.rs::delete_card`, `routes/waves.rs::delete_wave`,
-- `routes/coves.rs::delete_cove`) to clean up the terminal — kill daemon,
-- unlink socket, delete the row — *before* the card delete fires. The
-- sweeper is now a fallback for crash / OOM scenarios only (see
-- `terminal_sweeper.rs` doc + `docs/sync-engine-design.md` §10).
--
-- SQLite specifics
--   * Foreign-key constraints can't be altered in place; we rebuild the
--     `terminals` table with the new constraint. The migration runner opens
--     each `.sql` file in its own transaction, and sqlite's
--     `defer_foreign_keys=ON` keeps the FK check until commit so the
--     COPY/DROP/RENAME dance is sound.
--   * `PRAGMA foreign_keys` is connection-scoped — sqlx flips it on
--     post-connect (see `crates/calm-server/src/db/sqlite.rs::SqlxRepo::open`).
--     We toggle it off explicitly here just for the duration of the
--     rebuild so the temporary `terminals_new` table doesn't get caught
--     mid-step by a stray FK validation; sqlite restores normal handling
--     on the next statement after the implicit `END` (the migration
--     wrapper commits).
--   * Indexes on the renamed table need to be re-declared by hand — the
--     SQLite `CREATE TABLE ... AS SELECT` / `RENAME` paths don't carry
--     indexes across. The original `terminals` schema had no auxiliary
--     index (PK on `id` and UNIQUE on `card_id` are inline), so the
--     re-declared shape below is exactly the same minus the changed FK.

PRAGMA defer_foreign_keys = ON;

CREATE TABLE terminals_new (
    id            TEXT    PRIMARY KEY,
    card_id       TEXT    NOT NULL UNIQUE REFERENCES cards(id) ON DELETE RESTRICT,
    program       TEXT    NOT NULL,
    cwd           TEXT    NOT NULL,
    env           TEXT    NOT NULL DEFAULT '{}',
    daemon_handle TEXT,
    pid           INTEGER,                        -- added by migration 0005
    created_at    INTEGER NOT NULL
);

INSERT INTO terminals_new (id, card_id, program, cwd, env, daemon_handle, pid, created_at)
SELECT id, card_id, program, cwd, env, daemon_handle, pid, created_at FROM terminals;

DROP TABLE terminals;
ALTER TABLE terminals_new RENAME TO terminals;

PRAGMA defer_foreign_keys = OFF;
