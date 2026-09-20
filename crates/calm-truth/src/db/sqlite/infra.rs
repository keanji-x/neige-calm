use std::time::Duration;

use sqlx::Row;
use sqlx::Sqlite;
use sqlx::SqlitePool;
use sqlx::Transaction;

use crate::error::{CalmError, Result};

pub async fn begin_immediate_tx<'a>(pool: &'a SqlitePool) -> Result<Transaction<'a, Sqlite>> {
    const MAX_RETRIES: usize = 6;
    let mut backoff = Duration::from_millis(10);

    for attempt in 0..=MAX_RETRIES {
        match pool.begin_with("BEGIN IMMEDIATE").await {
            Ok(tx) => return Ok(tx),
            Err(e) if is_sqlite_busy(&e) && attempt < MAX_RETRIES => {
                tracing::debug!(
                    attempt,
                    error = %e,
                    "sqlite: BEGIN IMMEDIATE hit transient writer contention; retrying"
                );
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_millis(250));
            }
            Err(e) => return Err(e.into()),
        }
    }

    unreachable!("bounded retry loop must return or error");
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
