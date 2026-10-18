-- Archive belongs to a conversation card, independently of its provider sessions.
CREATE TABLE assistant_conversation_archives (
    card_id TEXT PRIMARY KEY NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
    archived_at_ms INTEGER NOT NULL
);
