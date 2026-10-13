//! `Store`: the single owner of `archive.sqlite3`: its schema, guard
//! triggers and catalog check, and every write the capture job makes.

mod freeze;
mod read;
pub mod schema;
mod write;

use std::path::{Path, PathBuf};
use std::time::Duration;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{Row, SqliteConnection};

use crate::catalog::{Catalog, Endpoint, EndpointId};
use crate::membership::IncompleteMembership;
use crate::types::{InvalidValue, Key, RunId, Source, UnixMs};
pub use read::{FrozenIndex, PairCoverage, PointRead, RunEnd, RunSummary, Universe};
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
    #[error("the archive did not switch to WAL; journal mode is {0}")]
    JournalMode(String),
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
    #[error("the window overlaps or is out of as_of order with run {with}")]
    WindowConflict { with: RunId },
    #[error("a plan started at {started_at:?} is outside its window")]
    PlanOutsideWindow { started_at: UnixMs },
    #[error("{at:?} is outside the window of run {run}")]
    OutsideWindow { run: RunId, at: UnixMs },
    #[error("{at:?} is before the clock of run {run}, now at {mark:?}")]
    ClockWentBack {
        run: RunId,
        at: UnixMs,
        mark: UnixMs,
    },
    #[error("error class {class} cannot come from endpoint {endpoint}")]
    ErrorClassForEndpoint {
        endpoint: String,
        class: &'static str,
    },
    #[error(
        "the stored ok constituents body of {index} (run {source_run}, attempt {attempt}) is not a membership: {reason}"
    )]
    StoredMembershipInvalid {
        source_run: RunId,
        index: Key,
        attempt: i64,
        reason: IncompleteMembership,
    },
    #[error(
        "run {run} is not complete: frozen {frozen}, {pairs_without_rows} planned pairs have no row"
    )]
    NotComplete {
        run: RunId,
        frozen: bool,
        pairs_without_rows: usize,
    },
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
    /// `<data_dir>/archive.sqlite3`, where the capture job keeps the archive.
    pub fn path(data_dir: &Path) -> PathBuf {
        data_dir.join("archive.sqlite3")
    }

    /// Opens the archive at `path` against the compiled catalog.
    ///
    /// A missing file is created, and a new (empty) file gets the schema and
    /// the catalog in one transaction: the first capture creates the
    /// archive. An existing file is refused unless its `user_version`, its
    /// whole schema (every table, index and guard trigger, by SQL text) and
    /// its `endpoint` rows match this build. A refused file is left as it
    /// was: the WAL journal mode is set only after the check passes.
    pub async fn open(path: &Path) -> std::result::Result<Store, OpenError> {
        Store::open_with_catalog(path, &Catalog::compiled()).await
    }

    /// `open` against another catalog. Only the catalog-drift tests use it.
    #[doc(hidden)]
    pub async fn open_with_catalog(
        path: &Path,
        catalog: &Catalog,
    ) -> std::result::Result<Store, OpenError> {
        // No `journal_mode` here: setting it would rewrite a file we may
        // refuse. `foreign_keys` and `synchronous` are per connection.
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
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
            sqlx::Executor::execute(&mut *tx, schema::DDL).await?;
            for e in self.catalog.endpoints() {
                insert_endpoint(&mut tx, e).await?;
            }
            tx.commit().await?;
            return self.enable_wal().await;
        }
        self.check_matches(
            &mut tx,
            (expected_schema, expected_version),
            found_version,
            found_schema,
        )
        .await?;
        tx.commit().await?;
        self.enable_wal().await
    }

    /// Refuses a file whose `user_version`, whole schema or `endpoint` rows
    /// differ from this build. The one check behind both opens.
    async fn check_matches(
        &self,
        conn: &mut SqliteConnection,
        (expected_schema, expected_version): (schema::Schema, i64),
        found_version: i64,
        found_schema: schema::Schema,
    ) -> std::result::Result<(), OpenError> {
        if found_version != expected_version {
            return Err(OpenError::UserVersion {
                found: found_version,
                expected: expected_version,
            });
        }
        if let Some(drift) = SchemaDrift::between(&expected_schema, &found_schema) {
            return Err(OpenError::Schema(drift));
        }
        if let Some(drift) = self.catalog_drift(conn).await? {
            return Err(OpenError::Catalog(drift));
        }
        Ok(())
    }

    /// Switches the accepted file to WAL (persistent in the file).
    async fn enable_wal(&self) -> std::result::Result<(), OpenError> {
        let mode: String = sqlx::query_scalar("PRAGMA journal_mode = WAL")
            .fetch_one(&self.pool)
            .await?;
        if mode == "wal" {
            Ok(())
        } else {
            Err(OpenError::JournalMode(mode))
        }
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

/// The archive opened read-only, for a reader that is not the capture job:
/// the status plugin. It has the Store's reads and none of its writes.
///
/// The file is opened `SQLITE_OPEN_READONLY` and never created: a missing
/// file is an error, not an empty archive. Nothing changes the file: no
/// schema, no catalog rows, no journal mode. A reader of a WAL file may
/// create the `-wal` and `-shm` sidecars when they are missing (SQLite
/// 3.22 and later), which needs a writable directory. The same checks as
/// [`Store::open`] refuse a file of another version, schema or catalog.
#[derive(Clone, Debug)]
pub struct ReadOnlyStore {
    store: Store,
}

impl ReadOnlyStore {
    pub async fn open(path: &Path) -> std::result::Result<ReadOnlyStore, OpenError> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .read_only(true)
            .create_if_missing(false)
            .busy_timeout(Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?;
        let store = Store {
            pool,
            catalog: Catalog::compiled(),
        };
        match ReadOnlyStore::check(&store).await {
            Ok(()) => Ok(ReadOnlyStore { store }),
            Err(e) => {
                store.pool.close().await;
                Err(e)
            }
        }
    }

    async fn check(store: &Store) -> std::result::Result<(), OpenError> {
        let expected = schema::reference().await?;
        // A deferred transaction: one snapshot, and no write lock.
        let mut tx = store.pool.begin().await?;
        let found_version = schema::user_version(&mut tx).await?;
        let found_schema = schema::read_schema(&mut tx).await?;
        store
            .check_matches(&mut tx, expected, found_version, found_schema)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn close(self) {
        self.store.close().await;
    }

    /// See [`Store::recent_runs`].
    pub async fn recent_runs(&self, source: Source, limit: u32) -> Result<Vec<RunSummary>> {
        self.store.recent_runs(source, limit).await
    }

    /// See [`Store::coverage`].
    pub async fn coverage(&self, run_id: RunId) -> Result<Vec<PairCoverage>> {
        self.store.coverage(run_id).await
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
