-- Track-local business identities for Planner semantic dispatch. These rows
-- preserve creation provenance; task declarations and state remain elsewhere.
CREATE TABLE planner_dispatch_receipts (
    track_id TEXT NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    name TEXT NOT NULL CHECK(length(CAST(name AS BLOB)) BETWEEN 1 AND 200),
    contract_json TEXT NOT NULL CHECK(json_valid(contract_json)),
    task_key TEXT NOT NULL,
    report_card_id TEXT NOT NULL,
    block_id TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY(track_id, name),
    UNIQUE(track_id, task_key)
);
