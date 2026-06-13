-- Sync engine event envelope version stamp.
--
-- The sync event log is a Tier-A persistence contract (see
-- `docs/upgrade-stability.md`). Before this migration, the wire / on-disk
-- envelope carried no version, leaving the kernel unable to refuse an
-- incompatible replay or evolve the envelope shape without breaking
-- replicas. This column makes the envelope version load-bearing instead
-- of implicit.
--
-- Old rows: the `DEFAULT 1` clause backfills every existing row to
-- version 1 — the de-facto schema everything written before this
-- migration spoke. The matching Rust-side constant is
-- `SYNC_EVENT_VERSION` in `event.rs`; bump both together when the
-- envelope shape changes in a way replicas need to gate on.

ALTER TABLE events ADD COLUMN event_version INTEGER NOT NULL DEFAULT 1;
