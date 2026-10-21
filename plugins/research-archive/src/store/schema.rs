//! The frozen schema of `archive.sqlite3` and the check that a file matches it.
//!
//! `DDL` is frozen at release, byte for byte: SQLite keeps each statement's
//! text in `sqlite_schema`, and `Store::open` compares a file's schema with
//! the one this const produces. A later change ships as a new `user_version`
//! with its own migration, never as an edit here.

use std::collections::BTreeMap;

use sqlx::{Connection, Row, SqliteConnection};

/// The schema, guard triggers and `user_version` of a new archive.
///
/// Timestamps are integer Unix milliseconds UTC; `as_of` is an ISO date.
/// Every table is `STRICT` (SQLite 3.37 or later), so a column holds only
/// its declared type.
///
/// **Append-only rule: no table has a uniqueness surface its guard does not
/// check.** Each table's `BEFORE INSERT` guard checks every PRIMARY KEY and
/// UNIQUE constraint, and no table has an implicit rowid: `run` and
/// `run_end` key on `INTEGER PRIMARY KEY` (the rowid itself), and every
/// other table is `WITHOUT ROWID`. So an insert that would collide with any
/// stored row meets the guard first, and a `BEFORE INSERT` trigger runs
/// before conflict resolution: `INSERT OR REPLACE`, `REPLACE` and
/// `ON CONFLICT DO UPDATE` cannot delete or rewrite a row, whether
/// `recursive_triggers` is on or off. `BEFORE UPDATE` and `BEFORE DELETE`
/// guards abort every other change. `blob`'s insert guard ignores a
/// duplicate (content addressing); the others abort.
///
/// **What this guarantees.** The guards and the CHECKs hold for ordinary SQL
/// on a connection with SQLite's default trigger and check settings. They
/// guard against mistakes and ordinary writers, not against a writer that
/// turns them off: `SQLITE_DBCONFIG_ENABLE_TRIGGER` off (the CLI's
/// `.dbconfig enable_trigger off`) lets updates and deletes through,
/// `PRAGMA ignore_check_constraints = ON` lets closed-set violations in, and
/// `PRAGMA writable_schema`, schema statements (`DROP TRIGGER`,
/// `DROP TABLE`, `ALTER TABLE`) and editing the file's bytes are not
/// covered either. `Store::open` refuses a file whose schema has drifted,
/// but cannot see rows written while a guard or check was off.
pub const DDL: &str = r#"
CREATE TABLE endpoint (
    endpoint_id  TEXT NOT NULL PRIMARY KEY CHECK (length(endpoint_id) > 0),
    method       TEXT NOT NULL,
    path         TEXT NOT NULL,
    query_json   TEXT NOT NULL,
    headers_json TEXT NOT NULL,
    dc_restrict  TEXT,
    key_kind     TEXT NOT NULL CHECK (key_kind IN ('index', 'security'))
) STRICT, WITHOUT ROWID;

CREATE TABLE blob (
    sha256 TEXT NOT NULL PRIMARY KEY
        CHECK (length(sha256) = 64 AND sha256 NOT GLOB '*[^0-9a-f]*'),
    body   BLOB NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE run (
    run_id       INTEGER NOT NULL PRIMARY KEY,
    source       TEXT NOT NULL,
    as_of        TEXT NOT NULL
        CHECK (as_of GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'
               AND date(as_of) IS NOT NULL AND date(as_of) = as_of),
    window_start INTEGER NOT NULL,
    window_end   INTEGER NOT NULL CHECK (window_end > window_start),
    started_at   INTEGER NOT NULL,
    UNIQUE (source, as_of)
) STRICT;

CREATE TABLE run_key (
    run_id INTEGER NOT NULL REFERENCES run (run_id),
    key    TEXT NOT NULL CHECK (length(key) > 0),
    origin TEXT NOT NULL CHECK (
        origin IN ('config', 'index-symbol')
        OR (substr(origin, 1, 6) = 'index:' AND length(origin) > 6)
    ),
    PRIMARY KEY (run_id, key, origin)
) STRICT, WITHOUT ROWID;

CREATE TABLE run_membership (
    run_id        INTEGER NOT NULL REFERENCES run (run_id),
    index_sym     TEXT NOT NULL CHECK (length(index_sym) > 0),
    mode          TEXT NOT NULL CHECK (mode IN ('observed', 'carried', 'none')),
    source_run_id INTEGER REFERENCES run (run_id),
    frozen_at     INTEGER NOT NULL,
    PRIMARY KEY (run_id, index_sym),
    CHECK ((mode = 'carried') = (source_run_id IS NOT NULL))
) STRICT, WITHOUT ROWID;

CREATE TABLE observation (
    run_id      INTEGER NOT NULL REFERENCES run (run_id),
    key         TEXT NOT NULL CHECK (length(key) > 0),
    endpoint_id TEXT NOT NULL REFERENCES endpoint (endpoint_id),
    attempt     INTEGER NOT NULL CHECK (attempt >= 1),
    fetched_at  INTEGER NOT NULL,
    status      TEXT NOT NULL CHECK (status IN (
        'ok', 'transient', 'throttled', 'auth_unavailable', 'region_restricted', 'permanent'
    )),
    sha256      TEXT REFERENCES blob (sha256),
    error_class TEXT CHECK (error_class IN (
        'envelope', 'http_status', 'transport', 'unexpected_response', 'auth',
        'dc_region_restricted', 'client', 'incomplete_membership'
    )),
    error_code  TEXT CHECK (length(error_code) > 0),
    PRIMARY KEY (run_id, key, endpoint_id, attempt),
    CHECK (CASE WHEN status = 'ok'
        THEN sha256 IS NOT NULL AND error_class IS NULL AND error_code IS NULL
        ELSE error_class IS NOT NULL AND error_code IS NOT NULL END),
    CHECK (error_class NOT IN ('envelope', 'http_status')
        OR error_code NOT GLOB '*[^0-9-]*'),
    CHECK (error_class <> 'incomplete_membership' OR status = 'permanent'),
    -- Bytes are kept for an ok body and for a membership body that failed
    -- the completeness rule, and for nothing else.
    CHECK ((sha256 IS NOT NULL) = (status = 'ok' OR error_class IS 'incomplete_membership')),
    CHECK ((error_class IS 'dc_region_restricted') = (status = 'region_restricted'))
) STRICT, WITHOUT ROWID;

CREATE INDEX observation_ok_by_key ON observation (key, endpoint_id, fetched_at)
    WHERE status = 'ok';

CREATE TABLE run_end (
    run_id      INTEGER NOT NULL PRIMARY KEY REFERENCES run (run_id),
    finished_at INTEGER NOT NULL,
    outcome     TEXT NOT NULL CHECK (outcome IN ('complete', 'partial', 'missed')),
    reason      TEXT
) STRICT;

CREATE TRIGGER endpoint_no_update BEFORE UPDATE ON endpoint
BEGIN SELECT RAISE(ABORT, 'endpoint is append-only'); END;
CREATE TRIGGER endpoint_no_delete BEFORE DELETE ON endpoint
BEGIN SELECT RAISE(ABORT, 'endpoint is append-only'); END;
CREATE TRIGGER endpoint_no_duplicate BEFORE INSERT ON endpoint
WHEN EXISTS (SELECT 1 FROM endpoint WHERE endpoint_id = NEW.endpoint_id)
BEGIN SELECT RAISE(ABORT, 'endpoint row exists'); END;

CREATE TRIGGER blob_no_update BEFORE UPDATE ON blob
BEGIN SELECT RAISE(ABORT, 'blob is append-only'); END;
CREATE TRIGGER blob_no_delete BEFORE DELETE ON blob
BEGIN SELECT RAISE(ABORT, 'blob is append-only'); END;
CREATE TRIGGER blob_dedup BEFORE INSERT ON blob
WHEN EXISTS (SELECT 1 FROM blob WHERE sha256 = NEW.sha256)
BEGIN SELECT RAISE(IGNORE); END;

CREATE TRIGGER run_no_update BEFORE UPDATE ON run
BEGIN SELECT RAISE(ABORT, 'run is append-only'); END;
CREATE TRIGGER run_no_delete BEFORE DELETE ON run
BEGIN SELECT RAISE(ABORT, 'run is append-only'); END;
CREATE TRIGGER run_no_duplicate BEFORE INSERT ON run
WHEN EXISTS (
    SELECT 1 FROM run
    WHERE run_id = NEW.run_id OR (source = NEW.source AND as_of = NEW.as_of)
)
BEGIN SELECT RAISE(ABORT, 'run row exists'); END;

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

CREATE TRIGGER run_membership_no_update BEFORE UPDATE ON run_membership
BEGIN SELECT RAISE(ABORT, 'run_membership is append-only'); END;
CREATE TRIGGER run_membership_no_delete BEFORE DELETE ON run_membership
BEGIN SELECT RAISE(ABORT, 'run_membership is append-only'); END;
CREATE TRIGGER run_membership_no_duplicate BEFORE INSERT ON run_membership
WHEN EXISTS (
    SELECT 1 FROM run_membership WHERE run_id = NEW.run_id AND index_sym = NEW.index_sym
)
BEGIN SELECT RAISE(ABORT, 'run_membership row exists'); END;

CREATE TRIGGER observation_no_update BEFORE UPDATE ON observation
BEGIN SELECT RAISE(ABORT, 'observation is append-only'); END;
CREATE TRIGGER observation_no_delete BEFORE DELETE ON observation
BEGIN SELECT RAISE(ABORT, 'observation is append-only'); END;
CREATE TRIGGER observation_no_duplicate BEFORE INSERT ON observation
WHEN EXISTS (
    SELECT 1 FROM observation
    WHERE run_id = NEW.run_id AND key = NEW.key
      AND endpoint_id = NEW.endpoint_id AND attempt = NEW.attempt
)
BEGIN SELECT RAISE(ABORT, 'observation row exists'); END;

CREATE TRIGGER run_end_no_update BEFORE UPDATE ON run_end
BEGIN SELECT RAISE(ABORT, 'run_end is append-only'); END;
CREATE TRIGGER run_end_no_delete BEFORE DELETE ON run_end
BEGIN SELECT RAISE(ABORT, 'run_end is append-only'); END;
CREATE TRIGGER run_end_no_duplicate BEFORE INSERT ON run_end
WHEN EXISTS (SELECT 1 FROM run_end WHERE run_id = NEW.run_id)
BEGIN SELECT RAISE(ABORT, 'run_end row exists'); END;

PRAGMA user_version = 1;
"#;

/// Version 2 adds a per-run endpoint snapshot. The released v1 DDL above
/// remains byte-frozen. On upgrade, snapshot the old catalog BEFORE adding
/// any new endpoint, so historical coverage and ever_planned stay truthful.
pub const MIGRATION_V2: &str = r#"
CREATE TABLE run_endpoint (
    run_id INTEGER NOT NULL REFERENCES run (run_id),
    endpoint_id TEXT NOT NULL REFERENCES endpoint (endpoint_id),
    PRIMARY KEY (run_id, endpoint_id)
) STRICT, WITHOUT ROWID;
CREATE TRIGGER run_endpoint_no_update BEFORE UPDATE ON run_endpoint
BEGIN SELECT RAISE(ABORT, 'run_endpoint is append-only'); END;
CREATE TRIGGER run_endpoint_no_delete BEFORE DELETE ON run_endpoint
BEGIN SELECT RAISE(ABORT, 'run_endpoint is append-only'); END;
CREATE TRIGGER run_endpoint_no_duplicate BEFORE INSERT ON run_endpoint
WHEN EXISTS (SELECT 1 FROM run_endpoint WHERE run_id = NEW.run_id AND endpoint_id = NEW.endpoint_id)
BEGIN SELECT RAISE(ABORT, 'run_endpoint row exists'); END;
INSERT INTO run_endpoint (run_id, endpoint_id)
SELECT r.run_id, e.endpoint_id FROM run r CROSS JOIN endpoint e;
PRAGMA user_version = 2;
"#;

/// One schema object: `(type, name)` → `(tbl_name, sql)` from `sqlite_schema`.
pub(crate) type Schema = BTreeMap<(String, String), (String, Option<String>)>;

/// The schema objects of the connected database, including the automatic
/// indexes (whose `sql` is NULL).
pub(crate) async fn read_schema(conn: &mut SqliteConnection) -> Result<Schema, sqlx::Error> {
    let rows = sqlx::query("SELECT type, name, tbl_name, sql FROM sqlite_schema")
        .fetch_all(conn)
        .await?;
    rows.iter()
        .map(|r| {
            Ok((
                (r.try_get("type")?, r.try_get("name")?),
                (r.try_get("tbl_name")?, r.try_get("sql")?),
            ))
        })
        .collect()
}

pub(crate) async fn user_version(conn: &mut SqliteConnection) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(conn)
        .await
}

/// The schema and `user_version` that `DDL` produces, built in a private
/// in-memory database. Comparing against this, rather than a second copy of
/// the statements, keeps `DDL` the only definition.
pub(crate) async fn reference() -> Result<(Schema, i64), sqlx::Error> {
    reference_version(3).await
}

pub(crate) async fn reference_v1() -> Result<(Schema, i64), sqlx::Error> {
    reference_version(1).await
}

pub(crate) async fn reference_v2() -> Result<(Schema, i64), sqlx::Error> {
    reference_version(2).await
}

async fn reference_version(version: i64) -> Result<(Schema, i64), sqlx::Error> {
    let mut conn = SqliteConnection::connect("sqlite::memory:").await?;
    sqlx::Executor::execute(&mut conn, DDL).await?;
    if version >= 2 {
        sqlx::Executor::execute(&mut conn, MIGRATION_V2).await?;
    }
    if version >= 3 {
        sqlx::Executor::execute(&mut conn, super::schema_v3::MIGRATION_V3).await?;
    }
    let schema = read_schema(&mut conn).await?;
    let version = user_version(&mut conn).await?;
    conn.close().await?;
    Ok((schema, version))
}

/// How a file's schema differs from the compiled one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaDrift {
    /// `(type, name)` of objects the file lacks.
    pub missing: Vec<(String, String)>,
    /// `(type, name)` of objects the file has in addition.
    pub extra: Vec<(String, String)>,
    /// `(type, name)` of objects whose table or SQL text differs.
    pub altered: Vec<(String, String)>,
}

impl SchemaDrift {
    pub(crate) fn between(expected: &Schema, found: &Schema) -> Option<SchemaDrift> {
        let missing: Vec<_> = expected
            .keys()
            .filter(|k| !found.contains_key(*k))
            .cloned()
            .collect();
        let extra: Vec<_> = found
            .keys()
            .filter(|k| !expected.contains_key(*k))
            .cloned()
            .collect();
        let altered: Vec<_> = expected
            .iter()
            .filter(|(k, v)| found.get(*k).is_some_and(|f| f != *v))
            .map(|(k, _)| k.clone())
            .collect();
        if missing.is_empty() && extra.is_empty() && altered.is_empty() {
            None
        } else {
            Some(SchemaDrift {
                missing,
                extra,
                altered,
            })
        }
    }
}

impl std::fmt::Display for SchemaDrift {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "missing {:?}, extra {:?}, altered {:?}",
            self.missing, self.extra, self.altered
        )
    }
}
