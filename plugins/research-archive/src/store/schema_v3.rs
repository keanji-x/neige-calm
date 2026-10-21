//! Released v3 migration. Never edit after release.
//!
//! The explicit command disables foreign keys BEFORE its transaction, rebuilds
//! only the two closed-set tables, checks all foreign keys before committing,
//! and restores enforcement. Historical values and all other tables stay intact.
pub const MIGRATION_V3: &str = r#"
CREATE TABLE endpoint_v3 (
    endpoint_id  TEXT NOT NULL PRIMARY KEY CHECK (length(endpoint_id) > 0),
    method       TEXT NOT NULL,
    path         TEXT NOT NULL,
    query_json   TEXT NOT NULL,
    headers_json TEXT NOT NULL,
    dc_restrict  TEXT,
    key_kind     TEXT NOT NULL CHECK (key_kind IN ('index', 'security', 'market'))
) STRICT, WITHOUT ROWID;
INSERT INTO endpoint_v3 SELECT * FROM endpoint;
DROP TABLE endpoint;
ALTER TABLE endpoint_v3 RENAME TO endpoint;
CREATE TRIGGER endpoint_no_update BEFORE UPDATE ON endpoint
BEGIN SELECT RAISE(ABORT, 'endpoint is append-only'); END;
CREATE TRIGGER endpoint_no_delete BEFORE DELETE ON endpoint
BEGIN SELECT RAISE(ABORT, 'endpoint is append-only'); END;
CREATE TRIGGER endpoint_no_duplicate BEFORE INSERT ON endpoint
WHEN EXISTS (SELECT 1 FROM endpoint WHERE endpoint_id = NEW.endpoint_id)
BEGIN SELECT RAISE(ABORT, 'endpoint row exists'); END;

CREATE TABLE run_key_v3 (
    run_id INTEGER NOT NULL REFERENCES run (run_id),
    key    TEXT NOT NULL CHECK (length(key) > 0),
    origin TEXT NOT NULL CHECK (
        origin IN ('config', 'index-symbol', 'benchmark', 'market')
        OR (substr(origin, 1, 6) = 'index:' AND length(origin) > 6)
    ),
    PRIMARY KEY (run_id, key, origin)
) STRICT, WITHOUT ROWID;
INSERT INTO run_key_v3 SELECT * FROM run_key;
DROP TABLE run_key;
ALTER TABLE run_key_v3 RENAME TO run_key;
CREATE TRIGGER run_key_no_update BEFORE UPDATE ON run_key
BEGIN SELECT RAISE(ABORT, 'run_key is append-only'); END;
CREATE TRIGGER run_key_no_delete BEFORE DELETE ON run_key
BEGIN SELECT RAISE(ABORT, 'run_key is append-only'); END;
CREATE TRIGGER run_key_no_duplicate BEFORE INSERT ON run_key
WHEN EXISTS (
    SELECT 1 FROM run_key
    WHERE run_id = NEW.run_id AND key = NEW.key AND origin = NEW.origin
)
BEGIN SELECT RAISE(ABORT, 'run_key row exists'); END;

CREATE TABLE run_plan_v3 (
    run_id INTEGER NOT NULL PRIMARY KEY REFERENCES run (run_id)
) STRICT, WITHOUT ROWID;
CREATE TRIGGER run_plan_v3_no_update BEFORE UPDATE ON run_plan_v3
BEGIN SELECT RAISE(ABORT, 'run_plan_v3 is append-only'); END;
CREATE TRIGGER run_plan_v3_no_delete BEFORE DELETE ON run_plan_v3
BEGIN SELECT RAISE(ABORT, 'run_plan_v3 is append-only'); END;
CREATE TRIGGER run_plan_v3_no_duplicate BEFORE INSERT ON run_plan_v3
WHEN EXISTS (SELECT 1 FROM run_plan_v3 WHERE run_id = NEW.run_id)
BEGIN SELECT RAISE(ABORT, 'run_plan_v3 row exists'); END;

CREATE TABLE run_pair (
    run_id INTEGER NOT NULL REFERENCES run (run_id),
    key TEXT NOT NULL CHECK (length(key) > 0),
    endpoint_id TEXT NOT NULL REFERENCES endpoint (endpoint_id),
    request_json TEXT NOT NULL CHECK (json_valid(request_json)),
    PRIMARY KEY (run_id, key, endpoint_id)
) STRICT, WITHOUT ROWID;
CREATE TRIGGER run_pair_no_update BEFORE UPDATE ON run_pair
BEGIN SELECT RAISE(ABORT, 'run_pair is append-only'); END;
CREATE TRIGGER run_pair_no_delete BEFORE DELETE ON run_pair
BEGIN SELECT RAISE(ABORT, 'run_pair is append-only'); END;
CREATE TRIGGER run_pair_no_duplicate BEFORE INSERT ON run_pair
WHEN EXISTS (SELECT 1 FROM run_pair WHERE run_id = NEW.run_id AND key = NEW.key AND endpoint_id = NEW.endpoint_id)
BEGIN SELECT RAISE(ABORT, 'run_pair row exists'); END;

PRAGMA user_version = 3;
"#;
