-- #985 slice 6 repair round 2: durable two-phase wave deletion.
--
-- The marker survives a process crash between external teardown and the
-- final database delete.  The terminal sweeper resumes every marked wave.
CREATE TABLE wave_deletions (
  wave_id         TEXT PRIMARY KEY REFERENCES waves(id) ON DELETE CASCADE,
  requested_at_ms INTEGER NOT NULL
);

-- Once the marker commits, no new teardown-owned resource may attach to the
-- wave.  Rust writers return a typed Conflict before reaching these triggers;
-- the triggers close the pool/autocommit race and protect raw/internal writers.
CREATE TRIGGER wave_deleting_blocks_child_insert
BEFORE INSERT ON waves
WHEN NEW.parent_wave_id IS NOT NULL
 AND EXISTS (SELECT 1 FROM wave_deletions d WHERE d.wave_id = NEW.parent_wave_id)
BEGIN
  SELECT RAISE(ABORT, 'parent wave is deleting');
END;

CREATE TRIGGER wave_deleting_blocks_child_update
BEFORE UPDATE OF parent_wave_id ON waves
WHEN NEW.parent_wave_id IS NOT NULL
 AND EXISTS (SELECT 1 FROM wave_deletions d WHERE d.wave_id = NEW.parent_wave_id)
BEGIN
  SELECT RAISE(ABORT, 'parent wave is deleting');
END;

CREATE TRIGGER wave_deleting_blocks_card_insert
BEFORE INSERT ON cards
WHEN EXISTS (SELECT 1 FROM wave_deletions d WHERE d.wave_id = NEW.wave_id)
BEGIN
  SELECT RAISE(ABORT, 'wave is deleting');
END;

CREATE TRIGGER wave_deleting_blocks_terminal_insert
BEFORE INSERT ON terminals
WHEN EXISTS (
  SELECT 1 FROM cards c JOIN wave_deletions d ON d.wave_id = c.wave_id
  WHERE c.id = NEW.card_id
)
BEGIN
  SELECT RAISE(ABORT, 'wave is deleting');
END;

CREATE TRIGGER wave_deleting_blocks_session_insert
BEFORE INSERT ON worker_sessions
WHEN EXISTS (SELECT 1 FROM wave_deletions d WHERE d.wave_id = NEW.wave_id)
BEGIN
  SELECT RAISE(ABORT, 'wave is deleting');
END;

CREATE TRIGGER wave_deleting_blocks_lease_insert
BEFORE INSERT ON workspace_leases
WHEN EXISTS (SELECT 1 FROM wave_deletions d WHERE d.wave_id = NEW.wave_id)
BEGIN
  SELECT RAISE(ABORT, 'wave is deleting');
END;
