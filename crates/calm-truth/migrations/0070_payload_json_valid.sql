-- #1016: make "payload holds valid JSON" an engine-enforced invariant.
--
-- `wave_detail` is one statement that assembles its `cards` / `overlays`
-- arrays with `printf` and splices the stored `payload` TEXT in RAW — sqlite
-- copies the bytes instead of parsing them into JSONB and rendering them back
-- out, which is ~40% of the statement's cost on payload-heavy waves.
--
-- The shape it replaced (`json_object(..., 'payload', json(c.payload), ...)`)
-- leaned on the same invariant, but the engine checked it on every read: a
-- row whose payload was not valid JSON made `json()` raise. A raw splice does
-- not check, and text that is not a single JSON value would not merely
-- garble the read — `{}},{"id":"...", ...` closes the card object and opens
-- another one, i.e. it could fabricate a card in the result.
--
-- Every writer already serializes a `serde_json::Value` (or writes the output
-- of a sqlite `json_*` function), so no row today can break this. These
-- triggers are the same defense-in-depth as `cards_role_validate_*` in
-- migration 0037: they move the invariant from "all current writers happen to
-- do the right thing" to "the database refuses anything else", so the read
-- side can splice without re-validating. The cost is one `json_valid` parse
-- per payload WRITE, on a path that just serialized that same JSON.
CREATE TRIGGER cards_payload_json_valid_insert
BEFORE INSERT ON cards
WHEN NOT json_valid(NEW.payload)
BEGIN
  SELECT RAISE(ABORT, 'cards.payload must be valid JSON (#1016)');
END;

CREATE TRIGGER cards_payload_json_valid_update
BEFORE UPDATE OF payload ON cards
WHEN NOT json_valid(NEW.payload)
BEGIN
  SELECT RAISE(ABORT, 'cards.payload must be valid JSON (#1016)');
END;

CREATE TRIGGER overlays_payload_json_valid_insert
BEFORE INSERT ON overlays
WHEN NOT json_valid(NEW.payload)
BEGIN
  SELECT RAISE(ABORT, 'overlays.payload must be valid JSON (#1016)');
END;

CREATE TRIGGER overlays_payload_json_valid_update
BEFORE UPDATE OF payload ON overlays
WHEN NOT json_valid(NEW.payload)
BEGIN
  SELECT RAISE(ABORT, 'overlays.payload must be valid JSON (#1016)');
END;
