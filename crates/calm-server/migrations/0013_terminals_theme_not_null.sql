-- Issue #177 — theme is a terminal-row invariant.
--
-- The original 0012_terminals_theme migration shipped `theme_fg` /
-- `theme_bg` as nullable TEXT columns so that pre-#177 rows (which had
-- no host theme to stamp) could coexist with new spawn paths that did.
-- That nullability was the seam the "racing spawn paths" bug exploited:
-- a callsite that forgot to thread the host theme would silently leave
-- both columns NULL, the WS auto-revive's `spawn_daemon_with_parts`
-- would then pass `SpawnDaemonOpts::default()` and the read-from-row
-- fallback would also return None — leaving codex's OSC 10/11 probe
-- unanswered.
--
-- The root-cause fix (this migration + the matching code changes):
-- theme is **required at row-creation time**. There is exactly one
-- write path (`card_with_*_create_tx` → `terminal_create_tx`), and it
-- now takes `theme: RequestTheme` as a non-optional argument. The
-- spawn helper reads `term.theme_fg` / `term.theme_bg` directly with
-- no fallback. The "no theme" case is gone — every terminal row, every
-- spawn, every revive carries identical theme args by construction.
--
-- Backfill before ALTERing: any pre-#177 row (the in-place upgrade
-- path) gets the dark-theme defaults that match
-- `RequestTheme::default_dark()` and `DARK_THEME_RGB` in
-- `web/src/shared/themeRgb.ts`. This is a one-time stamp; subsequent
-- rows go through the codepath that requires a real value.
UPDATE terminals
   SET theme_fg = '216,219,226'
 WHERE theme_fg IS NULL;
UPDATE terminals
   SET theme_bg = '15,20,24'
 WHERE theme_bg IS NULL;

-- SQLite doesn't support `ALTER COLUMN ... SET NOT NULL`. Rebuild the
-- table with the new constraint. Same dance as 0011 (see that
-- migration's header for the `PRAGMA defer_foreign_keys` rationale).
PRAGMA defer_foreign_keys = ON;

CREATE TABLE terminals_new (
    id            TEXT    PRIMARY KEY,
    card_id       TEXT    NOT NULL UNIQUE REFERENCES cards(id) ON DELETE RESTRICT,
    program       TEXT    NOT NULL,
    cwd           TEXT    NOT NULL,
    env           TEXT    NOT NULL DEFAULT '{}',
    daemon_handle TEXT,
    pid           INTEGER,
    theme_fg      TEXT    NOT NULL,
    theme_bg      TEXT    NOT NULL,
    created_at    INTEGER NOT NULL
);

INSERT INTO terminals_new
    (id, card_id, program, cwd, env, daemon_handle, pid,
     theme_fg, theme_bg, created_at)
SELECT id, card_id, program, cwd, env, daemon_handle, pid,
       theme_fg, theme_bg, created_at
  FROM terminals;

DROP TABLE terminals;
ALTER TABLE terminals_new RENAME TO terminals;

PRAGMA defer_foreign_keys = OFF;
