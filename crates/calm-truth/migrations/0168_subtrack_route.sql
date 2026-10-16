-- The subtrack route migration must update Automerge and its JSON projection together.
-- Rust applies this named data migration before repository open returns; SQL cannot edit CRDT.
CREATE TABLE report_data_migrations (
    name TEXT PRIMARY KEY NOT NULL,
    completed_at_ms INTEGER NOT NULL
);
