-- 0161_read_receipts.sql
--
-- #2596 — read receipts move from the browser's localStorage into the kernel, so a refresh or a
-- second device reads the same state. Single user, like `activity_dismissals` (0121).
--
-- One row per read target. `kind` is the closed set `track` | `conversation` (`ReadReceiptKind`
-- checks the same set in Rust). `target_id` has no foreign key: a conversation id names cards of
-- several kinds. `read_through_ms` only rises (a mark read stores the max); `manual_unread` is the
-- reader's explicit "mark unread", cleared by the next mark read.
CREATE TABLE read_receipts (
    kind            TEXT    NOT NULL CHECK (kind IN ('track', 'conversation')),
    target_id       TEXT    NOT NULL,
    read_through_ms INTEGER NOT NULL CHECK (read_through_ms >= 0),
    manual_unread   INTEGER NOT NULL CHECK (manual_unread IN (0, 1)),
    PRIMARY KEY (kind, target_id)
) WITHOUT ROWID;

-- The cutover instant: activity at or before it reads as read, so the switch from browser
-- receipts lights nothing up. One row, written here and never rewritten.
CREATE TABLE read_receipt_baseline (
    singleton   INTEGER PRIMARY KEY CHECK (singleton = 1),
    baseline_ms INTEGER NOT NULL
);

INSERT INTO read_receipt_baseline (singleton, baseline_ms)
VALUES (1, CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER));
