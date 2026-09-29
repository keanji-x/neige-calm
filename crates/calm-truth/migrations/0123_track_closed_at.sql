-- #1876 — a track is open or closed. One nullable `closed_at` (unix ms), non-null exactly when
-- the track is closed, replaces `lifecycle`, `terminal_at` and `archived_at`.
--
-- Every terminal row carries `terminal_at`, so it becomes the close time. No index, view,
-- trigger, foreign key or CHECK names the dropped columns, so plain DROP COLUMN applies
-- (SQLite 3.35+) and no table is rebuilt.
ALTER TABLE tracks ADD COLUMN closed_at INTEGER NULL;
UPDATE tracks SET closed_at = terminal_at WHERE lifecycle IN ('done','canceled','failed');
ALTER TABLE tracks DROP COLUMN lifecycle;
ALTER TABLE tracks DROP COLUMN terminal_at;
ALTER TABLE tracks DROP COLUMN archived_at;

-- The commit hash is computed once, when a commit is written, so dropping a hashed column
-- leaves stored hashes as they are.
ALTER TABLE track_vcs_commits DROP COLUMN lifecycle;

-- The kind is gone. Each row has a paired `track.updated` with the same agent message.
DELETE FROM events WHERE kind = 'track.lifecycle_changed';
