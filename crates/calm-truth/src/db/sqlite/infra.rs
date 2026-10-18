use std::time::Duration;

use sqlx::Row;
use sqlx::Sqlite;
use sqlx::SqlitePool;
use sqlx::Transaction;

use crate::error::{CalmError, Result};

mod observation;
pub use observation::ObservedTransaction;

/// Begin a writing transaction with the existing bounded busy-retry policy.
#[track_caller]
pub fn begin_immediate_tx<'a>(
    pool: &'a SqlitePool,
) -> impl std::future::Future<Output = Result<ObservedTransaction<'static>>> + Send + 'a {
    let caller = std::panic::Location::caller();
    async move {
        begin_observed(pool, caller, RetryPolicy::Bounded)
            .await
            .map_err(Into::into)
    }
}

/// One observed BEGIN attempt; the caller retains ownership of retry and cancellation policy.
#[track_caller]
pub fn begin_immediate_tx_once<'a>(
    pool: &'a SqlitePool,
) -> impl std::future::Future<Output = sqlx::Result<ObservedTransaction<'static>>> + Send + 'a {
    let caller = std::panic::Location::caller();
    begin_observed(pool, caller, RetryPolicy::Once)
}

enum RetryPolicy {
    Once,
    Bounded,
}

async fn begin_observed(
    pool: &SqlitePool,
    caller: &'static std::panic::Location<'static>,
    retry: RetryPolicy,
) -> sqlx::Result<ObservedTransaction<'static>> {
    use observation::{Observation, Phase};
    let retries = match retry {
        RetryPolicy::Once => 0,
        RetryPolicy::Bounded => 6,
    };
    let mut backoff = Duration::from_millis(10);
    let mut observation = Observation::new(caller);

    for attempt in 0..=retries {
        match begin_attempt(pool, &mut observation).await {
            Ok(transaction) => return Ok(ObservedTransaction::new(transaction, observation)),
            Err(error) if is_sqlite_busy(&error) && attempt < retries => {
                tracing::debug!(
                    attempt,
                    "sqlite: BEGIN IMMEDIATE hit transient writer contention; retrying"
                );
                observation.phase(Phase::Backoff);
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_millis(250));
            }
            Err(error) => {
                observation.admission_error();
                return Err(error);
            }
        }
    }
    unreachable!("bounded retry loop must return or error");
}

async fn begin_attempt(
    pool: &SqlitePool,
    observation: &mut observation::Observation,
) -> sqlx::Result<Transaction<'static, Sqlite>> {
    observation.phase(observation::Phase::Acquire);
    let connection = pool.acquire().await?;
    observation.phase(observation::Phase::Begin);
    // Mirrors sqlx 0.8.6 Pool::begin_with exactly, splitting acquisition from BEGIN.
    // This public doc-hidden constructor owns the pooled connection and preserves
    // SQLx cancellation/Drop cleanup. Verify this seam when upgrading sqlx.
    Transaction::begin(
        connection,
        Some(std::borrow::Cow::Borrowed("BEGIN IMMEDIATE")),
    )
    .await
}

pub fn is_sqlite_busy(e: &sqlx::Error) -> bool {
    let Some(db_err) = e.as_database_error() else {
        return false;
    };
    db_err.code().as_deref().is_some_and(is_sqlite_busy_code)
}

pub(super) fn is_sqlite_busy_code(code: &str) -> bool {
    if let Ok(code) = code.parse::<i64>() {
        // 5 = SQLITE_BUSY, 6 = SQLITE_LOCKED (plain 6 includes the shared-cache "database is deadlocked"). Retrying on it
        // is ONLY safe at BEGIN, where the fresh tx holds nothing; a mid-transaction statement retry re-deadlocks deterministically.
        return matches!(code & 0xFF, 5 | 6);
    }
    matches!(code, "SQLITE_BUSY" | "SQLITE_LOCKED")
        || code.starts_with("SQLITE_BUSY_")
        || code.starts_with("SQLITE_LOCKED_")
}

/// Refuse to boot when `_sqlx_migrations` contains a `version` not known to the binary's embedded `Migrator`
/// (downgrade is unsupported). A missing table means no applied migrations yet; `success = false` rows still count.
pub(super) async fn check_no_unknown_future_migrations(
    pool: &SqlitePool,
    migrator: &sqlx::migrate::Migrator,
) -> Result<()> {
    // Pre-check existence rather than catching "no such table", so it is not conflated with a real driver failure.
    let table_exists: Option<(String,)> = sqlx::query_as(
        r#"SELECT name FROM sqlite_master
           WHERE type = 'table' AND name = '_sqlx_migrations'"#,
    )
    .fetch_optional(pool)
    .await?;
    if table_exists.is_none() {
        return Ok(());
    }

    let applied: Vec<(i64,)> =
        sqlx::query_as(r#"SELECT version FROM _sqlx_migrations ORDER BY version ASC"#)
            .fetch_all(pool)
            .await?;

    let known: std::collections::HashSet<i64> = migrator.iter().map(|m| m.version).collect();
    let mut unknown: Vec<i64> = applied
        .into_iter()
        .map(|(v,)| v)
        .filter(|v| !known.contains(v))
        .collect();
    if unknown.is_empty() {
        return Ok(());
    }
    unknown.sort_unstable();
    let lowest = unknown[0];
    let detail = if unknown.len() == 1 {
        String::new()
    } else {
        let rest: Vec<String> = unknown[1..].iter().map(|v| v.to_string()).collect();
        format!(" (additional unknown versions: {})", rest.join(", "))
    };
    Err(CalmError::Internal(format!(
        "database has migration {lowest} applied that this binary doesn't know about \
         — refusing to boot; downgrade is not supported{detail}"
    )))
}

/// Compute the next sort value (max + 1) within a scoped table; `scope_sql` is appended verbatim after `FROM <table>`.
pub(super) async fn next_sort_scoped_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    table: &str,
    scope_sql: &str,
    scope_id: Option<&str>,
) -> Result<f64> {
    let sql = format!("SELECT COALESCE(MAX(sort), 0.0) + 1.0 AS s FROM {table} {scope_sql}");
    let mut q = sqlx::query(&sql);
    if let Some(id) = scope_id {
        q = q.bind(id);
    }
    let row = q.fetch_one(&mut **tx).await?;
    Ok(row.try_get::<f64, _>("s")?)
}
