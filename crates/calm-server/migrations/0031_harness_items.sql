CREATE TABLE IF NOT EXISTS harness_items (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  runtime_id    TEXT    NOT NULL,
  card_id       TEXT    NOT NULL,
  wave_id       TEXT    NOT NULL,
  thread_id     TEXT    NOT NULL,
  turn_id       TEXT,
  item_uuid     TEXT,
  item_type     TEXT,
  method        TEXT    NOT NULL,
  params        TEXT    NOT NULL,
  created_at_ms INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_harness_items_runtime_id
  ON harness_items(runtime_id, id);

CREATE INDEX IF NOT EXISTS idx_harness_items_card_id
  ON harness_items(card_id, id);
