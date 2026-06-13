-- Issue #229 — system-card infrastructure (PR A of the wave-report series).
--
-- Add a per-card `deletable` bit so kernel-owned cards (today: spec; PR B:
-- wave-report) can refuse direct REST / plugin-callback delete. Cascading
-- deletes via the wave/cove FK chain still go through — the guard only
-- closes the `DELETE /api/cards/:id` and `neige.card.delete` paths.
--
-- Why a column and not a role-check at the API layer: `Card.role` is the
-- *authorization label for emitted events* (see migration 0008). It happens
-- that today's only kernel-owned role is `'spec'`, but PR B introduces a
-- second one (`'reportcard'`) and we'd rather not re-encode "is this card
-- kernel-owned?" as a role-list scattered across the delete handlers.
-- `deletable` is the explicit storage-time bit; the API guard is a one-line
-- check against it.
--
-- Backfill: every existing `role = 'spec'` row flips to `deletable = 0`.
-- This closes a latent bug — spec cards were always kernel-owned by role
-- (one-spec-per-wave invariant in migration 0008) but `DELETE /api/cards/:id`
-- would happily drop them, leaving the wave with a missing spec card and
-- no way for the kernel to re-mint it without rebuilding the wave. PR A
-- both lands the new `'reportcard'` role variant *and* retroactively locks
-- down spec deletes via the same bit.

ALTER TABLE cards ADD COLUMN deletable INTEGER NOT NULL DEFAULT 1;

UPDATE cards SET deletable = 0 WHERE role = 'spec';

-- Partial unique index for the new ReportCard role: one report card per
-- wave (mirrors the spec-card index landed in 0008). The role variant
-- itself is defined in this PR but no rows are minted yet — PR B wires
-- the wave-create path to also stamp a report card.
CREATE UNIQUE INDEX idx_cards_one_report_per_wave
    ON cards (wave_id) WHERE role = 'reportcard';
