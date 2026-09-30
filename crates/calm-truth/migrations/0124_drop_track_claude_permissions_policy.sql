-- #1893 S5 — the Claude Code permission scope of Planner-opened terminals is deleted: the
-- `claude_permissions` argument of `calm.terminal.open` and the Track policy that bounded it.
-- The column 0109 added has no reader or writer left. No index, view, trigger, foreign key or
-- CHECK names it, so plain DROP COLUMN applies (SQLite 3.35+) and no table is rebuilt.
ALTER TABLE tracks DROP COLUMN claude_permissions_policy;
