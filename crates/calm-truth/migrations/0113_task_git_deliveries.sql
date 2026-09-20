-- #1727 S4 slice 2 — Git deliveries and the candidates they pin.
--
-- `workspace_leases.delivery_policy`: `'kernel'` = the kernel commits and pins this lease's
-- worktree (the one criterion for candidate binding); NULL = a lease claimed before this
-- migration, a non-git lease, or the fixtures-only plain lease (legacy). NULL-safe column CHECK:
-- `'kernel'` needs a recorded base (sqlite 3.40.1: `{sha,NULL} x {kernel,NULL,bogus,''}` accepts
-- exactly (sha,kernel), (sha,NULL), (NULL,NULL)).
ALTER TABLE workspace_leases ADD COLUMN delivery_policy TEXT NULL
  CHECK ((delivery_policy IS NULL) OR (delivery_policy = 'kernel' AND base_sha IS NOT NULL));

-- One row per delivery attempt of one worker attempt; the persistent hand-off written in the
-- report transaction before any Operation exists. The six settlement columns move from all-NULL
-- to one of two shapes exactly once, in one UPDATE (the trigger below is the backstop; the
-- settlement transaction's own `WHERE settlement IS NULL` is the guard).
CREATE TABLE task_git_deliveries (
  delivery_id TEXT PRIMARY KEY NOT NULL,
  track_id TEXT NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
  producer_attempt_id TEXT NOT NULL,
  card_id TEXT NOT NULL,
  lease_id TEXT NOT NULL REFERENCES workspace_leases(lease_id),
  ordinal INTEGER NOT NULL,
  operation_key TEXT NOT NULL UNIQUE,
  forge_idempotency_key TEXT NOT NULL UNIQUE,
  predecessor_delivery_id TEXT NULL REFERENCES task_git_deliveries(delivery_id),
  request_idempotency_key TEXT NULL,
  reason TEXT NULL,
  created_at_ms INTEGER NOT NULL,
  settlement TEXT NULL,
  settled_event_id INTEGER NULL,
  failure_code TEXT NULL,
  failure_reason TEXT NULL,
  retry_allowed INTEGER NULL,
  wake_reason TEXT NULL,
  UNIQUE(producer_attempt_id, ordinal),
  UNIQUE(producer_attempt_id, request_idempotency_key),
  CHECK ((settlement IS NULL AND settled_event_id IS NULL AND failure_code IS NULL AND failure_reason IS NULL AND retry_allowed IS NULL AND wake_reason IS NULL) OR (settlement IS NOT NULL AND settlement = 'candidate' AND settled_event_id IS NOT NULL AND failure_code IS NULL AND failure_reason IS NULL AND retry_allowed IS NULL AND wake_reason IS NOT NULL AND wake_reason IN ('ungated_candidate','gate_already_terminal','deferred_to_gate')) OR (settlement IS NOT NULL AND settlement = 'failed' AND settled_event_id IS NOT NULL AND failure_code IS NOT NULL AND failure_reason IS NOT NULL AND retry_allowed IS NOT NULL AND retry_allowed IN (0,1) AND wake_reason IS NOT NULL AND wake_reason = 'failed'))
);
CREATE TRIGGER task_git_delivery_settled_once BEFORE UPDATE ON task_git_deliveries
WHEN OLD.settlement IS NOT NULL OR NEW.settlement IS NULL
 OR NEW.delivery_id IS NOT OLD.delivery_id OR NEW.track_id IS NOT OLD.track_id
 OR NEW.producer_attempt_id IS NOT OLD.producer_attempt_id OR NEW.card_id IS NOT OLD.card_id
 OR NEW.lease_id IS NOT OLD.lease_id OR NEW.ordinal IS NOT OLD.ordinal
 OR NEW.operation_key IS NOT OLD.operation_key
 OR NEW.forge_idempotency_key IS NOT OLD.forge_idempotency_key
 OR NEW.predecessor_delivery_id IS NOT OLD.predecessor_delivery_id
 OR NEW.request_idempotency_key IS NOT OLD.request_idempotency_key
 OR NEW.reason IS NOT OLD.reason OR NEW.created_at_ms IS NOT OLD.created_at_ms
BEGIN SELECT RAISE(ABORT, 'git delivery settlement is written once; other columns are immutable'); END;

-- One candidate per successful delivery (`candidate_id = delivery_id`), at most one per attempt.
-- Every column is a byte copy of the operation result or of the lease row; nothing here is derived
-- from events.
CREATE TABLE task_candidates (
  candidate_id TEXT PRIMARY KEY NOT NULL REFERENCES task_git_deliveries(delivery_id),
  track_id TEXT NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
  producer_attempt_id TEXT NOT NULL UNIQUE,
  card_id TEXT NOT NULL,
  lease_id TEXT NOT NULL REFERENCES workspace_leases(lease_id),
  repo_root TEXT NOT NULL,
  git_common_dir TEXT NOT NULL,
  branch TEXT NOT NULL,
  base_sha TEXT NOT NULL,
  commit_sha TEXT NOT NULL,
  base_is_ancestor INTEGER NOT NULL CHECK (base_is_ancestor IN (0,1)),
  ref_name TEXT NOT NULL,
  created_at_ms INTEGER NOT NULL
);
CREATE TRIGGER task_candidate_immutable BEFORE UPDATE ON task_candidates
BEGIN SELECT RAISE(ABORT, 'git candidate identity is immutable'); END;
