-- Issue #973 — withdraw the proposal write channel.
--
-- `proposals` was only a rebuildable projection. The append-only
-- `proposal.submitted` / `proposal.resolved` rows in `events` remain the
-- audit truth and stay readable for historical replay.

DROP TABLE proposals;
