CREATE TABLE report_read_ledger (
    worker_session_id TEXT NOT NULL REFERENCES worker_sessions(id) ON DELETE CASCADE,
    report_card_id TEXT NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
    doc_rev INTEGER NOT NULL CHECK (doc_rev >= 0),
    whole INTEGER NOT NULL CHECK (whole IN (0, 1)),
    sections_json TEXT NOT NULL CHECK (json_valid(sections_json)),
    PRIMARY KEY (worker_session_id, report_card_id)
);
CREATE INDEX report_read_ledger_report ON report_read_ledger(report_card_id);
CREATE TABLE report_read_ledger_blocks (
    worker_session_id TEXT NOT NULL,
    report_card_id TEXT NOT NULL,
    block_id TEXT NOT NULL,
    rev INTEGER NOT NULL CHECK (rev BETWEEN 0 AND 4294967295),
    history_ref TEXT NOT NULL CHECK (json_valid(history_ref)),
    PRIMARY KEY (worker_session_id, report_card_id, block_id),
    FOREIGN KEY (worker_session_id, report_card_id)
        REFERENCES report_read_ledger(worker_session_id, report_card_id) ON DELETE CASCADE
);
