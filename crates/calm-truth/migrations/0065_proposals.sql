-- Issue #955 §5 (PR-a) — `proposals` projection table.
--
-- The event log (`proposal.submitted` / `proposal.resolved`) stays the
-- sole truth; this rebuildable projection was removed by migration 0066
-- when the proposal channel was withdrawn in #973.

CREATE TABLE proposals (
    proposal_id        TEXT PRIMARY KEY,
    wave_id            TEXT NOT NULL,
    plugin_id          TEXT NOT NULL,
    subject_kind       TEXT NOT NULL,
    base_doc_heads     TEXT NOT NULL,
    -- JSON serialization of the event's `ops` array (Vec<ProposalOp>).
    ops                TEXT NOT NULL,
    note               TEXT NOT NULL,
    idem_key           TEXT NOT NULL,
    -- pending | accepted | rejected | stale | withdrawn
    status             TEXT NOT NULL DEFAULT 'pending',
    submitted_event_id INTEGER NOT NULL,
    resolved_event_id  INTEGER,
    created_at         INTEGER NOT NULL,
    resolved_at        INTEGER
);

-- Pending list per wave (adjudication UI, PR-b REST).
CREATE INDEX idx_proposals_wave_pending
    ON proposals(wave_id) WHERE status = 'pending';

-- Per-(plugin, wave) pending quota count, read inside the submit tx.
CREATE INDEX idx_proposals_plugin_wave_pending
    ON proposals(plugin_id, wave_id) WHERE status = 'pending';

-- Pending-scoped idempotency: only ONE pending proposal may hold a
-- given (plugin, wave, idem_key); resolution releases the key.
CREATE UNIQUE INDEX idx_proposals_idem_pending
    ON proposals(plugin_id, wave_id, idem_key) WHERE status = 'pending';
