//! `Store`: the single owner of `archive.sqlite3`: its schema, guard
//! triggers and catalog check, and every write the capture job makes.

mod read;
pub mod schema;
mod write;

use std::path::Path;
use std::time::Duration;

use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteSynchronous,
};
use sqlx::{Row, SqliteConnection};

use crate::catalog::{Catalog, Endpoint, EndpointId};
use crate::types::{InvalidValue, Key, RunId};
pub use read::{PairCoverage, PointRead, Universe};
pub use schema::SchemaDrift;
pub use write::{PlanRequest, RunPlan};

/// Why `Store::open` refused a file.
#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] sqlx::Error),
    #[error("archive user_version is {found}, this build reads {expected}")]
    UserVersion { found: i64, expected: i64 },
    #[error("archive schema differs from the compiled DDL: {0}")]
    Schema(SchemaDrift),
    #[error("archive endpoint catalog differs from the compiled catalog: {0}")]
    Catalog(CatalogDrift),
}

/// How the stored `endpoint` rows differ from the compiled catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogDrift {
    /// Compiled ids with no stored row.
    pub missing: Vec<String>,
    /// Stored ids the catalog lacks.
    pub extra: Vec<String>,
    /// Ids whose stored row differs in any column.
    pub changed: Vec<String>,
}

impl std::fmt::Display for CatalogDrift {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "missing {:?}, extra {:?}, changed {:?}",
            self.missing, self.extra, self.changed
        )
    }
}

/// Why a Store call wrote nothing.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] sqlx::Error),
    #[error("stored value does not decode: {0}")]
    Corrupt(#[from] InvalidValue),
    #[error("run {0} does not exist")]
    UnknownRun(RunId),
    #[error("run {0} has ended")]
    RunEnded(RunId),
    #[error("run {0} is already frozen")]
    AlreadyFrozen(RunId),
    #[error("run {run} does not plan endpoint {endpoint} for key {key}")]
    NotPlanned {
        run: RunId,
        key: Key,
        endpoint: String,
    },
    #[error("freeze of run {run} names indexes {given:?}, the plan has {planned:?}")]
    FreezeIndexes {
        run: RunId,
        planned: Vec<Key>,
        given: Vec<Key>,
    },
    #[error("run {source_run} holds no ok constituents observation for {index}")]
    NoMembershipObservation { source_run: RunId, index: Key },
    #[error("a carry of {index} into run {run} must point to an earlier run")]
    CarryNotEarlier { run: RunId, index: Key },
    #[error("run {0} already exists for this source and day")]
    RunExists(RunId),
    #[error("the window of a missed day must have closed")]
    WindowOpen,
}

pub(crate) type Result<T> = std::result::Result<T, StoreError>;

/// The archive, opened for writing. Cloning shares the one connection.
#[derive(Clone, Debug)]
pub struct Store {
    pool: SqlitePool,
    catalog: Catalog,
}

impl Store {
    /// Opens `path` in WAL mode. A new (empty) file gets the schema and the
    /// catalog in one transaction. An existing file is refused unless its
    /// `user_version`, its whole schema (every table, index and guard
    /// trigger, by SQL text) and its `endpoint` rows match this build.
    pub async fn open(path: &Path, catalog: &Catalog) -> std::result::Result<Store, OpenError> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Full)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(5));
        // One connection: the job is the only writer, and every call is one
        // transaction on it.
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?;
        let store = Store {
            pool,
            catalog: catalog.clone(),
        };
        match store.check_or_create().await {
            Ok(()) => Ok(store),
            Err(e) => {
                store.pool.close().await;
                Err(e)
            }
        }
    }

    /// Closes the connection, checkpointing the WAL.
    pub async fn close(self) {
        self.pool.close().await;
    }

    pub fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    async fn check_or_create(&self) -> std::result::Result<(), OpenError> {
        let (expected_schema, expected_version) = schema::reference().await?;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let found_version = schema::user_version(&mut tx).await?;
        let found_schema = schema::read_schema(&mut tx).await?;
        if found_version == 0 && found_schema.is_empty() {
            sqlx::raw_sql(schema::DDL).execute(&mut *tx).await?;
            for e in self.catalog.endpoints() {
                insert_endpoint(&mut tx, e).await?;
            }
            tx.commit().await?;
            return Ok(());
        }
        if found_version != expected_version {
            return Err(OpenError::UserVersion {
                found: found_version,
                expected: expected_version,
            });
        }
        if let Some(drift) = SchemaDrift::between(&expected_schema, &found_schema) {
            return Err(OpenError::Schema(drift));
        }
        if let Some(drift) = self.catalog_drift(&mut tx).await? {
            return Err(OpenError::Catalog(drift));
        }
        tx.commit().await?;
        Ok(())
    }

    async fn catalog_drift(
        &self,
        conn: &mut SqliteConnection,
    ) -> std::result::Result<Option<CatalogDrift>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT endpoint_id, method, path, query_json, headers_json, dc_restrict, key_kind
             FROM endpoint ORDER BY endpoint_id",
        )
        .fetch_all(conn)
        .await?;
        let mut stored = std::collections::BTreeMap::new();
        for r in &rows {
            let id: String = r.try_get("endpoint_id")?;
            let row: EndpointRow = (
                r.try_get("method")?,
                r.try_get("path")?,
                r.try_get("query_json")?,
                r.try_get("headers_json")?,
                r.try_get("dc_restrict")?,
                r.try_get("key_kind")?,
            );
            stored.insert(id, row);
        }
        let mut drift = CatalogDrift {
            missing: Vec::new(),
            extra: Vec::new(),
            changed: Vec::new(),
        };
        for e in self.catalog.endpoints() {
            match stored.remove(e.id.as_str()) {
                None => drift.missing.push(e.id.as_str().to_owned()),
                Some(row) if row != endpoint_row(e) => drift.changed.push(e.id.as_str().to_owned()),
                Some(_) => {}
            }
        }
        drift.extra.extend(stored.into_keys());
        let clean = drift.missing.is_empty() && drift.extra.is_empty() && drift.changed.is_empty();
        Ok((!clean).then_some(drift))
    }

    /// The catalog entry for a stored `endpoint_id`. `open` proved the stored
    /// rows equal the catalog, so a miss means the file changed under us.
    fn endpoint_id(&self, stored: &str) -> Result<EndpointId> {
        self.catalog.get(stored).map(|e| e.id).ok_or_else(|| {
            StoreError::Corrupt(InvalidValue {
                what: "endpoint_id",
                value: stored.to_owned(),
            })
        })
    }
}

/// The non-id columns of an `endpoint` row, in table order.
type EndpointRow = (String, String, String, String, Option<String>, String);

fn endpoint_row(e: &Endpoint) -> EndpointRow {
    (
        e.method.as_str().to_owned(),
        e.path.to_owned(),
        e.query_json(),
        e.headers_json(),
        e.dc_restrict.map(|d| d.as_str().to_owned()),
        e.key_kind.as_str().to_owned(),
    )
}

async fn insert_endpoint(conn: &mut SqliteConnection, e: &Endpoint) -> sqlx::Result<()> {
    let (method, path, query, headers, dc, kind) = endpoint_row(e);
    sqlx::query(
        "INSERT INTO endpoint
         (endpoint_id, method, path, query_json, headers_json, dc_restrict, key_kind)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(e.id.as_str())
    .bind(method)
    .bind(path)
    .bind(query)
    .bind(headers)
    .bind(dc)
    .bind(kind)
    .execute(conn)
    .await?;
    Ok(())
}
