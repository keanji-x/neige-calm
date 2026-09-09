-- One kernel-issued repair pair per original producer. Private execution provenance.
CREATE TABLE task_candidate_repairs (
    id TEXT PRIMARY KEY NOT NULL,
    track_id TEXT NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    producer_key TEXT NOT NULL,
    repair_key TEXT NOT NULL,
    review_key TEXT NOT NULL,
    receipt_json TEXT NOT NULL CHECK(json_valid(receipt_json)),
    UNIQUE(track_id, producer_key),
    UNIQUE(track_id, repair_key),
    UNIQUE(track_id, review_key)
);
CREATE TRIGGER task_candidate_repair_immutable BEFORE UPDATE ON task_candidate_repairs
BEGIN SELECT RAISE(ABORT, 'candidate repair receipt is immutable'); END;
