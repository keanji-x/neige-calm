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
/// its declared type. Every table has a `BEFORE UPDATE` and a `BEFORE DELETE`
/// guard that aborts, and a `BEFORE INSERT` guard that fires when the
/// primary-key (or UNIQUE) row exists: `blob` ignores the insert, every other
/// table aborts. A `BEFORE INSERT` trigger runs before conflict resolution,
/// so the guard also stops `INSERT OR REPLACE` and `ON CONFLICT DO UPDATE`
/// whatever `recursive_triggers` is.
pub const DDL: &str = r#"
CREATE TABLE endpoint (
    endpoint_id  TEXT NOT NULL PRIMARY KEY CHECK (length(endpoint_id) > 0),
    method       TEXT NOT NULL,
    path         TEXT NOT NULL,
    query_json   TEXT NOT NULL,
    headers_json TEXT NOT NULL,
    dc_restrict  TEXT,
    key_kind     TEXT NOT NULL CHECK (key_kind IN ('index', 'security'))
) STRICT;

CREATE TABLE blob (
    sha256 TEXT NOT NULL PRIMARY KEY
        CHECK (length(sha256) = 64 AND sha256 NOT GLOB '*[^0-9a-f]*'),
    body   BLOB NOT NULL
) STRICT;

CREATE TABLE run (
    run_id       INTEGER NOT NULL PRIMARY KEY,
    source       TEXT NOT NULL,
    as_of        TEXT NOT NULL
        CHECK (as_of GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]' AND date(as_of) = as_of),
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
) STRICT;

CREATE TABLE run_membership (
    run_id        INTEGER NOT NULL REFERENCES run (run_id),
    index_sym     TEXT NOT NULL CHECK (length(index_sym) > 0),
    mode          TEXT NOT NULL CHECK (mode IN ('observed', 'carried', 'none')),
    source_run_id INTEGER REFERENCES run (run_id),
    frozen_at     INTEGER NOT NULL,
    PRIMARY KEY (run_id, index_sym),
    CHECK ((mode = 'carried') = (source_run_id IS NOT NULL))
) STRICT;

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
    error_class TEXT,
    error_code  TEXT,
    PRIMARY KEY (run_id, key, endpoint_id, attempt),
    CHECK (status <> 'ok' OR (sha256 IS NOT NULL AND error_class IS NULL AND error_code IS NULL))
) STRICT;

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
    let mut conn = SqliteConnection::connect("sqlite::memory:").await?;
    sqlx::raw_sql(DDL).execute(&mut conn).await?;
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
