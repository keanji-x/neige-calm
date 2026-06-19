-- Issue #760 slice 1: workflow-agnostic isolated workspace leases.
--
-- The kernel lease is deliberately git-free: a durable row plus a
-- disjoint directory path under `.claude/worktrees/<wave>/<card>`.
-- Plugin slices layer any git worktree semantics on top of this path.

CREATE TABLE workspace_leases (
  lease_id       TEXT PRIMARY KEY,
  card_id        TEXT NOT NULL,
  wave_id        TEXT NOT NULL REFERENCES waves(id) ON DELETE CASCADE,
  path           TEXT NOT NULL,
  state          TEXT NOT NULL CHECK (state IN ('held','releasing','released')),
  lease_owner    TEXT NOT NULL,
  lease_until_ms INTEGER NULL,
  boot_id        TEXT NULL,
  created_at_ms  INTEGER NOT NULL,
  updated_at_ms  INTEGER NOT NULL,
  released_at_ms INTEGER NULL
);

CREATE INDEX workspace_leases_state_idx
  ON workspace_leases(state, updated_at_ms, lease_id);

CREATE INDEX workspace_leases_card_state_idx
  ON workspace_leases(card_id, state, updated_at_ms);

CREATE INDEX workspace_leases_owner_idx
  ON workspace_leases(lease_owner, state);

CREATE UNIQUE INDEX workspace_leases_active_path_idx
  ON workspace_leases(path)
  WHERE state IN ('held','releasing');

-- Workspace lease events join the WS event union in the same release as
-- SYNC_EVENT_VERSION 4 -> 5. Re-stamp defensively in case any prerelease
-- build persisted the new kinds before the version bump landed.
UPDATE events
SET event_version = 5
WHERE kind IN ('workspace.leased', 'workspace.released')
  AND event_version < 5;
