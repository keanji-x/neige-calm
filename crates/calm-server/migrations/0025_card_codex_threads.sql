CREATE TABLE card_codex_threads (
    thread_id TEXT PRIMARY KEY,
    card_id   TEXT NOT NULL UNIQUE REFERENCES cards(id) ON DELETE CASCADE,
    role      TEXT NOT NULL CHECK (role IN ('plain', 'spec', 'worker')),
    wave_id   TEXT REFERENCES waves(id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE INDEX idx_card_codex_threads_card ON card_codex_threads(card_id);
CREATE INDEX idx_card_codex_threads_role_wave ON card_codex_threads(role, wave_id);

INSERT INTO card_codex_threads (thread_id, card_id, role, wave_id, created_at, updated_at)
SELECT json_extract(c.payload, '$.codex_thread_id'),
       c.id,
       c.role,
       c.wave_id,
       c.created_at,
       c.created_at
FROM cards c
WHERE json_extract(c.payload, '$.codex_thread_id') IS NOT NULL
  AND trim(json_extract(c.payload, '$.codex_thread_id')) != ''
  AND c.role IN ('plain', 'spec', 'worker');
