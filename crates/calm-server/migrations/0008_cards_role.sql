-- Wave-as-Actor PR3 (#136): card role gate.
--
-- `cards.role` carries the kernel's authorization label for each card.
-- Three values today:
--
--   * 'plain'  — default; cards the user adds via the UI, terminal cards,
--                pre-PR3 history. The role gate places no extra restrictions
--                on writes from these cards' implicit actors.
--   * 'spec'   — the wave's "spec card" (PR6 will mint exactly one per wave
--                at wave-create time). Only spec cards may emit
--                `WaveUpdated`; this is the structural choke point that
--                stops AI workers from editing wave-level metadata.
--   * 'worker' — dispatcher-spawned worker cards (PR5). May only emit
--                events whose scope is the card itself; never wave-scoped.
--
-- Existing cards default to 'plain' (PR6 will mint spec cards on wave
-- creation; PR5 will mint worker cards via the dispatcher). PR3 just lands
-- the column + gate.

ALTER TABLE cards ADD COLUMN role TEXT NOT NULL DEFAULT 'plain';

-- Only one spec card per wave (PR6's invariant; landing the index here so
-- the PR3 tests can assert it and PR6's wave-create path can rely on it as
-- a backstop in case the application-level mint races itself). Partial
-- index over `role = 'spec'` so the cost is proportional to the number of
-- spec cards, not the whole table.
CREATE UNIQUE INDEX idx_cards_one_spec_per_wave
    ON cards(wave_id) WHERE role = 'spec';
