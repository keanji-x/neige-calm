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
        // 5 = SQLITE_BUSY, 6 = SQLITE_LOCKED. Plain code 6 includes the
        // shared-cache unlock_notify deadlock ("database is deadlocked",
        // #930). Retrying on it is ONLY safe at BEGIN, where the fresh tx
        // holds nothing; a mid-transaction statement retry keeps the tx's
        // table locks and re-deadlocks deterministically (see
        // deadlock_semantics_tests).
        return matches!(code & 0xFF, 5 | 6);
    }
    matches!(code, "SQLITE_BUSY" | "SQLITE_LOCKED")
        || code.starts_with("SQLITE_BUSY_")
        || code.starts_with("SQLITE_LOCKED_")
}

// ---- helpers -----------------------------------------------------------------

/// Tier-A upgrade stability guard: refuse to boot when `_sqlx_migrations`
/// contains a `version` not known to the binary's embedded `Migrator`.
///
/// This is the "old binary reading new DB" case from
/// `docs/upgrade-stability.md`. Downgrade is unsupported — once the user's
/// data has been migrated forward, an older binary must not continue
/// against a schema it can't reason about.
///
/// Behavior:
///
/// * The `_sqlx_migrations` table may not exist on a brand-new DB (sqlx
///   creates it on first `run()`). Treat absence as "no applied migrations
///   yet" — opens normally.
/// * `success = false` rows are still included in the diff: their `version`
///   is what matters for "binary doesn't know about". A half-applied future
///   migration is still a future migration.
/// * If multiple unknown versions exist, the error names the lowest one
///   (most useful for debugging: it's the first row that drifted past
///   what this binary knows). The remaining unknown versions are appended
///   in parentheses so operators can see the full extent of the drift.
pub(super) async fn check_no_unknown_future_migrations(
    pool: &SqlitePool,
    migrator: &sqlx::migrate::Migrator,
) -> Result<()> {
    // Does `_sqlx_migrations` exist? `sqlite_master` is always present.
    // We pre-check existence rather than catching the "no such table"
    // error so we don't conflate it with a real driver failure.
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
        // List the remaining unknown versions so operators can see the
        // full forward-drift surface without grepping the DB themselves.
        let rest: Vec<String> = unknown[1..].iter().map(|v| v.to_string()).collect();
        format!(" (additional unknown versions: {})", rest.join(", "))
    };
    Err(CalmError::Internal(format!(
        "database has migration {lowest} applied that this binary doesn't know about \
         — refusing to boot; downgrade is not supported{detail}"
    )))
}

/// Compute the next sort value (max + 1) within a scoped table.
///
/// `scope_sql` is appended verbatim after `FROM <table>`; supply `""` for
/// global scope, or `"WHERE cove_id = ?1"` etc. Bind a single optional
/// scope parameter via `scope_id`.
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
