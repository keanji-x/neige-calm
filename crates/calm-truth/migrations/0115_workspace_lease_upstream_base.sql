-- #1777 — a lease may start from the attached repository's upstream.
--
-- `workspace_leases.base_source` gains `'upstream'`: the last known commit of
-- the upstream of the branch the attached repository's HEAD is on (the
-- kernel-fetched `refs/neige/upstream/<remote>/<branch>`, else the
-- repository's own remote-tracking ref). It sits in the `head`/`commit` arm of
-- the 0111 tuple CHECK: no `base_attempt_id`, every other base column set.
-- Nothing else changes; the 0113 `delivery_policy` CHECK is carried over as is.
--
-- SQLite cannot alter a CHECK, so the table is rebuilt (0099's shape: copy,
-- drop, rename, re-declare the indexes). `workspace_leases` is a parent:
-- `task_git_deliveries.lease_id` and `task_candidates.lease_id` reference it,
-- NOT NULL, and both tables refuse UPDATE by trigger, so 0099's
-- NULL-out-and-restore cannot detach them. With enforcement ON (it stays ON:
-- `PRAGMA foreign_keys` is a no-op inside the migration's transaction), the
-- DROP's implicit DELETE would orphan them. So the three child tables
-- (`task_candidates`, `task_git_delivery_abandonments`,
-- `task_git_deliveries`) are saved whole, rowid included, emptied, and
-- re-inserted after the rename; their triggers are BEFORE UPDATE only, and
-- every foreign key is checked at statement end, so the self-referencing
-- `predecessor_delivery_id` re-inserts in one statement. Rowids are carried
-- over for the rebuilt table too.
CREATE TEMP TABLE task_candidates_0115 AS SELECT rowid AS saved_rowid, * FROM task_candidates;
CREATE TEMP TABLE task_git_delivery_abandonments_0115 AS
  SELECT rowid AS saved_rowid, * FROM task_git_delivery_abandonments;
CREATE TEMP TABLE task_git_deliveries_0115 AS
  SELECT rowid AS saved_rowid, * FROM task_git_deliveries;
DELETE FROM task_candidates;
DELETE FROM task_git_delivery_abandonments;
DELETE FROM task_git_deliveries;

CREATE TABLE workspace_leases_0115 (
  lease_id       TEXT PRIMARY KEY,
  card_id        TEXT NOT NULL,
  track_id        TEXT NOT NULL REFERENCES "tracks"(id) ON DELETE CASCADE,
  path           TEXT NOT NULL,
  state          TEXT NOT NULL CHECK (state IN ('held','releasing','released')),
  lease_owner    TEXT NOT NULL,
  lease_until_ms INTEGER NULL,
  boot_id        TEXT NULL,
  created_at_ms  INTEGER NOT NULL,
  updated_at_ms  INTEGER NOT NULL,
  released_at_ms INTEGER NULL
, base_sha TEXT NULL, base_source TEXT NULL, base_attempt_id TEXT NULL, canonical_path TEXT NULL, git_common_dir TEXT NULL
  CHECK ((base_sha IS NULL AND base_source IS NULL AND base_attempt_id IS NULL AND canonical_path IS NULL AND git_common_dir IS NULL) OR (base_sha IS NOT NULL AND base_source IS NOT NULL AND base_source IN ('head','upstream','commit') AND base_attempt_id IS NULL AND canonical_path IS NOT NULL AND git_common_dir IS NOT NULL) OR (base_sha IS NOT NULL AND base_source IS NOT NULL AND base_source = 'attempt' AND base_attempt_id IS NOT NULL AND canonical_path IS NOT NULL AND git_common_dir IS NOT NULL)), delivery_policy TEXT NULL
  CHECK ((delivery_policy IS NULL) OR (delivery_policy = 'kernel' AND base_sha IS NOT NULL)));

INSERT INTO workspace_leases_0115 (
  rowid, lease_id, card_id, track_id, path, state, lease_owner, lease_until_ms, boot_id,
  created_at_ms, updated_at_ms, released_at_ms, base_sha, base_source, base_attempt_id,
  canonical_path, git_common_dir, delivery_policy
)
SELECT
  rowid, lease_id, card_id, track_id, path, state, lease_owner, lease_until_ms, boot_id,
  created_at_ms, updated_at_ms, released_at_ms, base_sha, base_source, base_attempt_id,
  canonical_path, git_common_dir, delivery_policy
FROM workspace_leases;
DROP TABLE workspace_leases;
ALTER TABLE workspace_leases_0115 RENAME TO workspace_leases;

CREATE INDEX workspace_leases_state_idx
  ON workspace_leases(state, updated_at_ms, lease_id);
CREATE INDEX workspace_leases_card_state_idx
  ON workspace_leases(card_id, state, updated_at_ms);
CREATE INDEX workspace_leases_owner_idx
  ON workspace_leases(lease_owner, state);
CREATE UNIQUE INDEX workspace_leases_active_path_idx
  ON workspace_leases(path)
  WHERE state IN ('held','releasing');

INSERT INTO task_git_deliveries (
  rowid, delivery_id, track_id, producer_attempt_id, card_id, lease_id, ordinal, operation_key,
  forge_idempotency_key, predecessor_delivery_id, request_idempotency_key, reason, created_at_ms,
  settlement, settled_event_id, failure_code, failure_reason, retry_allowed, wake_reason
)
SELECT
  saved_rowid, delivery_id, track_id, producer_attempt_id, card_id, lease_id, ordinal,
  operation_key, forge_idempotency_key, predecessor_delivery_id, request_idempotency_key, reason,
  created_at_ms, settlement, settled_event_id, failure_code, failure_reason, retry_allowed,
  wake_reason
FROM task_git_deliveries_0115;
INSERT INTO task_git_delivery_abandonments (
  rowid, delivery_id, track_id, producer_attempt_id, request_idempotency_key, reason,
  task_outcome, task_status, created_at_ms
)
SELECT
  saved_rowid, delivery_id, track_id, producer_attempt_id, request_idempotency_key, reason,
  task_outcome, task_status, created_at_ms
FROM task_git_delivery_abandonments_0115;
INSERT INTO task_candidates (
  rowid, candidate_id, track_id, producer_attempt_id, card_id, lease_id, repo_root,
  git_common_dir, branch, base_sha, commit_sha, base_is_ancestor, ref_name, created_at_ms
)
SELECT
  saved_rowid, candidate_id, track_id, producer_attempt_id, card_id, lease_id, repo_root,
  git_common_dir, branch, base_sha, commit_sha, base_is_ancestor, ref_name, created_at_ms
FROM task_candidates_0115;
DROP TABLE task_candidates_0115;
DROP TABLE task_git_delivery_abandonments_0115;
DROP TABLE task_git_deliveries_0115;
