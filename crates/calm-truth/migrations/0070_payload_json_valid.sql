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
-- A trigger only guards writes made AFTER it exists, so "hereafter payload is
-- valid JSON" is worth nothing to a read path that also splices rows written
-- BEFORE it. Enumerating today's writers is an argument, not a check, so this
-- migration first SCANS the existing `cards` / `overlays` rows and ABORTS —
-- fail-closed, taking the whole migration (and therefore startup) with it —
-- if a single stored payload fails `json_valid`. Only a database that has been
-- scanned clean gets the triggers, so from here on every row in both tables,
-- old and new, has been checked.
--
-- `RAISE()` is only callable from a trigger program and its message must be a
-- string literal, so the scan runs one probe per table: rows that fail
-- `json_valid` are inserted into a temp table whose BEFORE INSERT trigger
-- raises. No offending row, no insert, no abort. The message names the table
-- and carries the query that lists the offending ids.
CREATE TEMP TABLE _migration_0070_cards_probe (id TEXT NOT NULL, payload TEXT NOT NULL);
CREATE TEMP TRIGGER _migration_0070_cards_abort
BEFORE INSERT ON _migration_0070_cards_probe
BEGIN
  SELECT RAISE(ABORT, 'migration 0070 aborted: cards.payload holds text that is not valid JSON. List the offending rows with: SELECT id FROM cards WHERE NOT json_valid(payload);');
END;
INSERT INTO _migration_0070_cards_probe
SELECT id, payload FROM cards WHERE NOT json_valid(payload);
DROP TRIGGER _migration_0070_cards_abort;
DROP TABLE _migration_0070_cards_probe;

CREATE TEMP TABLE _migration_0070_overlays_probe (id TEXT NOT NULL, payload TEXT NOT NULL);
CREATE TEMP TRIGGER _migration_0070_overlays_abort
BEFORE INSERT ON _migration_0070_overlays_probe
BEGIN
  SELECT RAISE(ABORT, 'migration 0070 aborted: overlays.payload holds text that is not valid JSON. List the offending rows with: SELECT id FROM overlays WHERE NOT json_valid(payload);');
END;
INSERT INTO _migration_0070_overlays_probe
SELECT id, payload FROM overlays WHERE NOT json_valid(payload);
DROP TRIGGER _migration_0070_overlays_abort;
DROP TABLE _migration_0070_overlays_probe;

-- `json_valid` is the exact property the raw splice needs — well-formed JSON
-- *text*, which is what makes `{}},{"id":…` impossible — and deliberately not
-- more. It also passes scalars (`123`, `"s"`, `true`, `null`), and that is
-- correct here: `null` is a payload the kernel writes itself (the direct-create
-- card route defaults an absent body payload to `Value::Null`), a spliced
-- scalar still yields well-formed JSON in the array, and `Card::payload` /
-- `Overlay::payload` are `serde_json::Value`, which accepts it. Tightening to
-- `json_type(payload) = 'object'` would reject payloads the API contract
-- accepts today, and would buy the read path nothing: the `json(c.payload)`
-- shape this replaced passed scalars through just the same.
--
-- These triggers are the same defense-in-depth as `cards_role_validate_*` in
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
