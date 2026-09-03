-- Historical pre-drop migration: both ratchets intentionally exclude this tree.
CREATE TABLE runtimes (id TEXT PRIMARY KEY, status INTEGER NOT NULL);
UPDATE runtimes SET status = 1;

-- A historical helper name also documents the Rust-only symbol scan boundary:
-- runtime_start_tx
