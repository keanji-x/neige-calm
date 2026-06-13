CREATE TABLE worker_flow_cursors (
    card_id          TEXT NOT NULL,
    source_kind      TEXT NOT NULL,
    source_path      TEXT NOT NULL,
    record_index     INTEGER NOT NULL DEFAULT 0,
    byte_offset      INTEGER NOT NULL DEFAULT 0,
    last_source_uuid TEXT,
    updated_at_ms    INTEGER NOT NULL,
    PRIMARY KEY (card_id, source_kind),
    FOREIGN KEY (card_id) REFERENCES cards(id) ON DELETE CASCADE
);
