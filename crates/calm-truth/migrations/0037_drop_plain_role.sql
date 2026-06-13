-- #585: collapse CardRole::Plain into Worker. All user-facing card
-- creation paths now mint Worker (matching the +claude flow that
-- already did so). Legacy rows with role='plain' become 'worker'.
UPDATE cards SET role = 'worker' WHERE role = 'plain';

-- Defense in depth: reject any future write that tries to set role to a
-- value the Rust enum can no longer deserialize. The legacy DEFAULT
-- 'plain' from migration 0008 is unreachable today (all writers pass role
-- explicitly), but a hand-written raw INSERT would silently bypass it.
CREATE TRIGGER cards_role_validate_insert
BEFORE INSERT ON cards
WHEN NEW.role NOT IN ('worker', 'spec', 'reportcard')
BEGIN
  SELECT RAISE(ABORT, 'cards.role must be one of worker|spec|reportcard (#585)');
END;

CREATE TRIGGER cards_role_validate_update
BEFORE UPDATE OF role ON cards
WHEN NEW.role NOT IN ('worker', 'spec', 'reportcard')
BEGIN
  SELECT RAISE(ABORT, 'cards.role must be one of worker|spec|reportcard (#585)');
END;
