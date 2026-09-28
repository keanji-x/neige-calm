-- #1830 S1 — the kernel-made git worktree an attached track's Planner runs in:
-- `<repo_root>/.claude/worktrees/track-<id>` on branch `neige/track-<id>`. NULL for managed,
-- child and pre-#1830 tracks (no backfill: a Codex thread and a Claude session keep the cwd
-- they started with). `workspace_path` keeps naming the user's checkout.
ALTER TABLE tracks ADD COLUMN workspace_worktree_path TEXT NULL;
