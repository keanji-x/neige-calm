PRAGMA foreign_keys=OFF;

CREATE TABLE harness_items_new (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    runtime_id    TEXT    NOT NULL,
    card_id       TEXT    NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
    wave_id       TEXT    NOT NULL,
    thread_id     TEXT    NOT NULL,
    turn_id       TEXT,
    item_uuid     TEXT,
    item_type     TEXT,
    method        TEXT    NOT NULL,
    params        TEXT    NOT NULL,
    created_at_ms INTEGER NOT NULL
);

DELETE FROM harness_items
WHERE card_id NOT IN (SELECT id FROM cards);

INSERT INTO harness_items_new
SELECT id, runtime_id, card_id, wave_id, thread_id, turn_id,
       item_uuid, item_type, method, params, created_at_ms
FROM harness_items;

DROP TABLE harness_items;
ALTER TABLE harness_items_new RENAME TO harness_items;

CREATE INDEX idx_harness_items_runtime_id ON harness_items(runtime_id, id);
CREATE INDEX idx_harness_items_card_id    ON harness_items(card_id, id);

PRAGMA foreign_keys=ON;
