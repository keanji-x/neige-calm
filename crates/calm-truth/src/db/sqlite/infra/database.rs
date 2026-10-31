//! Shared ownership of SQLite read capacity and fair, single-connection writer admission.
use std::str::FromStr;
use std::sync::Arc;

use futures::FutureExt;
use futures::future::{BoxFuture, Shared};
use std::time::Duration;

use sqlx::sqlite::{
    SqliteConnectOptions, SqliteConnection, SqlitePoolOptions, SqliteTransactionManager,
};
use sqlx::{ConnectOptions, Connection, Executor, SqlitePool, TransactionManager as _};

use crate::error::{CalmError, Result};

use super::super::{SQLITE_ACQUIRE_TIMEOUT_MS, SQLITE_BUSY_TIMEOUT_MS};

/// Clones share both pools and the memory keepalive. Writer ordering applies to each
/// acquisition within this handle, not independent opens, external writers or retries.
/// SQLx owns transaction cancellation and connection cleanup, including the worker barrier.
#[derive(Clone)]
pub struct SqliteDb {
    inner: Arc<Inner>,
}

type AnchorClose = Shared<BoxFuture<'static, ()>>;

struct Inner {
    read: SqlitePool,
    writer: SqlitePool,
    // Both roles must retain the same memory database across connection recovery.
    memory_cache_anchor: Option<AnchorClose>,
}

impl SqliteDb {
    /// Open both connection roles without application schema migrations. Callers own
    /// initialization before publishing the handle; independently opened handles do not
    /// share writer admission. Private in-memory caches cannot share the role schema.
    pub async fn connect(url: &str) -> Result<Self> {
        Self::connect_mode(url, true).await
    }

    /// Operator tooling must never accidentally create a database at a misspelled path.
    pub async fn connect_existing(url: &str) -> Result<Self> {
        Self::connect_mode(url, false).await
    }

    async fn connect_mode(url: &str, create: bool) -> Result<Self> {
        let mut opts = SqliteConnectOptions::from_str(url)
            .map_err(|e| CalmError::Internal(format!("invalid sqlite url {url:?}: {e}")))?
            .create_if_missing(create)
            .foreign_keys(true);
        opts = opts.log_statements(tracing::log::LevelFilter::Debug);
        // A private memory cache cannot share schema/data between the two connection roles.
        // Use SQLx's reported normalized options rather than recognize URL spellings ourselves.
        // SQLx 0.8.6 cannot render some valid filenames as URL authorities. The
        // public filename setter leaves mode/cache unchanged; replace it only on
        // this reporting clone. Connections below retain the original options.
        let normalized = opts.clone().filename("/").to_url_lossy();
        if normalized
            .query_pairs()
            .any(|(key, value)| key == "mode" && value == "memory")
            && normalized
                .query_pairs()
                .any(|(key, value)| key == "cache" && value == "private")
        {
            return Err(CalmError::Internal(
                "SQLite in-memory databases require cache=shared for read and writer connections"
                    .into(),
            ));
        }

        let pool = pool_options().connect_with(opts.clone()).await?;

        // Anchor in-memory shared caches BEFORE anything else touches the pool.
        // `pragma_database_list.file` is empty for in-memory (and temp-file) DBs.
        let mut candidate = pool.acquire().await?;
        let main_db_file: String =
            sqlx::query_scalar("SELECT file FROM pragma_database_list WHERE name = 'main'")
                .fetch_one(&mut *candidate)
                .await?;
        let memory_cache_anchor = if main_db_file.is_empty() {
            Some(candidate.detach())
        } else {
            drop(candidate);
            None
        };

        let writer = pool_options().max_connections(1).connect_with(opts).await?;
        Ok(Self::new(pool, writer, memory_cache_anchor))
    }

    fn new(
        read: SqlitePool,
        writer: SqlitePool,
        memory_cache_anchor: Option<SqliteConnection>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                read,
                writer,
                // Retain the real SQLx close future so cancellation or another clone
                // cannot mistake taking the connection for its close acknowledgement.
                memory_cache_anchor: memory_cache_anchor.map(|anchor| {
                    async move {
                        let _ = anchor.close().await;
                    }
                    .boxed()
                    .shared()
                }),
            }),
        }
    }

    /// Read queries and preparation outside a writing transaction use this capacity.
    pub fn read_pool(&self) -> &SqlitePool {
        &self.inner.read
    }

    /// Domain-owned autocommit mutations (including RETURNING and VACUUM) use this pool.
    /// Its single SQLx lease is held through statement consumption and worker cleanup.
    pub fn writer_pool(&self) -> &SqlitePool {
        &self.inner.writer
    }

    /// Close both shared pools and wait for their checked-out connections to return.
    /// As with SQLx close, this affects every clone of this database handle.
    /// Normal anchor shutdown is shared across concurrent and cancelled callers.
    /// Errors follow SQLx pool's best-effort close semantics and are not returned;
    /// this does not strengthen the driver's exceptional termination guarantees.
    pub async fn close(&self) {
        futures::future::join(self.inner.read.close(), self.inner.writer.close()).await;
        if let Some(anchor_close) = &self.inner.memory_cache_anchor {
            anchor_close.clone().await;
        }
    }

    #[cfg(test)]
    pub(in crate::db::sqlite) fn has_memory_cache_anchor(&self) -> bool {
        self.inner
            .memory_cache_anchor
            .as_ref()
            .is_some_and(|close| close.peek().is_none())
    }
}

// One connection/recovery contract for both roles. SQLx defaults preserve read capacity
// (10), acquisition fairness, and worker ping on return; only writer capacity differs.
fn pool_options() -> SqlitePoolOptions {
    SqlitePoolOptions::new()
        .acquire_timeout(Duration::from_millis(SQLITE_ACQUIRE_TIMEOUT_MS))
        // Re-issue the pragmas per connection: connect options are dropped for some URL forms (e.g. memory).
        .after_connect(|conn, _meta| {
            Box::pin(async move {
                conn.execute("PRAGMA foreign_keys = ON;").await?;
                let busy_timeout_pragma =
                    format!("PRAGMA busy_timeout = {SQLITE_BUSY_TIMEOUT_MS};");
                conn.execute(busy_timeout_pragma.as_str()).await?;
                conn.execute("PRAGMA journal_mode = WAL;").await?;
                Ok(())
            })
        })
        // Self-heal connections released while still inside a transaction (a cancelled
        // `begin_with` leaks one). Roll back rather than discard: dropping an in-memory
        // DB's connection can drop the database.
        .after_release(|conn, _meta| {
            Box::pin(async move {
                if !Connection::is_in_transaction(conn) {
                    return Ok(true);
                }
                // A dropped `Transaction` has only queued its rollback; ping round-trips the
                // worker queue so it completes before we judge the connection leaked.
                Connection::ping(&mut *conn).await?;
                if !Connection::is_in_transaction(conn) {
                    return Ok(true);
                }
                tracing::warn!(
                    "sqlite: connection released to pool inside an open transaction \
                     (cancelled begin?); rolling it back"
                );
                // Bounded unwind: one rollback per depth level covers
                // nested savepoints without risking an infinite loop.
                for _ in 0..8 {
                    SqliteTransactionManager::rollback(conn).await?;
                    if !Connection::is_in_transaction(conn) {
                        return Ok(true);
                    }
                }
                // Fail closed: the Err makes the pool close_hard the connection. Safe for
                // in-memory repos because `_memory_cache_anchor` keeps the cache alive.
                Err(sqlx::Error::Protocol(
                    "connection still inside a transaction after bounded rollback".into(),
                ))
            })
        })
}

#[cfg(test)]
mod tests;
