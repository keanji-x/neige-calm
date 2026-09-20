-- #1727 S4 slice 1 — a workspace lease records the commit its worktree starts
-- from and where that worktree really is.
--
-- `base_sha` is the commit `git worktree add` is pinned to (today always the
-- attached repository's HEAD as read inside the worker op's prepare
-- transaction, `base_source = 'head'`; `'commit'` / `'attempt'` are written by
-- a later slice). `base_attempt_id` names the producing attempt for
-- `'attempt'` only. `canonical_path` is the realpath the provisioned worktree
-- must resolve to and `git_common_dir` the realpath of the repository's common
-- git dir, both read in the same transaction.
--
-- Every column stays NULL on rows written before this migration (legacy
-- leases) and on the fixtures-only plain lease. The tuple CHECK below sits on
-- the last ADD COLUMN and is NULL-safe per column: a CHECK only rejects FALSE,
-- and `NULL IN (...)`, `NULL = 'attempt'` and `TRUE AND NULL` are all NULL, so
-- each branch spells `IS NULL` / `IS NOT NULL` for every column and tests
-- `IS NOT NULL` before `IN` / `=` (`FALSE AND NULL` is FALSE). Exactly four
-- shapes are accepted: all-NULL, (sha, head, NULL, path, dir),
-- (sha, commit, NULL, path, dir), (sha, attempt, attempt-id, path, dir).
ALTER TABLE workspace_leases ADD COLUMN base_sha TEXT NULL;
ALTER TABLE workspace_leases ADD COLUMN base_source TEXT NULL;
ALTER TABLE workspace_leases ADD COLUMN base_attempt_id TEXT NULL;
ALTER TABLE workspace_leases ADD COLUMN canonical_path TEXT NULL;
ALTER TABLE workspace_leases ADD COLUMN git_common_dir TEXT NULL
  CHECK ((base_sha IS NULL AND base_source IS NULL AND base_attempt_id IS NULL AND canonical_path IS NULL AND git_common_dir IS NULL) OR (base_sha IS NOT NULL AND base_source IS NOT NULL AND base_source IN ('head','commit') AND base_attempt_id IS NULL AND canonical_path IS NOT NULL AND git_common_dir IS NOT NULL) OR (base_sha IS NOT NULL AND base_source IS NOT NULL AND base_source = 'attempt' AND base_attempt_id IS NOT NULL AND canonical_path IS NOT NULL AND git_common_dir IS NOT NULL));
