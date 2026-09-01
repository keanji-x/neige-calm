-- #1189 S1: admit `assistant` into the cards.role whitelist.
--
-- Migration 0037 installed `cards_role_validate_insert` /
-- `cards_role_validate_update`, which RAISE(ABORT) on any role outside
-- worker|spec|reportcard. Minting a `CardRole::Assistant` card would
-- abort at RUNTIME with no compile-time signal. 0037 is already applied
-- in production and sqlx checksums the whole file, so it must not be
-- edited: drop and rebuild the two triggers here instead, unchanged
-- apart from the widened whitelist and the ABORT text.
DROP TRIGGER IF EXISTS cards_role_validate_insert;
DROP TRIGGER IF EXISTS cards_role_validate_update;

CREATE TRIGGER cards_role_validate_insert
BEFORE INSERT ON cards
WHEN NEW.role NOT IN ('worker', 'spec', 'reportcard', 'assistant')
BEGIN
  SELECT RAISE(ABORT, 'cards.role must be one of worker|spec|reportcard|assistant (#585, #1189)');
END;

CREATE TRIGGER cards_role_validate_update
BEFORE UPDATE OF role ON cards
WHEN NEW.role NOT IN ('worker', 'spec', 'reportcard', 'assistant')
BEGIN
  SELECT RAISE(ABORT, 'cards.role must be one of worker|spec|reportcard|assistant (#585, #1189)');
END;
