-- A Planner card's transcript split into conversations (#2774). Conversation 0 is implicit;
-- conversation k holds the card's harness_items with after_item_id(k) < id <= after_item_id(k+1),
-- and the highest ordinal is the current conversation, unbounded above.
CREATE TABLE planner_conversation_starts (
  card_id TEXT NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
  ordinal INTEGER NOT NULL CHECK (ordinal >= 1),
  after_item_id INTEGER NOT NULL CHECK (after_item_id >= 0),
  started_at_ms INTEGER NOT NULL,
  PRIMARY KEY (card_id, ordinal)
);
