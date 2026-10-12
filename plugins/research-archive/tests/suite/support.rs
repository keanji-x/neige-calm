//! Shared fixtures: a Store in a private directory, typed shorthands, a raw
//! connection that bypasses the Store, and a byte-exact dump of the file.

use std::path::{Path, PathBuf};

use chrono::NaiveDate;
use research_archive::catalog::{Catalog, EndpointId};
use research_archive::store::{PlanRequest, RunPlan};
use research_archive::types::{
    AsOf, ErrorClass, ErrorDetail, FailureClass, Key, Membership, Response, Source, UnixMs, Window,
};
use research_archive::{Store, StoreError};
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{ConnectOptions, Row, SqliteConnection};

pub const DAY: i64 = 86_400_000;
pub const TABLES: [&str; 7] = [
    "endpoint",
    "blob",
    "run",
    "run_key",
    "run_membership",
    "observation",
    "run_end",
];

pub struct Fixture {
    _dir: tempfile::TempDir,
    pub path: PathBuf,
    pub store: Store,
}

/// A new archive in a private directory under `TMPDIR`.
pub async fn fresh() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("archive.sqlite3");
    let store = Store::open(&path).await.unwrap();
    Fixture {
        _dir: dir,
        path,
        store,
    }
}

pub fn key(s: &str) -> Key {
    Key::new(s).unwrap()
}

pub fn keys(s: &[&str]) -> Vec<Key> {
    s.iter().map(|k| key(k)).collect()
}

pub fn endpoint(id: &str) -> EndpointId {
    Catalog::compiled().get(id).unwrap().id
}

/// Day `n` of the archive's test calendar (2026-01-01 + n).
pub fn as_of(n: i64) -> AsOf {
    AsOf::new(NaiveDate::from_ymd_opt(2026, 1, 1).unwrap() + chrono::Days::new(n as u64)).unwrap()
}

/// The window of day `n`: `[n·DAY, n·DAY + 16 h)`.
pub fn window(n: i64) -> Window {
    Window::new(UnixMs(n * DAY), UnixMs(n * DAY + 16 * 3_600_000)).unwrap()
}

/// An instant `offset_ms` into day `n`'s window.
pub fn at(n: i64, offset_ms: i64) -> UnixMs {
    UnixMs(n * DAY + offset_ms)
}

pub fn plan_request(n: i64, config: &[&str], indexes: &[&str]) -> PlanRequest {
    PlanRequest {
        source: Source::Longbridge,
        as_of: as_of(n),
        window: window(n),
        started_at: at(n, 1),
        config: keys(config),
        indexes: keys(indexes),
    }
}

/// A response body (stored as `ok` unless it is an incomplete membership).
pub fn ok(body: &[u8]) -> Response {
    Response::Body(body.to_vec())
}

/// A complete `index_constituents` body listing `members`.
pub fn constituents(members: &[&str]) -> Vec<u8> {
    let stocks: Vec<String> = members
        .iter()
        .map(|m| format!("{{\"symbol\":\"{m}\"}}"))
        .collect();
    format!(
        "{{\"total\":{},\"stocks\":[{}]}}",
        members.len(),
        stocks.join(",")
    )
    .into_bytes()
}

/// A failure outcome with a plausible detail for its class.
pub fn failed(class: FailureClass) -> Response {
    let (error_class, code) = match class {
        FailureClass::Throttled => (ErrorClass::Envelope, "429002"),
        FailureClass::Transient => (ErrorClass::Transport, "request_timeout"),
        FailureClass::AuthUnavailable => (ErrorClass::Auth, "invalid_access_token"),
        FailureClass::RegionRestricted => (ErrorClass::DcRegionRestricted, "dc_region_restricted"),
        FailureClass::Permanent => (ErrorClass::HttpStatus, "404"),
    };
    Response::Failed {
        class,
        error: ErrorDetail {
            class: error_class,
            code: code.into(),
        },
    }
}

/// Records an `ok` constituents observation per index at `frozen_at`, then
/// freezes the run at `frozen_at`. Panics on failure.
pub async fn observe_and_freeze(
    store: &Store,
    plan: &RunPlan,
    frozen_at: UnixMs,
    indexes: &[(&str, &[&str])],
) -> Vec<(Key, Membership)> {
    for (index, members) in indexes {
        store
            .record_observation(
                plan.run_id,
                &key(index),
                endpoint("index_constituents"),
                frozen_at,
                &ok(&constituents(members)),
            )
            .await
            .unwrap();
    }
    store
        .freeze_membership(plan.run_id, frozen_at)
        .await
        .unwrap()
}

/// A closed archive with at least one row in every table: day 1 plans
/// `AAPL.US` and `.SPX.US`, freezes `.SPX.US` = {AAPL.US, MSFT.US}, records
/// one consensus body, and ends `partial`.
pub async fn populated() -> (tempfile::TempDir, PathBuf) {
    let f = fresh().await;
    let plan = f
        .store
        .plan_run(&plan_request(1, &["AAPL.US"], &[".SPX.US"]))
        .await
        .unwrap();
    observe_and_freeze(
        &f.store,
        &plan,
        at(1, 10),
        &[(".SPX.US", &["AAPL.US", "MSFT.US"])],
    )
    .await;
    f.store
        .record_observation(
            plan.run_id,
            &key("AAPL.US"),
            endpoint("consensus"),
            at(1, 20),
            &ok(b"{\"eps\":1.2345678901234567890}"),
        )
        .await
        .unwrap();
    f.store
        .end_run(
            plan.run_id,
            at(1, 30),
            research_archive::types::Finish::Partial,
            Some("window closed"),
        )
        .await
        .unwrap();
    f.store.close().await;
    for table in TABLES {
        let mut conn = raw(&f.path, false).await;
        let n: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
            .fetch_one(&mut conn)
            .await
            .unwrap();
        assert!(n > 0, "{table} has no row");
    }
    (f._dir, f.path)
}

/// A plain connection to the file, as any SQLite client would open it:
/// foreign keys off, `recursive_triggers` as given and read back.
pub async fn raw(path: &Path, recursive_triggers: bool) -> SqliteConnection {
    let mut conn = SqliteConnectOptions::new()
        .filename(path)
        .foreign_keys(false)
        .pragma(
            "recursive_triggers",
            if recursive_triggers { "ON" } else { "OFF" },
        )
        .connect()
        .await
        .unwrap();
    let on: i64 = sqlx::query_scalar("PRAGMA recursive_triggers")
        .fetch_one(&mut conn)
        .await
        .unwrap();
    assert_eq!(on == 1, recursive_triggers);
    conn
}

/// Every row of every table as SQL literals (`quote()`), sorted, so two
/// dumps are equal only when the stored bytes are. No table has a hidden
/// rowid: `run` and `run_end` key on it, the rest are `WITHOUT ROWID`.
pub async fn dump(conn: &mut SqliteConnection) -> Vec<String> {
    let mut out = Vec::new();
    for table in TABLES {
        let cols: Vec<String> = sqlx::query(&format!("PRAGMA table_info({table})"))
            .fetch_all(&mut *conn)
            .await
            .unwrap()
            .iter()
            .map(|r| format!("quote({})", r.get::<String, _>("name")))
            .collect();
        let sql = format!(
            "SELECT '{table}|' || {} FROM {table}",
            cols.join(" || '|' || ")
        );
        let mut rows: Vec<String> = sqlx::query_scalar(&sql)
            .fetch_all(&mut *conn)
            .await
            .unwrap();
        rows.sort();
        out.extend(rows);
    }
    out
}

/// The error message of a Store error that came from SQLite.
pub fn sqlite_message(e: &StoreError) -> String {
    match e {
        StoreError::Sqlite(sqlx::Error::Database(d)) => d.message().to_owned(),
        other => panic!("expected a SQLite error, got {other:?}"),
    }
}
