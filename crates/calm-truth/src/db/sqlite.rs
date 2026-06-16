//! SQLite-backed `Repo` implementation. **Owned by Track A.**
//!
//! Implements every method on the `Repo` trait against a `sqlx::SqlitePool`.
//! The pool is opened with `PRAGMA foreign_keys = ON` per-connection, the
//! bundled migrations under `migrations/` are run on `open()`, and every
//! observable behavior of `MockRepo` (cascades, sort defaulting, not-found
//! semantics, overlay upsert by unique key) is replicated here.
//!
//! ## Sync engine — internal layout
//!
//! Every entity write the trait exposes (`cove_create`, `wave_update`,
//! `card_create`, ...) is implemented as a thin wrapper around a `_tx`-
//! suffixed free function that takes `&mut Transaction<'_, Sqlite>` and
//! does the actual SQL. The wrappers each open their own one-shot
//! transaction (the existing single-call semantics), but the `_tx`
//! functions can also be **composed inside** `Repo::write_with_event`'s
//! closure so the entity write and the `INSERT INTO events ...` run in
//! the same transaction. See `db::mod`'s sync-engine comment.

use async_trait::async_trait;
use futures::future::BoxFuture;
use sqlx::ConnectOptions;
use sqlx::Executor;
use sqlx::QueryBuilder;
use sqlx::Row;
use sqlx::Sqlite;
use sqlx::SqlitePool;
use sqlx::Transaction;
use sqlx::sqlite::SqliteRow;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::collections::HashMap;
use std::str::FromStr;
#[cfg(feature = "worker-session-parity-drop")]
use std::sync::Arc;
#[cfg(feature = "worker-session-parity-drop")]
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::{
    Repo, RepoEventWrite, RepoOutOfDomain, RepoRead, RepoSyncDomainRaw, SessionCardIdentity,
    SharedCodexDaemonRecord, SharedCodexDaemonUpdate, WaveEvent, WriteInTxFn,
    WriteWithActorEventsFn, WriteWithEventFn, WriteWithEventsFn,
};
use crate::card_kind::validate_card_kind_global;
use crate::card_role_cache::CardRoleCache;
use crate::decision_gate::DecisionGate;
use crate::error::{CalmError, Result};
use crate::event::{BroadcastEnvelope, Event, EventBus, EventScope, SYNC_EVENT_VERSION};
use crate::ids::{ActorId, CardId, CoveId, WaveId};
use crate::model::*;
use crate::runtime_repo::{
    AgentProvider, CardId as RuntimeCardId, CardRuntime, Result as RuntimeResult, RunStatus,
    RuntimeId, RuntimeInit, RuntimeKind, RuntimeRepo, RuntimeRepoError, ThreadAttribution,
    Tx as RuntimeTx,
};
use crate::runtime_row::{
    WS_BACKED_CARD_RUNTIME_SELECT, card_runtime_from_row, card_runtime_from_ws_join_row,
    projectable_runtimes_for_cards_from_rows, projectable_runtimes_for_cards_query,
    run_status_from_db,
};
use crate::session_repo::{CommitExitOutcome, DeadRootCandidate, SessionRepo, Tx as SessionTx};
use crate::validation::{
    CLAUDE_PAYLOAD_SCHEMA_VERSION, CODEX_PAYLOAD_SCHEMA_VERSION, TERMINAL_PAYLOAD_SCHEMA_VERSION,
};
use crate::wave_cove_cache::WaveCoveCache;
use crate::wave_vcs;
use calm_types::worker::{
    Liveness, LivenessTag, SessionMode, WorkerContract, WorkerProviderKind, WorkerSession,
    WorkerSessionId, WorkerSessionState,
};

pub struct SqlxRepo {
    pool: SqlitePool,
    /// PR3 (#136) — write-through role cache local to the repo so the
    /// gated `RepoSyncDomainRaw` trait methods (`card_create` /
    /// `card_delete`) can call the `_tx` helpers without every test
    /// fixture having to hand a cache in. Production writes go through
    /// `AppState::card_role_cache` — a separate `Arc<DashMap<…>>`
    /// instance also kept in sync via the `_tx` helpers when the
    /// production `write_with_event` path runs. Both caches converge
    /// on whatever the `cards` table holds, since `seed_from_db`
    /// fully repopulates from sqlite. The duplication is intentional:
    /// `enforce_role` only ever consults the cache passed in at the
    /// call site, so AppState's view stays authoritative for
    /// production while the repo-local view backs the test-only raw
    /// path.
    card_role_cache: CardRoleCache,
    /// #234 — write-through `WaveId -> CoveId` cache, same rationale as
    /// `card_role_cache` above: the raw `RepoSyncDomainRaw` wave write
    /// paths (`wave_create` / `wave_delete`) keep this in sync via the
    /// `_tx` helpers, while production `write_with_event` callers thread
    /// `AppState::wave_cove_cache` (a separate instance that
    /// `AppState::new` seeds from the same pool). Both converge on
    /// the persisted `waves` table.
    wave_cove_cache: WaveCoveCache,
    #[cfg(feature = "worker-session-parity-drop")]
    worker_session_parity_on_drop: Arc<AtomicBool>,
}

impl SqlxRepo {
    /// Open / create the SQLite DB at `url`, run pending migrations, and
    /// enable foreign-key enforcement per-connection.
    ///
    /// Accepts both `sqlite::memory:` (used in tests) and on-disk
    /// `sqlite://path?mode=rwc` URLs.
    pub async fn open(url: &str) -> Result<Self> {
        let mut opts = SqliteConnectOptions::from_str(url)
            .map_err(|e| CalmError::Internal(format!("invalid sqlite url {url:?}: {e}")))?
            .create_if_missing(true)
            .foreign_keys(true);
        // Reduce noise from sqlx's per-statement logging at info; keep debug.
        opts = opts.log_statements(tracing::log::LevelFilter::Debug);

        let pool = SqlitePoolOptions::new()
            // Belt-and-braces: also re-issue the pragmas on every fresh
            // connection in case connect options are silently dropped for
            // some URL forms (e.g. memory).
            .after_connect(|conn, _meta| {
                Box::pin(async move {
                    conn.execute("PRAGMA foreign_keys = ON;").await?;
                    conn.execute("PRAGMA busy_timeout = 5000;").await?;
                    conn.execute("PRAGMA journal_mode = WAL;").await?;
                    Ok(())
                })
            })
            .connect_with(opts)
            .await?;

        // Tier-A upgrade stability boundary (`docs/upgrade-stability.md`):
        // refuse to boot when the DB carries a migration row that this
        // binary doesn't know about. Downgrade is unsupported — an older
        // binary opening a newer DB must fail loudly here rather than
        // continue against a schema it can't reason about. sqlx 0.8.x's
        // own `run()` would also refuse (via `MigrateError::VersionMissing`
        // unless `set_ignore_missing(true)` is set), but we check first so
        // (a) the error message wording is owned by us, not sqlx, and (b)
        // sqlx never gets a chance to apply any pending known migration
        // before we've rejected the open.
        check_no_unknown_future_migrations(&pool, &crate::MIGRATOR).await?;

        crate::MIGRATOR
            .run(&pool)
            .await
            .map_err(|e| CalmError::Internal(format!("migrate: {e}")))?;

        wave_vcs::backfill_existing_waves(&pool).await?;

        // PR3 (#136): seed the repo-local role cache from the freshly-
        // migrated table. This is the backing store for the gated raw
        // path's `card_create_tx` / `card_delete_tx` calls; the
        // production write path uses `AppState::card_role_cache`,
        // which `AppState::new` re-seeds from the same pool.
        let card_role_cache = CardRoleCache::new();
        card_role_cache.seed_from_db(&pool).await?;
        let wave_cove_cache = WaveCoveCache::new();
        wave_cove_cache.seed_from_db(&pool).await?;

        Ok(Self {
            pool,
            card_role_cache,
            wave_cove_cache,
            #[cfg(feature = "worker-session-parity-drop")]
            worker_session_parity_on_drop: Arc::new(AtomicBool::new(true)),
        })
    }

    /// Direct access to the pool for tests / fixtures / sync-engine
    /// integration tests that need to `SELECT` from the `events` table
    /// outside the `Repo` trait surface.
    ///
    /// Marked `#[doc(hidden)]` because production code must go through
    /// the trait (so a future swap to a non-sqlite backend stays
    /// possible). Integration tests under `tests/` need real access for
    /// replay / atomicity assertions; that's what this surface is for.
    #[doc(hidden)]
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// PR3 (#136) — borrow the repo's role cache. `AppState::new` clones
    /// this into its own field so the production write path's `enforce_role`
    /// lookup sees the same map as the repo's `_tx` write-through.
    /// `CardRoleCache: Clone` is cheap (`Arc<DashMap<…>>` under the hood).
    pub fn card_role_cache(&self) -> &CardRoleCache {
        &self.card_role_cache
    }

    #[cfg(feature = "worker-session-parity-drop")]
    pub fn disable_worker_session_parity_on_drop_for_test(&self) {
        self.worker_session_parity_on_drop
            .store(false, Ordering::SeqCst);
    }

    /// #234 — borrow the repo's wave→cove cache. Mirrors
    /// [`card_role_cache`](Self::card_role_cache). `AppState::new`
    /// re-seeds its own clone from the same pool.
    pub fn wave_cove_cache(&self) -> &WaveCoveCache {
        &self.wave_cove_cache
    }

    /// **Private.** The raw events-table insert. Lives off the trait per
    /// design doc §1.4: only `Repo::write_with_event` and
    /// `Repo::log_pure_event` may reach this path, so the commit-then-emit
    /// invariant is unbypassable from the route / plugin host layers.
    ///
    /// Returns the auto-incremented row id, which is then stamped onto
    /// the `BroadcastEnvelope` the wrapper emits on the bus.
    ///
    /// PR2 of #136:
    ///   * `actor` is typed [`ActorId`] and stored as `serde_json::to_string(&actor)`
    ///     in the `events.actor` TEXT column (forward-compatible with future
    ///     actor enrichment).
    ///   * `scope` is decomposed into the four `events.scope_*` columns added
    ///     in migration 0007. `EventScope::System` writes `scope_kind='system'`
    ///     with NULL ancestor cols; the other variants populate whatever
    ///     prefix of the cove → wave → card chain they carry.
    async fn event_append_in_tx(
        tx: &mut Transaction<'_, Sqlite>,
        actor: &ActorId,
        scope: &EventScope,
        correlation: Option<&str>,
        event: &Event,
    ) -> Result<i64> {
        let kind = event.kind_tag();
        let payload = event.payload_value();
        let payload_text = serde_json::to_string(&payload)?;
        let actor_text = serde_json::to_string(actor)?;
        let at = now_ms();
        let scope_kind = scope.kind();
        let scope_cove = scope.cove_id().map(|c| c.as_str());
        let scope_wave = scope.wave_id().map(|w| w.as_str());
        let scope_card = scope.card_id().map(|c| c.as_str());
        let row = sqlx::query(
            r#"INSERT INTO events (
                   kind, payload, actor, at, correlation, event_version,
                   scope_kind, scope_cove, scope_wave, scope_card
               )
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
               RETURNING id"#,
        )
        .bind(kind)
        .bind(&payload_text)
        .bind(&actor_text)
        .bind(at)
        .bind(correlation)
        .bind(SYNC_EVENT_VERSION)
        .bind(scope_kind)
        .bind(scope_cove)
        .bind(scope_wave)
        .bind(scope_card)
        .fetch_one(&mut **tx)
        .await?;
        let id: i64 = row.try_get("id")?;
        Ok(id)
    }

    /// `#[cfg(test)]`-gated raw appender for fixture seeding / replay
    /// loaders. Bypasses the wrapper deliberately so test scaffolds can
    /// reconstruct an event stream verbatim (id-stamped) without driving
    /// the full handler stack.
    #[cfg(test)]
    pub async fn event_append_fixture(
        &self,
        actor: ActorId,
        scope: EventScope,
        correlation: Option<&str>,
        event: &Event,
    ) -> Result<i64> {
        let mut tx = self.pool.begin().await?;
        let id = Self::event_append_in_tx(&mut tx, &actor, &scope, correlation, event).await?;
        tx.commit().await?;
        Ok(id)
    }
}

#[cfg(feature = "worker-session-parity-drop")]
impl Drop for SqlxRepo {
    fn drop(&mut self) {
        if std::thread::panicking() || !self.worker_session_parity_on_drop.load(Ordering::SeqCst) {
            return;
        }

        let pool = self.pool.clone();
        let handle = std::thread::Builder::new()
            .name("worker-session-parity-drop".into())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|e| format!("build parity-drop runtime: {e}"))?;
                rt.block_on(assert_worker_session_parity_for_test(&pool))
            })
            .expect("spawn worker-session parity-drop thread");

        match handle.join() {
            Ok(Ok(())) => {}
            Ok(Err(message)) => {
                panic!(
                    "runtimes/worker_sessions parity divergence at SqlxRepo teardown:\n{message}"
                )
            }
            Err(payload) => std::panic::resume_unwind(payload),
        }
    }
}

#[cfg(feature = "worker-session-parity-drop")]
async fn assert_worker_session_parity_for_test(
    pool: &SqlitePool,
) -> std::result::Result<(), String> {
    let rows = sqlx::query(
        r#"SELECT r.id AS runtime_id,
                  r.status AS runtime_status,
                  ws.state AS session_state,
                  r.thread_id AS runtime_thread_id,
                  ws.thread_id AS session_thread_id,
                  r.session_id AS runtime_session_id,
                  ws.agent_session_id AS session_agent_session_id,
                  r.active_turn_id AS runtime_active_turn_id,
                  ws.active_turn_id AS session_active_turn_id,
                  r.terminal_run_id AS runtime_terminal_run_id,
                  ws.terminal_run_id AS session_terminal_run_id,
                  r.handle_state_json AS runtime_handle_state_json,
                  ws.handle_state_json AS session_handle_state_json,
                  r.created_at_ms AS runtime_created_at_ms,
                  ws.created_at_ms AS session_created_at_ms,
                  r.updated_at_ms AS runtime_updated_at_ms,
                  ws.updated_at_ms AS session_updated_at_ms,
                  r.completed_at_ms AS runtime_completed_at_ms,
                  ws.completed_at_ms AS session_completed_at_ms
           FROM runtimes r
           LEFT JOIN worker_sessions ws ON ws.id = r.id
           WHERE ws.id IS NULL
              OR ws.state != r.status
              OR NOT (ws.thread_id IS r.thread_id)
              OR NOT (ws.agent_session_id IS r.session_id)
              OR NOT (ws.active_turn_id IS r.active_turn_id)
              OR NOT (ws.terminal_run_id IS r.terminal_run_id)
              OR NOT (ws.handle_state_json IS r.handle_state_json)
              OR ws.created_at_ms != r.created_at_ms
              OR ws.updated_at_ms != r.updated_at_ms
              OR NOT (ws.completed_at_ms IS r.completed_at_ms)
           ORDER BY r.created_at_ms ASC, r.id ASC"#,
    )
    .fetch_all(pool)
    .await
    .map_err(|e| format!("query runtimes/worker_sessions parity: {e}"))?;

    if rows.is_empty() {
        return Ok(());
    }

    let details = rows
        .iter()
        .map(|row| {
            format!(
                "runtime_id={} status={:?}/{:?} thread={:?}/{:?} session={:?}/{:?} turn={:?}/{:?} terminal={:?}/{:?} handle={:?}/{:?} created={:?}/{:?} updated={:?}/{:?} completed={:?}/{:?}",
                row.get::<String, _>("runtime_id"),
                row.try_get::<String, _>("runtime_status").ok(),
                row.try_get::<Option<String>, _>("session_state").ok().flatten(),
                row.try_get::<Option<String>, _>("runtime_thread_id").ok().flatten(),
                row.try_get::<Option<String>, _>("session_thread_id").ok().flatten(),
                row.try_get::<Option<String>, _>("runtime_session_id").ok().flatten(),
                row.try_get::<Option<String>, _>("session_agent_session_id").ok().flatten(),
                row.try_get::<Option<String>, _>("runtime_active_turn_id").ok().flatten(),
                row.try_get::<Option<String>, _>("session_active_turn_id").ok().flatten(),
                row.try_get::<Option<String>, _>("runtime_terminal_run_id").ok().flatten(),
                row.try_get::<Option<String>, _>("session_terminal_run_id").ok().flatten(),
                row.try_get::<Option<String>, _>("runtime_handle_state_json").ok().flatten(),
                row.try_get::<Option<String>, _>("session_handle_state_json").ok().flatten(),
                row.try_get::<i64, _>("runtime_created_at_ms").ok(),
                row.try_get::<Option<i64>, _>("session_created_at_ms").ok().flatten(),
                row.try_get::<i64, _>("runtime_updated_at_ms").ok(),
                row.try_get::<Option<i64>, _>("session_updated_at_ms").ok().flatten(),
                row.try_get::<Option<i64>, _>("runtime_completed_at_ms").ok().flatten(),
                row.try_get::<Option<i64>, _>("session_completed_at_ms").ok().flatten(),
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    Err(details)
}

impl Repo for SqlxRepo {
    fn sqlite_pool(&self) -> Option<SqlitePool> {
        Some(self.pool.clone())
    }
}

pub async fn append_decision_event_in_tx<G: DecisionGate + ?Sized>(
    tx: &mut Transaction<'_, Sqlite>,
    gate: &G,
    actor: &ActorId,
    scope: &EventScope,
    correlation: Option<&str>,
    event: &Event,
) -> Result<i64> {
    gate.decide(tx, actor, scope, event).await?.into_result()?;
    let event_id = SqlxRepo::event_append_in_tx(tx, actor, scope, correlation, event).await?;
    if let Some(wave_id) = scope.wave_id() {
        wave_vcs::commit_in_tx(
            tx,
            wave_id,
            actor,
            event_id,
            event,
            wave_vcs::MANIFEST_SCHEMA_VERSION,
        )
        .await?;
    }
    Ok(event_id)
}

pub async fn append_decision_events_in_tx<G: DecisionGate + ?Sized>(
    tx: &mut Transaction<'_, Sqlite>,
    gate: &G,
    actor: &ActorId,
    scope: &EventScope,
    correlation: Option<&str>,
    events: &[Event],
) -> Result<Vec<i64>> {
    let mut event_ids = Vec::with_capacity(events.len());
    for event in events {
        gate.decide(tx, actor, scope, event).await?.into_result()?;
        event_ids.push(SqlxRepo::event_append_in_tx(tx, actor, scope, correlation, event).await?);
    }
    if let (Some(wave_id), Some(event_id)) = (scope.wave_id(), event_ids.last()) {
        wave_vcs::commit_events_in_tx(
            tx,
            wave_id,
            actor,
            *event_id,
            events,
            wave_vcs::MANIFEST_SCHEMA_VERSION,
        )
        .await?;
    }
    Ok(event_ids)
}

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

fn is_sqlite_busy(e: &sqlx::Error) -> bool {
    let Some(db_err) = e.as_database_error() else {
        return false;
    };
    db_err.code().as_deref().is_some_and(is_sqlite_busy_code)
}

fn is_sqlite_busy_code(code: &str) -> bool {
    if let Ok(code) = code.parse::<i64>() {
        return matches!(code & 0xFF, 5 | 6);
    }
    matches!(code, "SQLITE_BUSY" | "SQLITE_LOCKED")
        || code.starts_with("SQLITE_BUSY_")
        || code.starts_with("SQLITE_LOCKED_")
}

pub async fn terminal_get_by_card_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: &str,
) -> Result<Option<Terminal>> {
    let row = sqlx::query_as::<_, Terminal>(
        r#"SELECT id, card_id, program, cwd, env, pid,
                  theme_fg, theme_bg, exit_code, signal_killed, created_at
           FROM terminals WHERE card_id = ?1"#,
    )
    .bind(card_id)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row)
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
async fn check_no_unknown_future_migrations(
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
async fn next_sort_scoped_in_tx(
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

// ---------------------------------------------------------------------------
// `_tx` helpers — composable inside `Repo::write_with_event` closures.
// ---------------------------------------------------------------------------

pub async fn cove_create_tx(tx: &mut Transaction<'_, Sqlite>, p: NewCove) -> Result<Cove> {
    let sort = match p.sort {
        Some(s) => s,
        None => next_sort_scoped_in_tx(tx, "coves", "", None).await?,
    };
    let now = now_ms();
    let id = new_id();
    // Issue #175: user-facing creates always land as `CoveKind::User`.
    // The `coves.kind` column was added in migration 0009 with DEFAULT
    // 'user'; we bind the variant explicitly here (mirroring the
    // `card_create_with_id_tx` pattern that binds `CardRole::Worker`)
    // so the storage shape stays self-documenting and a future kind
    // addition surfaces here as a compile error rather than silently
    // accepting the DB default. The system cove is minted exclusively
    // via `cove_create_system_tx` below.
    sqlx::query(
        r#"INSERT INTO coves (id, name, color, sort, kind, created_at, updated_at)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)"#,
    )
    .bind(&id)
    .bind(&p.name)
    .bind(&p.color)
    .bind(sort)
    .bind(CoveKind::User.as_db_str())
    .bind(now)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    Ok(Cove {
        id: id.into(),
        name: p.name,
        color: p.color,
        sort,
        kind: CoveKind::User,
        created_at: now,
        updated_at: now,
    })
}

/// Issue #175 — mint the singleton system cove that hosts the default
/// Today terminal's wave + card. The unique partial index on
/// `coves(kind) WHERE kind = 'system'` from migration 0009 enforces the
/// at-most-one invariant DB-side; the upsert endpoint
/// (`POST /api/coves/system`) checks for existence before calling this
/// helper, so a healthy production path never trips the index. We
/// don't translate a uniqueness violation into a typed conflict here
/// — if two callers race past the existence check we want the txn to
/// roll back and the loser to retry via the upsert endpoint, which
/// will re-read the now-existing row.
///
/// `name`, `color`, and `sort` are sentinel values the user never sees
/// (system coves are filtered out of `GET /api/coves`). They exist
/// because the underlying columns are `NOT NULL`; the chosen sentinels
/// (`name = 'system'`, `color = '#000'`, `sort = -1.0`) are documented
/// here so a debugger landing on a system row knows it's looking at
/// scaffolding, not user data.
pub async fn cove_create_system_tx(tx: &mut Transaction<'_, Sqlite>) -> Result<Cove> {
    let now = now_ms();
    let id = new_id();
    // Sort sentinel: -1.0 places the system cove below any user cove
    // (which start at 1.0 via `next_sort_scoped_in_tx`) if a debugger
    // ever asks for `coves ORDER BY sort`. Hidden from `GET /api/coves`
    // either way; this is just a debugger-friendly default.
    let sort = -1.0_f64;
    sqlx::query(
        r#"INSERT INTO coves (id, name, color, sort, kind, created_at, updated_at)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)"#,
    )
    .bind(&id)
    .bind("system")
    .bind("#000")
    .bind(sort)
    .bind(CoveKind::System.as_db_str())
    .bind(now)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    Ok(Cove {
        id: id.into(),
        name: "system".into(),
        color: "#000".into(),
        sort,
        kind: CoveKind::System,
        created_at: now,
        updated_at: now,
    })
}

pub async fn cove_update_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    p: CovePatch,
) -> Result<Cove> {
    let mut c = sqlx::query_as::<_, crate::db::rows::CoveRow>(
        r#"SELECT id, name, color, sort, kind, created_at, updated_at
           FROM coves WHERE id = ?1"#,
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?
    .map(Cove::from)
    .ok_or_else(|| CalmError::NotFound(format!("cove {id}")))?;

    if let Some(v) = p.name {
        c.name = v;
    }
    if let Some(v) = p.color {
        c.color = v;
    }
    if let Some(v) = p.sort {
        c.sort = v;
    }
    c.updated_at = now_ms();

    // `kind` is intentionally absent from `CovePatch` — issue #175
    // forbids re-tagging a cove between user/system through the regular
    // PATCH surface. The system cove is minted exactly once via
    // `cove_create_system_tx` and never demoted; user coves stay user.
    sqlx::query(
        r#"UPDATE coves SET name = ?1, color = ?2, sort = ?3, updated_at = ?4
           WHERE id = ?5"#,
    )
    .bind(&c.name)
    .bind(&c.color)
    .bind(c.sort)
    .bind(c.updated_at)
    .bind(c.id.as_str())
    .execute(&mut **tx)
    .await?;
    Ok(c)
}

pub async fn cove_delete_tx(tx: &mut Transaction<'_, Sqlite>, id: &str) -> Result<()> {
    let wave_ids: Vec<(String,)> = sqlx::query_as("SELECT id FROM waves WHERE cove_id = ?1")
        .bind(id)
        .fetch_all(&mut **tx)
        .await?;
    for (wave_id,) in wave_ids {
        sqlx::query("DELETE FROM wave_vcs_refs WHERE wave_id = ?1")
            .bind(&wave_id)
            .execute(&mut **tx)
            .await?;
        sqlx::query("DELETE FROM wave_vcs_commits WHERE wave_id = ?1")
            .bind(&wave_id)
            .execute(&mut **tx)
            .await?;
        // #644 — `tasks` has no FK to `waves`; mirror `wave_delete_tx`.
        sqlx::query("DELETE FROM tasks WHERE wave_id = ?1")
            .bind(&wave_id)
            .execute(&mut **tx)
            .await?;
        clear_wave_root_session_refs_for_worker_session_delete_tx(
            tx,
            WorkerSessionDeleteScope::Wave { wave_id: &wave_id },
        )
        .await?;
        sqlx::query("DELETE FROM worker_sessions WHERE wave_id = ?1")
            .bind(&wave_id)
            .execute(&mut **tx)
            .await?;
    }
    let res = sqlx::query("DELETE FROM coves WHERE id = ?1")
        .bind(id)
        .execute(&mut **tx)
        .await?;
    if res.rows_affected() == 0 {
        return Err(CalmError::NotFound(format!("cove {id}")));
    }
    Ok(())
}

/// Issue #250 PR 2 — in-tx variant of [`SqlxRepo::cove_folder_create`].
///
/// Needed because the wave-create path with `attach_folder = true`
/// claims a folder and writes the wave row in the **same** transaction:
/// either both land or neither does. The route layer
/// (`routes::waves::create_wave`) hands path normalization +
/// conflict-classification responsibilities here (mirror of the route
/// layer in `routes::cove_folders::create_folder`), but the conflict
/// scan reuses the existing in-memory pass over `cove_folders_list_all`
/// inside the same tx so a concurrent claim from another connection is
/// detected by the UNIQUE constraint at INSERT time. Returns the
/// inserted row; the caller emits whatever event/cache write it needs.
pub async fn cove_folder_create_tx(
    tx: &mut Transaction<'_, Sqlite>,
    cove_id: &str,
    path: &str,
) -> Result<CoveFolder> {
    let exists: Option<(String,)> = sqlx::query_as("SELECT id FROM coves WHERE id = ?1")
        .bind(cove_id)
        .fetch_optional(&mut **tx)
        .await?;
    if exists.is_none() {
        return Err(CalmError::NotFound(format!("cove {cove_id}")));
    }
    let now = now_ms();
    let res =
        sqlx::query("INSERT INTO cove_folders (cove_id, path, created_at) VALUES (?1, ?2, ?3)")
            .bind(cove_id)
            .bind(path)
            .bind(now)
            .execute(&mut **tx)
            .await;
    match res {
        Ok(out) => Ok(CoveFolder {
            id: out.last_insert_rowid(),
            cove_id: cove_id.to_string().into(),
            path: path.to_string(),
            created_at: now,
        }),
        Err(sqlx::Error::Database(dbe)) if dbe.message().contains("UNIQUE") => Err(
            CalmError::Conflict(format!("cove_folders.path already claims `{path}`")),
        ),
        Err(e) => Err(e.into()),
    }
}

/// Issue #250 PR 2 — in-tx variant of `cove_folders_list_all`. Used by
/// the wave-create `attach_folder = true` path so the conflict scan
/// reads consistent state alongside the row insert. SQLite serializes
/// writers anyway, but routing through the same tx future-proofs the
/// path against per-connection isolation surprises.
pub async fn cove_folders_list_all_tx(tx: &mut Transaction<'_, Sqlite>) -> Result<Vec<CoveFolder>> {
    let rows = sqlx::query_as::<_, crate::db::rows::CoveFolderRow>(
        r#"SELECT id, cove_id, path, created_at
           FROM cove_folders ORDER BY path ASC"#,
    )
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows.into_iter().map(CoveFolder::from).collect())
}

pub async fn wave_create_tx(
    tx: &mut Transaction<'_, Sqlite>,
    p: NewWave,
    wave_cove_cache: &WaveCoveCache,
) -> Result<Wave> {
    let exists: Option<(String,)> = sqlx::query_as("SELECT id FROM coves WHERE id = ?1")
        .bind(p.cove_id.as_str())
        .fetch_optional(&mut **tx)
        .await?;
    if exists.is_none() {
        return Err(CalmError::NotFound(format!("cove {}", p.cove_id)));
    }

    let sort = match p.sort {
        Some(s) => s,
        None => {
            next_sort_scoped_in_tx(tx, "waves", "WHERE cove_id = ?1", Some(p.cove_id.as_ref()))
                .await?
        }
    };
    let now = now_ms();
    let id = new_id();
    // Issue #145 — new waves seed at `lifecycle = 'draft'`. The DB
    // DEFAULT in migration 0012 also pins this, but stamping it
    // explicitly here matches the "required field, no Option" model:
    // every wave-create path declares the seed lifecycle in code so a
    // future change to the seed value can't be reached by skipping
    // the column from the INSERT list.
    let lifecycle = crate::model::WaveLifecycle::Draft;
    // Issue #250 PR 2 — `cwd` lands on the row verbatim from `NewWave`.
    // The route layer (`POST /api/waves`) already validated absolute-
    // path shape + cove-folder ownership; this writer stays mechanical.
    // `terminal_at` is `NULL` on every fresh wave (Draft is non-terminal
    // by construction; `WaveLifecycle::is_terminal` returns false for it).
    sqlx::query(
        r#"INSERT INTO waves
           (id, cove_id, title, sort, archived_at, pinned_at, lifecycle, cwd, terminal_at, created_at, updated_at)
           VALUES (?1, ?2, ?3, ?4, NULL, NULL, ?5, ?6, NULL, ?7, ?8)"#,
    )
    .bind(&id)
    .bind(p.cove_id.as_str())
    .bind(&p.title)
    .bind(sort)
    .bind(lifecycle.as_db_str())
    .bind(&p.cwd)
    .bind(now)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    // #234 — write-through into the wave→cove cache. Same semantics as
    // the `card_role_cache` write-through in `card_create_with_id_tx`: a
    // follow-up emit inside the same `write_with_event` closure can
    // see the freshly-minted binding via `enforce_role`'s lookup.
    let wave_id: WaveId = id.clone().into();
    wave_cove_cache.insert(wave_id.clone(), p.cove_id.clone());
    Ok(Wave {
        id: wave_id,
        cove_id: p.cove_id,
        title: p.title,
        sort,
        archived_at: None,
        pinned_at: None,
        lifecycle,
        cwd: p.cwd,
        terminal_at: None,
        created_at: now,
        updated_at: now,
    })
}

pub async fn wave_update_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    p: WavePatch,
) -> Result<Wave> {
    let mut w = sqlx::query_as::<_, crate::db::rows::WaveRow>(
        r#"SELECT id, cove_id, title, sort, archived_at, pinned_at, lifecycle, cwd, terminal_at, created_at, updated_at
           FROM waves WHERE id = ?1"#,
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?
    .map(Wave::from)
    .ok_or_else(|| CalmError::NotFound(format!("wave {id}")))?;

    if let Some(v) = p.title {
        w.title = v;
    }
    if let Some(v) = p.sort {
        w.sort = v;
    }
    if let Some(v) = p.archived_at {
        w.archived_at = v;
    }
    if let Some(v) = p.pinned_at {
        w.pinned_at = v;
    }
    // Issue #145 — `WavePatch.lifecycle` is applied here, but the
    // transition is validated by `validate_transition` at the call
    // site (REST handler / MCP tool), *outside* the DB layer. Routing
    // the validator through the route boundary (rather than this
    // function) keeps `wave_update_tx` a pure mechanical row write
    // and avoids threading `ActorId` through every call site that
    // patches the row. Production code paths that mutate
    // `lifecycle` must call `validate_transition` first.
    //
    // Issue #250 PR 2 — `terminal_at` rides on the lifecycle column:
    // when this patch advances the wave into a terminal state we
    // stamp the current time; when it reopens a terminal wave
    // (terminal → planning, the only legal reopen edge today) we
    // clear `terminal_at` back to NULL. A patch that doesn't touch
    // `lifecycle` leaves `terminal_at` alone — that matches the
    // archive precedent (changing `title` doesn't bump `archived_at`).
    // The stamp happens inside the same transaction as the wave row
    // update and the caller's `WaveLifecycleChanged` event, so a
    // mid-tx crash leaves none of them behind.
    if let Some(new_lifecycle) = p.lifecycle {
        if new_lifecycle != w.lifecycle {
            if new_lifecycle.is_terminal() {
                w.terminal_at = Some(now_ms());
            } else if w.lifecycle.is_terminal() {
                // Reopen (terminal → non-terminal). Today the only
                // legal edge here is `terminal → planning` (user-
                // driven, gated by `validate_transition`). Clearing
                // the stamp ensures a reopened wave doesn't render
                // with a stale terminal date on the calendar.
                w.terminal_at = None;
            }
        }
        w.lifecycle = new_lifecycle;
    }
    w.updated_at = now_ms();

    sqlx::query(
        r#"UPDATE waves
           SET title = ?1, sort = ?2, archived_at = ?3, pinned_at = ?4,
               lifecycle = ?5, terminal_at = ?6, updated_at = ?7
           WHERE id = ?8"#,
    )
    .bind(&w.title)
    .bind(w.sort)
    .bind(w.archived_at)
    .bind(w.pinned_at)
    .bind(w.lifecycle.as_db_str())
    .bind(w.terminal_at)
    .bind(w.updated_at)
    .bind(w.id.as_str())
    .execute(&mut **tx)
    .await?;

    // Issue #644 — scheduler budget + gate policy (migration 0041).
    // These columns deliberately do NOT live on the `Wave` struct while
    // the plan is inert (PR-A): keeping them off the struct leaves every
    // `SELECT` column list, the `WaveUpdated` wire payload, and the
    // ts-rs export untouched. Targeted single-column writes here are the
    // whole PATCH surface; the PR-B scheduler reads the columns by SQL.
    if let Some(budget) = p.task_budget {
        sqlx::query("UPDATE waves SET task_budget = ?1 WHERE id = ?2")
            .bind(budget)
            .bind(w.id.as_str())
            .execute(&mut **tx)
            .await?;
    }
    if let Some(require_gates) = p.require_task_gates {
        sqlx::query("UPDATE waves SET require_task_gates = ?1 WHERE id = ?2")
            .bind(require_gates)
            .bind(w.id.as_str())
            .execute(&mut **tx)
            .await?;
    }
    Ok(w)
}

// ---------------------------------------------------------------------------
// Tasks (issue #644 — wave-scoped task plan, migration 0041)
//
// The `_tx` helpers run inside the caller's eventized write so the row
// writes and the `plan.updated` event land (or roll back) together —
// same shape as `wave_update_tx` above. Reads are mirrored on
// `RepoRead` for the tool layer's pre-checks and `calm.plan.list`.
// ---------------------------------------------------------------------------

/// Shared SELECT column list for `tasks` rows. One spelling so the
/// `FromRow` mapping can't drift between the pool reads and the in-tx
/// reads.
const TASK_COLUMNS: &str = "id, wave_id, key, kind, goal, context_json, acceptance_criteria, \
     cwd, depends_on_json, priority, gate_json, status, status_detail, worker_card_id, \
     gate_result_json, gate_attempt, gate_pid, gate_pid_starttime, gate_pid_boot_id, \
     created_at_ms, updated_at_ms, finished_at_ms";

/// In-tx read of a wave's full plan, in scheduler order
/// (`priority DESC, created_at_ms ASC, key ASC` — design §5.2). Used by
/// `calm.plan.upsert` so dep/cycle/mutability validation sees state
/// consistent with the rows it is about to write.
pub async fn tasks_by_wave_tx(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &str,
) -> Result<Vec<Task>> {
    let sql = format!(
        "SELECT {TASK_COLUMNS} FROM tasks WHERE wave_id = ?1 \
         ORDER BY priority DESC, created_at_ms ASC, key ASC"
    );
    let rows = sqlx::query_as::<_, Task>(&sql)
        .bind(wave_id)
        .fetch_all(&mut **tx)
        .await?;
    Ok(rows)
}

/// Insert one fresh plan row (`status = 'pending'`). The caller
/// (`calm.plan.upsert`) has already validated key shape + per-wave
/// uniqueness inside the same tx; the `UNIQUE (wave_id, key)`
/// constraint backs that check, so a violation here is surfaced as a
/// conflict rather than swallowed.
pub async fn task_insert_tx(tx: &mut Transaction<'_, Sqlite>, t: &Task) -> Result<()> {
    let res = sqlx::query(
        r#"INSERT INTO tasks
               (id, wave_id, key, kind, goal, context_json, acceptance_criteria, cwd,
                depends_on_json, priority, gate_json, status, status_detail, worker_card_id,
                gate_result_json, gate_attempt, gate_pid, gate_pid_starttime, gate_pid_boot_id,
                created_at_ms, updated_at_ms, finished_at_ms)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17,
                   ?18, ?19, ?20, ?21, ?22)"#,
    )
    .bind(&t.id)
    .bind(&t.wave_id)
    .bind(&t.key)
    .bind(t.kind)
    .bind(&t.goal)
    .bind(&t.context_json)
    .bind(&t.acceptance_criteria)
    .bind(&t.cwd)
    .bind(&t.depends_on_json)
    .bind(t.priority)
    .bind(&t.gate_json)
    .bind(t.status)
    .bind(&t.status_detail)
    .bind(&t.worker_card_id)
    .bind(&t.gate_result_json)
    .bind(t.gate_attempt)
    .bind(t.gate_pid)
    .bind(t.gate_pid_starttime)
    .bind(&t.gate_pid_boot_id)
    .bind(t.created_at_ms)
    .bind(t.updated_at_ms)
    .bind(t.finished_at_ms)
    .execute(&mut **tx)
    .await;
    match res {
        Ok(_) => Ok(()),
        Err(sqlx::Error::Database(dbe)) if dbe.message().contains("UNIQUE") => Err(
            CalmError::Conflict(format!("tasks ({}, {}) already exists", t.wave_id, t.key)),
        ),
        Err(e) => Err(e.into()),
    }
}

/// Revise a still-`pending` plan row. Only the spec-revisable payload
/// columns move (design §4.1 rule 5: goal/context/acceptance/cwd/deps/
/// priority/gate); identity, status, and the gate bookkeeping columns
/// are untouched. Guarded `WHERE status = 'pending'`: a row that left
/// `pending` between the caller's in-tx read and this write surfaces as
/// `Conflict` so the whole batch rolls back instead of half-applying.
pub async fn task_update_pending_tx(tx: &mut Transaction<'_, Sqlite>, t: &Task) -> Result<()> {
    let res = sqlx::query(
        r#"UPDATE tasks
           SET kind = ?1, goal = ?2, context_json = ?3, acceptance_criteria = ?4, cwd = ?5,
               depends_on_json = ?6, priority = ?7, gate_json = ?8, updated_at_ms = ?9
           WHERE id = ?10 AND status = 'pending'"#,
    )
    .bind(t.kind)
    .bind(&t.goal)
    .bind(&t.context_json)
    .bind(&t.acceptance_criteria)
    .bind(&t.cwd)
    .bind(&t.depends_on_json)
    .bind(t.priority)
    .bind(&t.gate_json)
    .bind(t.updated_at_ms)
    .bind(&t.id)
    .execute(&mut **tx)
    .await?;
    if res.rows_affected() == 0 {
        return Err(CalmError::Conflict(format!(
            "task {} is no longer pending; concurrent state change",
            t.key
        )));
    }
    Ok(())
}

/// In-tx single-row read of one plan row. Used by `calm.plan.cancel`
/// to disambiguate a 0-row guarded flip (concurrent cancel → idempotent
/// success vs. concurrent dispatch → conflict) against state consistent
/// with the write it just attempted.
pub async fn task_get_tx(tx: &mut Transaction<'_, Sqlite>, id: &str) -> Result<Option<Task>> {
    let sql = format!("SELECT {TASK_COLUMNS} FROM tasks WHERE id = ?1");
    let row = sqlx::query_as::<_, Task>(&sql)
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?;
    Ok(row)
}

/// In-tx wave-existence guard for the plan writers. `tasks.wave_id`
/// deliberately has no FK to `waves` (design §2 — events-outlive-rows
/// convention), so without this check a delete/upsert race could insert
/// plan rows for a wave whose row was just removed. Surfaced as
/// `Conflict` so the tool layer maps it onto the 409-style vocabulary.
pub async fn require_wave_exists_tx(tx: &mut Transaction<'_, Sqlite>, wave_id: &str) -> Result<()> {
    let exists: Option<(String,)> = sqlx::query_as("SELECT id FROM waves WHERE id = ?1")
        .bind(wave_id)
        .fetch_optional(&mut **tx)
        .await?;
    if exists.is_none() {
        return Err(CalmError::Conflict(format!(
            "wave {wave_id} was deleted concurrently"
        )));
    }
    Ok(())
}

/// Guarded `pending → canceled` flip (design §3.1). Returns the number
/// of rows moved (`0` = the task was not `pending`; the caller decides
/// between idempotent success and the in-flight refusal).
pub async fn task_cancel_tx(tx: &mut Transaction<'_, Sqlite>, id: &str, now: i64) -> Result<u64> {
    let res = sqlx::query(
        r#"UPDATE tasks
           SET status = 'canceled', updated_at_ms = ?1, finished_at_ms = ?1
           WHERE id = ?2 AND status = 'pending'"#,
    )
    .bind(now)
    .bind(id)
    .execute(&mut **tx)
    .await?;
    Ok(res.rows_affected())
}

/// Issue #644 PR-B — in-tx read of one wave's lifecycle plus its raw
/// `task_budget` override. The scheduler's claim tx re-checks
/// schedulability against this (not the pre-claim snapshot) so a wave
/// moved to Blocked/Canceled/Done between the ready-set pass and the
/// claim can never have new work claimed (review F4), and the budget is
/// revalidated in the same tx so a PATCH that shrank it mid-window
/// cannot over-fill the wave (round-2 review F1). `None` = the wave row
/// is gone (concurrent delete); the inner `Option<i64>` is the nullable
/// `task_budget` column (NULL = kernel default).
pub async fn wave_lifecycle_and_budget_tx(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &str,
) -> Result<Option<(WaveLifecycle, Option<i64>)>> {
    // #679 PR1 — `WaveLifecycle` lost its `sqlx::Type` derive when it
    // moved to calm-types; decode TEXT and parse via `TryFrom<String>`.
    let row: Option<(String, Option<i64>)> =
        sqlx::query_as("SELECT lifecycle, task_budget FROM waves WHERE id = ?1")
            .bind(wave_id)
            .fetch_optional(&mut **tx)
            .await?;
    row.map(|(lifecycle, budget)| {
        WaveLifecycle::try_from(lifecycle)
            .map(|lifecycle| (lifecycle, budget))
            .map_err(|e| CalmError::Internal(format!("waves.lifecycle decode: {e}")))
    })
    .transpose()
}

/// Issue #644 PR-C — the wave-level gate policy flag
/// (`waves.require_task_gates`, §6.6), read inside `calm.plan.upsert`'s
/// tx for the rule-6 check. A gone wave row reads as `false` — the
/// caller's `require_wave_exists_tx` already errored that case loudly.
pub async fn wave_require_task_gates_tx(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &str,
) -> Result<bool> {
    let row: Option<(i64,)> = sqlx::query_as("SELECT require_task_gates FROM waves WHERE id = ?1")
        .bind(wave_id)
        .fetch_optional(&mut **tx)
        .await?;
    Ok(row.is_some_and(|(v,)| v != 0))
}

/// Issue #644 PR-B — the scheduler's single-winner claim
/// (`pending → dispatched`, design §5.4). Returns rows moved (`0` =
/// someone else won the claim; the caller skips silently). Runs inside
/// the same tx that appends `Event::TaskDispatched` and the
/// `Dispatching → Working` promotion so projections never observe a
/// claimed row without its dispatch record.
pub async fn task_claim_pending_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    now: i64,
) -> Result<u64> {
    let res = sqlx::query(
        r#"UPDATE tasks
           SET status = 'dispatched', updated_at_ms = ?1
           WHERE id = ?2 AND status = 'pending'"#,
    )
    .bind(now)
    .bind(id)
    .execute(&mut **tx)
    .await?;
    Ok(res.rows_affected())
}

/// Issue #644 PR-B — the scheduler's post-spawn running stamp (design
/// §3/§5.4). Guarded `WHERE status = 'dispatched'`: a fast worker that
/// already reported (`done`/`failed`, or `verifying` once gates land)
/// makes this a no-op so the late scheduler write can never regress the
/// row. `worker_card_id` is `COALESCE`-stamped — whichever side (this
/// stamp or the report tx) lands first wins; neither overwrites.
pub async fn task_mark_running_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    worker_card_id: Option<&str>,
    now: i64,
) -> Result<u64> {
    let res = sqlx::query(
        r#"UPDATE tasks
           SET status = 'running',
               worker_card_id = COALESCE(worker_card_id, ?1),
               updated_at_ms = ?2
           WHERE id = ?3 AND status = 'dispatched'"#,
    )
    .bind(worker_card_id)
    .bind(now)
    .bind(id)
    .execute(&mut **tx)
    .await?;
    Ok(res.rows_affected())
}

/// Round-4 review F1/F2 — durable ownership proof for the
/// unstamped-row window: is `card_id` the card the worker-spawn
/// operation for `task_id` actually created?
///
/// The worker-spawn op (`kind 'codex-worker' | 'terminal-worker'`,
/// `idempotency_key = task id`) records its created card as the
/// operation target: `prepare_tx_and_advance` stamps
/// `target_type = 'card'` / `target_id` in the SAME tx in which the
/// adapter's `prepare_tx` creates the card, and the operations table
/// has no client-reachable write path. Card payloads, by contrast,
/// stay patchable via `PATCH /api/cards/{id}` (the kind validators
/// allow extra fields), so a payload `idempotency_key` echo proves
/// nothing.
///
/// Round-5 review F2: the op must additionally be SCHEDULER-created —
/// its persisted `payload_json` actor is `ActorId::KernelDispatcher`
/// (`build_worker_payload` stamps it; serde shape
/// `{"actor":{"kind":"KernelDispatcher"}}`). A legacy
/// `calm.task.dispatch` operation carries the requesting envelope's
/// actor (the spec card, `{"kind":"AiSpec",...}`) and could otherwise
/// collide on the same idempotency key — that foreign op's worker card
/// must NOT be able to flip the plan task during the unstamped
/// `dispatched` window (the scheduler classifies the payload-hash
/// conflict as a permanent spawn failure instead).
///
/// Returns `false` when no scheduler worker op row targets the card —
/// including the crash window between the claim and the op insert,
/// where NO ownership is provable: unstamped reports are rejected
/// there, the sweep's dispatched arm resubmits the op, and the real
/// worker spawned by that resubmit can report.
pub async fn worker_op_targets_card_tx(
    tx: &mut Transaction<'_, Sqlite>,
    task_id: &str,
    card_id: &str,
) -> Result<bool> {
    let owns: bool = sqlx::query_scalar(
        r#"SELECT EXISTS(
               SELECT 1 FROM operations
               WHERE kind IN ('codex-worker', 'terminal-worker')
                 AND idempotency_key = ?1
                 AND target_type = 'card'
                 AND target_id = ?2
                 AND json_extract(payload_json, '$.actor.kind') = 'KernelDispatcher'
           )"#,
    )
    .bind(task_id)
    .bind(card_id)
    .fetch_one(&mut **tx)
    .await?;
    Ok(owns)
}

/// Who is asserting a worker-report flip (round-2 review F2).
///
/// The two-sided `worker_card_id` guard from round 1 only protects
/// rows that already carry a stamp; an UNSTAMPED `dispatched` row (the
/// report-beat-the-running-stamp window) would otherwise accept any
/// same-wave worker that echoes the task id. The ownership proof for
/// that window is the worker-spawn operation's immutable target card
/// ([`worker_op_targets_card_tx`], round-4 review F1/F2) — NOT the
/// reporting card's payload, which is mutable via
/// `PATCH /api/cards/{id}` and therefore forgeable.
#[derive(Clone, Copy, Debug)]
pub enum TaskReporter<'a> {
    /// Kernel-internal caller that owns the row by construction (the
    /// scheduler's spawn-failure reconcile). Bypasses the card guard
    /// and leaves `worker_card_id` untouched (NULL COALESCE arm).
    Kernel,
    /// A worker card's report. `owns_key` must be the result of
    /// [`worker_op_targets_card_tx`] for the REPORTING card — `true`
    /// is the unstamped-row ownership proof; stamped rows are still
    /// guarded by `worker_card_id = card_id`.
    Card { card_id: &'a str, owns_key: bool },
}

impl<'a> TaskReporter<'a> {
    /// `(card_id bind, owns_key bind)` for the shared SQL guard shape.
    fn binds(self) -> (Option<&'a str>, bool) {
        match self {
            TaskReporter::Kernel => (None, true),
            TaskReporter::Card { card_id, owns_key } => (Some(card_id), owns_key),
        }
    }
}

/// Issue #644 PR-B — worker-reported success flip
/// (`dispatched/running → done`, design §3), run **inside** the
/// `calm.task.complete` emit tx (and by the terminal-exit completion
/// paths) so there is no event-persisted-but-row-stale crash window.
///
/// `dispatched` is included because a fast worker can report before the
/// scheduler's `wait()` returns. `gate_json IS NULL` is load-bearing
/// since PR-C: a gated row goes to `verifying` (see
/// [`task_start_verifying_from_worker_tx`]), never straight to `done` —
/// the worker's self-report is a claim, not evidence (§3/§6).
///
/// `wave_id` is part of the guard so a caller can never flip another
/// wave's row even if it echoes a foreign task id.
///
/// The card guard is two-sided (review F3 + round-2 F2 + round-4 F1):
/// besides the COALESCE stamp, a [`TaskReporter::Card`] caller only
/// flips a row whose `worker_card_id` matches it, or an unstamped row
/// when the reporting card proves op-target ownership (`owns_key`,
/// [`worker_op_targets_card_tx`]). A sibling worker echoing another
/// task's idempotency key — even via a forged card payload — can
/// therefore never terminalize that row, stamped or not.
/// [`TaskReporter::Kernel`] bypasses — reserved for kernel callers
/// that own the row.
pub async fn task_complete_from_worker_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    wave_id: &str,
    reporter: TaskReporter<'_>,
    now: i64,
) -> Result<u64> {
    let (worker_card_id, owns_key) = reporter.binds();
    let res = sqlx::query(
        r#"UPDATE tasks
           SET status = 'done',
               status_detail = NULL,
               worker_card_id = COALESCE(worker_card_id, ?1),
               updated_at_ms = ?2,
               finished_at_ms = ?2
           WHERE id = ?3 AND wave_id = ?4
             AND status IN ('dispatched', 'running')
             AND gate_json IS NULL
             AND (?1 IS NULL OR worker_card_id = ?1
                  OR (worker_card_id IS NULL AND ?5))"#,
    )
    .bind(worker_card_id)
    .bind(now)
    .bind(id)
    .bind(wave_id)
    .bind(owns_key)
    .execute(&mut **tx)
    .await?;
    Ok(res.rows_affected())
}

/// Which row flip a successful worker report performed (issue #644
/// PR-C). `Done` = ungated row terminalized; `Verifying` = gated row
/// handed to the gate runner (lifecycle promotion is suppressed — the
/// gate-result tx promotes instead, §3); `None` = the guarded UPDATEs
/// matched nothing (no row / already moved on / ownership miss — the
/// caller disambiguates).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SuccessReportFlip {
    Done,
    Verifying,
    None,
}

/// Issue #644 PR-C — worker-reported success flip for GATED rows
/// (`dispatched/running → verifying`, design §3): the same write that
/// persists the worker's `task.completed` hands the row to the gate
/// runner instead of terminalizing it. Identical guards to
/// [`task_complete_from_worker_tx`] except the gate condition is
/// inverted (`gate_json IS NOT NULL`). `gate_result_json` from any
/// prior wave of the plan is untouched (rows can only re-enter
/// `verifying` via a fresh report on a non-terminal row, which the
/// status guard already excludes).
pub async fn task_start_verifying_from_worker_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    wave_id: &str,
    reporter: TaskReporter<'_>,
    now: i64,
) -> Result<u64> {
    let (worker_card_id, owns_key) = reporter.binds();
    let res = sqlx::query(
        r#"UPDATE tasks
           SET status = 'verifying',
               status_detail = NULL,
               worker_card_id = COALESCE(worker_card_id, ?1),
               updated_at_ms = ?2
           WHERE id = ?3 AND wave_id = ?4
             AND status IN ('dispatched', 'running')
             AND gate_json IS NOT NULL
             AND (?1 IS NULL OR worker_card_id = ?1
                  OR (worker_card_id IS NULL AND ?5))"#,
    )
    .bind(worker_card_id)
    .bind(now)
    .bind(id)
    .bind(wave_id)
    .bind(owns_key)
    .execute(&mut **tx)
    .await?;
    Ok(res.rows_affected())
}

/// Issue #644 PR-C — the ONE success-report flip both report paths
/// (`calm.task.complete` emit tx, terminal-exit completion) run:
/// ungated rows terminalize (`done`), gated rows enter `verifying`.
/// The two guarded UPDATEs are mutually exclusive on `gate_json`, so
/// at most one matches.
pub async fn task_report_success_from_worker_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    wave_id: &str,
    reporter: TaskReporter<'_>,
    now: i64,
) -> Result<SuccessReportFlip> {
    if task_complete_from_worker_tx(tx, id, wave_id, reporter, now).await? > 0 {
        return Ok(SuccessReportFlip::Done);
    }
    if task_start_verifying_from_worker_tx(tx, id, wave_id, reporter, now).await? > 0 {
        return Ok(SuccessReportFlip::Verifying);
    }
    Ok(SuccessReportFlip::None)
}

/// Issue #644 PR-C — the gate adapter's guarded attempt bump (design
/// §6.2 `prepare_tx`): exactly one `task-verify` operation may prepare
/// attempt `N`, and only while the row is still `verifying`. 0 rows =
/// a different attempt won or the task moved on; the caller fails the
/// op benignly.
pub async fn task_gate_attempt_bump_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    attempt: i64,
    now: i64,
) -> Result<u64> {
    let res = sqlx::query(
        r#"UPDATE tasks
           SET gate_attempt = ?1, updated_at_ms = ?2
           WHERE id = ?3 AND gate_attempt = ?4 AND status = 'verifying'"#,
    )
    .bind(attempt)
    .bind(now)
    .bind(id)
    .bind(attempt - 1)
    .execute(&mut **tx)
    .await?;
    Ok(res.rows_affected())
}

/// Issue #644 PR-C — the gate-result flip
/// (`verifying → done|failed`, design §3/§6.2): records the verdict,
/// clears the gate-process bookkeeping triple, and stamps
/// `finished_at_ms`, guarded on `status = 'verifying'` AND the attempt
/// number so a superseded attempt's late observer writes nothing.
/// Callers append `Event::TaskGateResult` + the lifecycle promotion in
/// the SAME tx only when this returns 1.
pub async fn task_apply_gate_result_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    attempt: i64,
    passed: bool,
    status_detail: Option<&str>,
    gate_result_json: &str,
    now: i64,
) -> Result<u64> {
    let status = if passed { "done" } else { "failed" };
    let res = sqlx::query(
        r#"UPDATE tasks
           SET status = ?1,
               status_detail = ?2,
               gate_result_json = ?3,
               gate_pid = NULL,
               gate_pid_starttime = NULL,
               gate_pid_boot_id = NULL,
               updated_at_ms = ?4,
               finished_at_ms = ?4
           WHERE id = ?5 AND status = 'verifying' AND gate_attempt = ?6"#,
    )
    .bind(status)
    .bind(status_detail)
    .bind(gate_result_json)
    .bind(now)
    .bind(id)
    .bind(attempt)
    .execute(&mut **tx)
    .await?;
    Ok(res.rows_affected())
}

/// Issue #644 PR-B — worker-reported / kernel-observed failure flip
/// (`dispatched/running → failed`, design §3). Same guards as the
/// success flip except the gate condition: a worker failure never runs
/// a gate (§3), so gated rows fail the same way. `status_detail`
/// distinguishes `'worker-reported'` (the worker said so, or its
/// terminal exited non-zero) from `'spawn-failed'` (the scheduler could
/// not start it).
///
/// `reporter` carries the same two-sided guard as the success flip
/// (review F3 + round-2 F2 + round-4 F1): a card only flips a
/// matching-stamp row or an unstamped row it proves op-target
/// ownership of; `Kernel` bypasses.
pub async fn task_fail_from_worker_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    wave_id: &str,
    reporter: TaskReporter<'_>,
    status_detail: &str,
    now: i64,
) -> Result<u64> {
    let (worker_card_id, owns_key) = reporter.binds();
    let res = sqlx::query(
        r#"UPDATE tasks
           SET status = 'failed',
               status_detail = ?1,
               worker_card_id = COALESCE(worker_card_id, ?2),
               updated_at_ms = ?3,
               finished_at_ms = ?3
           WHERE id = ?4 AND wave_id = ?5
             AND status IN ('dispatched', 'running')
             AND (?2 IS NULL OR worker_card_id = ?2
                  OR (worker_card_id IS NULL AND ?6))"#,
    )
    .bind(status_detail)
    .bind(worker_card_id)
    .bind(now)
    .bind(id)
    .bind(wave_id)
    .bind(owns_key)
    .execute(&mut **tx)
    .await?;
    Ok(res.rows_affected())
}

enum WorkerSessionDeleteScope<'a> {
    Wave { wave_id: &'a str },
    Card { card_id: &'a str },
}

async fn clear_wave_root_session_refs_for_worker_session_delete_tx(
    tx: &mut Transaction<'_, Sqlite>,
    scope: WorkerSessionDeleteScope<'_>,
) -> Result<()> {
    match scope {
        WorkerSessionDeleteScope::Wave { wave_id } => {
            sqlx::query(
                r#"UPDATE waves
                      SET root_session_id = NULL
                    WHERE root_session_id IN (
                        SELECT id FROM worker_sessions WHERE wave_id = ?1
                    )"#,
            )
            .bind(wave_id)
            .execute(&mut **tx)
            .await?;
        }
        WorkerSessionDeleteScope::Card { card_id } => {
            sqlx::query(
                r#"UPDATE waves
                      SET root_session_id = NULL
                    WHERE root_session_id IN (
                        SELECT id FROM runtimes WHERE card_id = ?1
                    )"#,
            )
            .bind(card_id)
            .execute(&mut **tx)
            .await?;
        }
    }
    Ok(())
}

pub async fn wave_delete_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    wave_cove_cache: &WaveCoveCache,
) -> Result<()> {
    sqlx::query("DELETE FROM wave_vcs_refs WHERE wave_id = ?1")
        .bind(id)
        .execute(&mut **tx)
        .await?;
    sqlx::query("DELETE FROM wave_vcs_commits WHERE wave_id = ?1")
        .bind(id)
        .execute(&mut **tx)
        .await?;
    // #644 — `tasks.wave_id` has no FK to `waves` (events-outlive-rows
    // convention, design §2), so plan rows must be deleted explicitly
    // alongside the other no-FK wave-owned tables above.
    sqlx::query("DELETE FROM tasks WHERE wave_id = ?1")
        .bind(id)
        .execute(&mut **tx)
        .await?;
    clear_wave_root_session_refs_for_worker_session_delete_tx(
        tx,
        WorkerSessionDeleteScope::Wave { wave_id: id },
    )
    .await?;
    // `worker_sessions.wave_id` is a required FK. Card/runtime rows may
    // cascade below, but sessions must leave before the wave row itself.
    sqlx::query("DELETE FROM worker_sessions WHERE wave_id = ?1")
        .bind(id)
        .execute(&mut **tx)
        .await?;
    let res = sqlx::query("DELETE FROM waves WHERE id = ?1")
        .bind(id)
        .execute(&mut **tx)
        .await?;
    if res.rows_affected() == 0 {
        return Err(CalmError::NotFound(format!("wave {id}")));
    }
    // #234 — keep the wave→cove cache in lockstep with the table. Mirror
    // of the card-delete-side write-through in `card_delete_tx`.
    wave_cove_cache.remove(&WaveId::from(id));
    Ok(())
}

/// Card-row insert that lets the caller pre-mint the row id.
///
/// Carved out from `card_create_tx` so atomic-card endpoints (terminal,
/// codex) can stamp the soon-to-exist card id into per-card sidecar paths
/// (e.g. `codex_homes_dir.join(card_id)`) *before* the row hits the DB,
/// without re-fetching the row after insert. The standalone
/// [`card_create_tx`] wrapper preserves the original "mint inside the
/// helper" contract for every other caller.
pub async fn card_create_with_id_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: String,
    p: NewCard,
    role: CardRole,
    // Issue #229 PR A — explicit, required: every call site must decide
    // whether the card is user-deletable. Per `[[required-over-option]]`
    // an `Option<bool>` with a serde default would silently hide the
    // wrong default at any future callsite (kernel-owned cards minted
    // as deletable would be a security regression). The three live
    // callers cover the policy:
    //   * `card_create_tx`              → `true`  (user-facing Worker cards)
    //   * dispatcher worker terminals    → `true`  (workers are user-facing)
    //   * `card_with_codex_create_tx`    → caller decides (`false` for spec)
    deletable: bool,
    card_role_cache: &CardRoleCache,
) -> Result<Card> {
    let exists: Option<(String,)> = sqlx::query_as("SELECT id FROM waves WHERE id = ?1")
        .bind(p.wave_id.as_str())
        .fetch_optional(&mut **tx)
        .await?;
    if exists.is_none() {
        return Err(CalmError::NotFound(format!("wave {}", p.wave_id)));
    }

    let sort = match p.sort {
        Some(s) => s,
        None => {
            next_sort_scoped_in_tx(tx, "cards", "WHERE wave_id = ?1", Some(p.wave_id.as_ref()))
                .await?
        }
    };
    let now = now_ms();
    let payload_text = serde_json::to_string(&p.payload)?;
    // `role` lands in the `cards.role` column added by migration 0008
    // (PR3, #136). User-facing card creation now uniformly passes
    // `CardRole::Worker`; wave-create passes `CardRole::Spec`.
    //
    // `deletable` lands in the column added by migration 0013 (#229 PR A).
    // SQLite has no native bool; we encode as `1` / `0`, matching the
    // column's `INTEGER NOT NULL DEFAULT 1` shape. sqlx maps `bool ↔ i64`
    // transparently via its `Encode<Sqlite>` impl, so the bind is direct.
    sqlx::query(
        r#"INSERT INTO cards
               (id, wave_id, kind, sort, payload, role, deletable, created_at, updated_at)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"#,
    )
    .bind(&id)
    .bind(p.wave_id.as_str())
    .bind(&p.kind)
    .bind(sort)
    .bind(&payload_text)
    .bind(role.as_db_str())
    .bind(deletable)
    .bind(now)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    // PR3 (#136) — write-through into the role cache. The cache update
    // happens *inside* the surrounding `write_with_event` transaction
    // so a follow-up emit in the same closure can see the freshly
    // minted role via `enforce_role`'s lookup. A txn rollback leaves a
    // stale entry; that's acceptable per the cache's documented
    // semantics — `enforce_role` denies in the only direction that
    // matters (unknown card) and the next boot's `seed_from_db` will
    // overwrite stale entries from the persisted truth.
    let card_id: CardId = id.into();
    card_role_cache.insert(card_id.clone(), role, p.wave_id.clone());
    Ok(Card {
        id: card_id,
        wave_id: p.wave_id,
        kind: p.kind,
        sort,
        payload: p.payload,
        runtime: None,
        deletable,
        created_at: now,
        updated_at: now,
    })
}

pub async fn card_create_tx(
    tx: &mut Transaction<'_, Sqlite>,
    p: NewCard,
    card_role_cache: &CardRoleCache,
) -> Result<Card> {
    // User-facing Worker cards are user-deletable by default — the user
    // added them via REST and can remove them the same way. Spec / report
    // cards take the explicit `false` route via
    // `card_with_codex_create_tx`.
    card_create_with_id_tx(tx, new_id(), p, CardRole::Worker, true, card_role_cache).await
}

pub async fn card_update_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    p: CardPatch,
) -> Result<Card> {
    let mut c = sqlx::query_as::<_, crate::db::rows::CardRow>(
        r#"SELECT id, wave_id, kind, sort, payload, deletable, created_at, updated_at
           FROM cards WHERE id = ?1"#,
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?
    .map(Card::from)
    .ok_or_else(|| CalmError::NotFound(format!("card {id}")))?;

    if let Some(v) = p.kind {
        c.kind = v;
    }
    if let Some(v) = p.sort {
        c.sort = v;
    }
    if let Some(v) = p.payload {
        c.payload = v;
    }
    // Issue #229 PR A — `p.deletable` is intentionally ignored here.
    // The route handler in `routes/cards.rs::update_card` returns 400
    // when a client sends the field; the field exists on `CardPatch`
    // only to make that 400 explicit (rather than serde silently
    // dropping an unknown field). The UPDATE statement below also
    // doesn't touch the `deletable` column — defense in depth.
    c.updated_at = now_ms();
    let payload_text = serde_json::to_string(&c.payload)?;

    sqlx::query(
        r#"UPDATE cards SET kind = ?1, sort = ?2, payload = ?3, updated_at = ?4
           WHERE id = ?5"#,
    )
    .bind(&c.kind)
    .bind(c.sort)
    .bind(&payload_text)
    .bind(c.updated_at)
    .bind(c.id.as_str())
    .execute(&mut **tx)
    .await?;
    Ok(c)
}

/// Issue #247 PR1 — wave-report-specific transactional update that
/// rewrites both the legacy `payload` JSON column AND the new opaque
/// CRDT blob in `body_crdt` in one statement. Wraps [`card_update_tx`]
/// for the JSON+timestamps path, then re-runs a single UPDATE to
/// stamp the blob. Both writes happen inside the supplied `tx` so a
/// rollback drops them together — the JSON cache and the CRDT
/// authoritative bytes never drift.
///
/// `body_crdt` is the `automerge::AutoCommit::save()` bytes from
/// `wave_report_doc::ReportDoc::to_bytes`; the kernel never
/// interprets the column outside of the round-trip via that module.
///
/// This is a **wave-report-only** seam. Terminal / codex /
/// plugin cards continue going through `card_update_tx`, which never
/// touches `body_crdt` — the column stays NULL on those rows forever.
pub async fn card_update_with_crdt_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    p: CardPatch,
    body_crdt: Vec<u8>,
) -> Result<Card> {
    // Reuse the existing JSON+timestamps update path so the two
    // codepaths can't drift on what `updated_at` / payload-text
    // semantics look like.
    let card = card_update_tx(tx, id, p).await?;
    // Second statement: stamp the opaque CRDT bytes onto the row.
    // Split into its own UPDATE (rather than extending the one above)
    // so plain `card_update_tx` callers never sqlx-bind a `Vec<u8>`
    // they don't care about. The combined cost is one extra UPDATE
    // per wave-report write, which is dominated by the surrounding
    // event-emit work.
    sqlx::query(r#"UPDATE cards SET body_crdt = ?1 WHERE id = ?2"#)
        .bind(&body_crdt)
        .bind(card.id.as_str())
        .execute(&mut **tx)
        .await?;
    Ok(card)
}

/// Issue #247 PR1 — read the opaque CRDT blob for a card inside an
/// open transaction. Returns `None` in either of two cases:
///
///   * the card row doesn't exist (fetched via `fetch_optional` —
///     no `NotFound` is raised, the absent row collapses into the
///     same "no blob to load" signal as a NULL column), or
///   * the row exists but `body_crdt` IS NULL (every pre-PR1 row,
///     plus non-wave-report cards which never get initialized).
///
/// Returns `Some(bytes)` for any row whose first post-PR1 write has
/// run through `card_update_with_crdt_tx`.
///
/// Read inside the same tx as the update so a concurrent writer
/// can't slip a blob in between this read and our `to_bytes` write
/// (the wave-report write path is the only writer of the column
/// today, but pinning the read to the tx is cheap and matches the
/// pattern the rest of `*_tx` uses).
pub async fn card_body_crdt_get_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
) -> Result<Option<Vec<u8>>> {
    let row: Option<(Option<Vec<u8>>,)> =
        sqlx::query_as(r#"SELECT body_crdt FROM cards WHERE id = ?1"#)
            .bind(id)
            .fetch_optional(&mut **tx)
            .await?;
    Ok(row.and_then(|(blob,)| blob))
}

pub async fn card_delete_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    card_role_cache: &CardRoleCache,
) -> Result<()> {
    clear_wave_root_session_refs_for_worker_session_delete_tx(
        tx,
        WorkerSessionDeleteScope::Card { card_id: id },
    )
    .await?;
    sqlx::query(
        "DELETE FROM worker_sessions WHERE id IN (SELECT id FROM runtimes WHERE card_id = ?1)",
    )
    .bind(id)
    .execute(&mut **tx)
    .await?;

    let res = sqlx::query("DELETE FROM cards WHERE id = ?1")
        .bind(id)
        .execute(&mut **tx)
        .await?;
    if res.rows_affected() == 0 {
        return Err(CalmError::NotFound(format!("card {id}")));
    }
    // Not reached when a wave/cove delete cascades cards via FK — those
    // paths sweep card overlays in their own txn via
    // overlay_delete_card_overlays_by_wave_tx / overlay_delete_subtree_by_cove_tx.
    overlay_delete_by_entity_tx(tx, "card", id).await?;
    // PR3 (#136) — keep the role cache in lockstep with the table.
    // Like the insert-side write-through, this happens before commit;
    // a txn rollback would leave the cache temporarily missing an
    // entry. The consequence is at worst an `enforce_role` deny on a
    // re-emit that would have been allowed (the card still exists),
    // which is the *safe* failure mode for an auth gate.
    card_role_cache.remove(&CardId::from(id));
    Ok(())
}

pub async fn terminal_delete_tx(tx: &mut Transaction<'_, Sqlite>, id: &str) -> Result<()> {
    let res = sqlx::query("DELETE FROM terminals WHERE id = ?1")
        .bind(id)
        .execute(&mut **tx)
        .await?;
    if res.rows_affected() == 0 {
        return Err(CalmError::NotFound(format!("terminal {id}")));
    }
    Ok(())
}

/// Transactional terminal-row insert. Structural twin of the `terminal_create`
/// method on `SqlxRepo` — same parent-card-exists and per-card uniqueness
/// pre-checks, same `NotFound` / `Conflict` mapping — but composable inside
/// `Repo::write_with_event` closures alongside the card write.
///
/// Currently only invoked from `card_with_terminal_create_tx`; the standalone
/// `RepoOutOfDomain::terminal_create` path still talks to the pool directly so
/// the existing `POST /api/cards/:id/terminal` recipe keeps its behavior
/// untouched until #13 PR2 swaps it out.
pub async fn terminal_create_tx(
    tx: &mut Transaction<'_, Sqlite>,
    p: NewTerminal,
) -> Result<Terminal> {
    // Parent card must exist; surface as NotFound to mirror MockRepo.
    let exists: Option<(String,)> = sqlx::query_as("SELECT id FROM cards WHERE id = ?1")
        .bind(p.card_id.as_str())
        .fetch_optional(&mut **tx)
        .await?;
    if exists.is_none() {
        return Err(CalmError::NotFound(format!("card {}", p.card_id)));
    }
    // Per-card uniqueness — surface as Conflict to mirror MockRepo
    // (the schema also enforces this via UNIQUE on terminals.card_id).
    let dup: Option<(String,)> = sqlx::query_as("SELECT id FROM terminals WHERE card_id = ?1")
        .bind(p.card_id.as_str())
        .fetch_optional(&mut **tx)
        .await?;
    if dup.is_some() {
        return Err(CalmError::Conflict(format!(
            "terminal already exists for card {}",
            p.card_id
        )));
    }

    let now = now_ms();
    let id = new_id();
    let env_text = serde_json::to_string(&p.env)?;
    // #177 — theme is a write-once row invariant. Render the
    // `(r, g, b)` tuples to comma-decimal once at row creation so
    // every spawn path that reads this row can use the theme with zero
    // allocation.
    let theme_fg = p.theme.fg_arg();
    let theme_bg = p.theme.bg_arg();
    sqlx::query(
        r#"INSERT INTO terminals
               (id, card_id, program, cwd, env, pid, theme_fg, theme_bg, created_at)
           VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6, ?7, ?8)"#,
    )
    .bind(&id)
    .bind(p.card_id.as_str())
    .bind(&p.program)
    .bind(&p.cwd)
    .bind(&env_text)
    .bind(&theme_fg)
    .bind(&theme_bg)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    Ok(Terminal {
        id,
        card_id: p.card_id,
        program: p.program,
        cwd: p.cwd,
        env: p.env,
        pid: None,
        theme_fg,
        theme_bg,
        exit_code: None,
        signal_killed: false,
        created_at: now,
    })
}

/// Atomically create a `terminal`-kind card AND its associated terminal row
/// inside a single transaction. Runtime identity is written to `runtimes`;
/// API/WS responses project the legacy payload fields at read time.
///
/// This is the kernel side of #13's plan to collapse today's 3-step
/// terminal-card recipe (card-add → terminal-create → card-update) into one
/// atomic db helper. PR1 just lands this helper; PR2 will wire it to a new
/// `POST /api/waves/:id/terminal-cards` endpoint and delete the old recipe.
///
/// On any failure the surrounding transaction rolls back, so partial state
/// (card without terminal, or terminal without runtime row) is impossible.
#[allow(clippy::too_many_arguments)]
pub async fn card_with_terminal_create_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: String,
    runtime_id: &str,
    spawn_op_id: Option<&str>,
    wave_id: WaveId,
    sort: Option<f64>,
    program: String,
    cwd: String,
    env: serde_json::Value,
    role: CardRole,
    // Issue #229 PR A — required deletable bit, threaded through to
    // `card_create_with_id_tx`. Dispatcher's worker-terminal path passes
    // `true` (workers are user-facing — users can close them); the
    // direct `POST /api/waves/:id/terminal-cards` path passes `true` for
    // the same reason. Future kernel-owned terminal cards (none today)
    // would pass `false`.
    deletable: bool,
    card_role_cache: &CardRoleCache,
    // #177 — host browser's theme RGB, written onto the terminal row
    // alongside the card so every spawn path reads it from the row and
    // stamps consistent `--terminal-fg/-bg` argv (closes the WS auto-
    // revive race observed in PR #193).
    theme: RequestTheme,
) -> Result<(Card, Terminal)> {
    // 1. Card row with placeholder payload — schemaVersion is stamped in
    //    step 5 once we have the terminal row.
    //
    // PR2 of #136: card id is now pre-minted by the caller (same pattern
    // the codex helper has had since #117) so the surrounding
    // `write_with_event` can stamp `EventScope::Card { card, .. }` on
    // the audit row without racing the txn.
    //
    // User-facing terminal creation and dispatcher worker-terminal paths
    // pass `CardRole::Worker`. The cache
    // write-through inside `card_create_with_id_tx` keeps the role
    // visible to `enforce_role` calls later in the same tx.
    let card = card_create_with_id_tx(
        tx,
        card_id,
        NewCard {
            wave_id,
            kind: "terminal".into(),
            sort,
            payload: serde_json::Value::Null,
        },
        role,
        deletable,
        card_role_cache,
    )
    .await?;

    // 2. Terminal row, parented to the card.
    let term = terminal_create_tx(
        tx,
        NewTerminal {
            card_id: card.id.clone(),
            program,
            cwd,
            env,
            theme,
        },
    )
    .await?;

    // 3. Build the canonical terminal-card payload.
    let payload = serde_json::json!({
        "schemaVersion": TERMINAL_PAYLOAD_SCHEMA_VERSION,
    });

    // 4. Defense-in-depth: payload validation. The boundary call in
    //    `routes/cards.rs:141` already enforces this for direct create, but
    //    composing inside the kernel means we run our own check rather than
    //    trusting a payload we built ourselves.
    validate_card_kind_global("terminal", &payload)?;

    // 5. Re-stamp the card with the real payload.
    let card = card_update_tx(
        tx,
        card.id.as_ref(),
        CardPatch {
            kind: None,
            sort: None,
            payload: Some(payload),
            // #229 PR A — kernel-internal callers never patch
            // `deletable`; the route handler 400s clients that try.
            deletable: None,
        },
    )
    .await?;

    let runtime_init = RuntimeInit {
        id: runtime_id.to_string(),
        card_id: card.id.to_string(),
        kind: RuntimeKind::Terminal,
        agent_provider: None,
        status: RunStatus::Starting,
        terminal_run_id: Some(term.id.clone()),
        thread_id: None,
        session_id: None,
        active_turn_id: None,
        handle_state_json: None,
        lease_owner: None,
        lease_until_ms: None,
        spawn_op_id: spawn_op_id.map(str::to_string),
        now_ms: now_ms(),
    };
    if let Some(existing) = runtime_get_active_for_card_tx(tx, card.id.as_ref()).await? {
        runtime_supersede_tx(tx, &existing.id, runtime_init).await?;
    } else {
        runtime_start_tx(tx, runtime_init).await?;
    }

    Ok((card, term))
}

/// Issue #310 followup — atomically delete a card + its backing terminal
/// row inside a single tx, in the order the `RESTRICT` FK demands
/// (terminal first, then card). The structural inverse of
/// [`card_with_terminal_create_tx`] / [`card_with_codex_create_tx`].
///
/// **Use site** is the dispatcher's post-commit failure cleanup: when
/// `per-card CODEX_HOME seeding` or `spawn_daemon_with_parts` returns
/// Err *after* the row-creation tx has already committed, the worker
/// card + terminal row are orphans — the runtime references a terminal
/// whose daemon never came up, and a retry with the same
/// `idempotency_key` would short-circuit on the abandoned row instead
/// of trying again. Rolling both rows back here lets the retry succeed.
///
/// **Idempotent shape.** Each delete swallows `NotFound` so a caller
/// that races the orphan sweeper (which deletes terminals out from
/// under us on a 30-60s cadence) still completes cleanly. The card
/// delete may still surface `NotFound` if the sweeper additionally
/// reaped the card — same shape as the route handler in
/// `routes/cards.rs::delete_card`, where the comment notes the same
/// race is acceptable.
///
/// `card_role_cache` is threaded through so the cache stays in
/// lockstep with the row delete — same write-through invariant
/// `card_delete_tx` itself enforces.
pub async fn card_with_terminal_rollback_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: &str,
    terminal_id: &str,
    card_role_cache: &CardRoleCache,
) -> Result<()> {
    // Order matters — the FK on `terminals.card_id` is `ON DELETE RESTRICT`
    // since migration 0011, so the card delete would fail with a FK
    // violation if the terminal row still existed.
    match terminal_delete_tx(tx, terminal_id).await {
        Ok(()) => {}
        Err(e) if e.is_not_found() => {}
        Err(e) => return Err(e),
    }
    match card_delete_tx(tx, card_id, card_role_cache).await {
        Ok(()) => {}
        Err(e) if e.is_not_found() => {}
        Err(e) => return Err(e),
    }
    Ok(())
}

/// Atomically create a `codex`-kind card, its associated terminal row, and
/// the initial `Starting` runtime row inside a single transaction. Runtime
/// identity is written to `runtimes`; API/WS responses project the legacy
/// payload fields at read time.
///
/// Twin of [`card_with_terminal_create_tx`] for the codex-card flow (#117).
/// Differs in two places from the terminal helper:
///
///   1. The caller pre-mints `card_id` (option C in the design doc) so the
///      handler can derive per-card filesystem paths (`CODEX_HOME =
///      <codex_homes_dir>/<card_id>/`) before the row hits the DB. The
///      pre-mint avoids a post-commit "stamp env" round-trip that option B
///      would have required, and keeps a single `card.added` envelope on
///      the bus.
///   2. The canonical payload carries `cwd` when non-empty — the frontend's
///      `codex.tsx` placeholder reads it for status text while the daemon
///      boots. Terminal cards have no such field.
///
/// `program` is hardwired to `"codex"`. The caller still owns env
/// composition (CODEX_HOME / NEIGE_CARD_ID / proxy vars) since those
/// require `AppState` and a settings snapshot that the db layer shouldn't
/// see.
///
/// On any failure the surrounding transaction rolls back; a partial state
/// (card without terminal, or terminal without runtime row) is impossible.
/// PR7a (#136) — third return slot is `Some(raw_token)` for Spec/Worker
/// cards. The caller is expected to thread the raw value into the codex
/// daemon's `NEIGE_MCP_TOKEN` env var immediately and discard it — the
/// hash is persisted in `card_mcp_tokens`, but the raw form is
/// unrecoverable on a kernel restart (by design).
#[allow(clippy::too_many_arguments)]
pub async fn card_with_codex_create_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: String,
    runtime_id: &str,
    spawn_op_id: Option<&str>,
    wave_id: WaveId,
    sort: Option<f64>,
    cwd: String,
    env: serde_json::Value,
    prompt: Option<String>,
    icon_bg: Option<String>,
    icon_fg: Option<String>,
    role: CardRole,
    // Issue #229 PR A — required deletable bit. The wave-create route
    // passes `false` (the spec card is kernel-owned, must survive
    // direct REST / plugin-callback delete attempts). The user-facing
    // `POST /api/waves/:id/codex-cards` route passes `true`.
    deletable: bool,
    card_role_cache: &CardRoleCache,
    // #177 — host browser's theme RGB; written onto the terminal row
    // in the same transaction so the codex daemon's spawn argv is
    // deterministic regardless of which spawn path lands it.
    theme: RequestTheme,
) -> Result<(Card, Terminal, Option<String>)> {
    // 1. Card row with placeholder payload — schemaVersion and UI hints
    //    are stamped in step 5 once we have the terminal row.
    //
    // User-facing codex creation and dispatcher paths pass
    // `CardRole::Worker`. The wave-create route passes `CardRole::Spec`
    // so the auto-minted spec card is recognized by `enforce_role` as a
    // `WaveUpdated`-permitted emitter. The cache write-through
    // inside `card_create_with_id_tx` keeps the role visible to
    // `enforce_role` calls later in the same tx.
    let card = card_create_with_id_tx(
        tx,
        card_id,
        NewCard {
            wave_id,
            kind: "codex".into(),
            sort,
            payload: serde_json::Value::Null,
        },
        role,
        deletable,
        card_role_cache,
    )
    .await?;

    // 2. Terminal row, parented to the card. `program == "codex"` always —
    //    the codex CLI runs in the PTY directly (see `routes::codex_cards`).
    let term = terminal_create_tx(
        tx,
        NewTerminal {
            card_id: card.id.clone(),
            program: "codex".into(),
            cwd: cwd.clone(),
            env,
            theme,
        },
    )
    .await?;

    // 3. Build the canonical codex-card payload. `cwd` is omitted when the
    //    caller passed an empty string — the frontend treats a missing
    //    `cwd` as "show no path hint" rather than "show an empty path".
    let mut payload = serde_json::Map::new();
    payload.insert(
        "schemaVersion".into(),
        serde_json::Value::from(CODEX_PAYLOAD_SCHEMA_VERSION),
    );
    if !cwd.is_empty() {
        payload.insert("cwd".into(), serde_json::Value::String(cwd));
    }
    // `prompt` — surfaces to the `legacy auto-submit` subscriber, which
    // gates auto-Enter on this being a non-empty string. An empty /
    // missing value here is the "user spawned codex without a hands-free
    // prompt" path, identical to pre-#110 behaviour. Trimmed and empty-
    // filtered so the subscriber's `.filter(|s| !s.is_empty())` is the
    // single source of truth.
    if let Some(p) = prompt.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        payload.insert("prompt".into(), serde_json::Value::String(p.to_string()));
    }
    if let Some(c) = icon_bg.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        payload.insert("icon_bg".into(), serde_json::Value::String(c.to_string()));
    }
    if let Some(c) = icon_fg.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        payload.insert("icon_fg".into(), serde_json::Value::String(c.to_string()));
    }
    let payload = serde_json::Value::Object(payload);

    // 4. Defense-in-depth: payload validation. The boundary call in
    //    `routes/cards.rs` enforces this for direct create; composing
    //    inside the kernel means we re-run the check on the payload we
    //    just built.
    validate_card_kind_global("codex", &payload)?;

    // 5. Re-stamp the card with the real payload.
    let card = card_update_tx(
        tx,
        card.id.as_ref(),
        CardPatch {
            kind: None,
            sort: None,
            payload: Some(payload),
            // #229 PR A — kernel-internal callers never patch
            // `deletable`; the route handler 400s clients that try.
            deletable: None,
        },
    )
    .await?;

    // 6. PR7a (#136) — when the card is Spec/Worker, mint a fresh per-card
    //    MCP token, store the hash in `card_mcp_tokens` inside the same tx
    //    (FK enforced — the card row above is the parent), and return the
    //    raw value to the caller so it can be threaded into the codex
    //    daemon's `NEIGE_MCP_TOKEN` env var.
    //
    //    Doing this here (rather than at the route layer) keeps the
    //    invariant atomic: a committed card row whose role is Spec/Worker
    //    will *always* have a matching token row, and a rolled-back tx
    //    drops both together.
    let mut mcp_token_hash = None;
    let mcp_token = if matches!(role, CardRole::Spec | CardRole::Worker) {
        let token = crate::mcp_auth::CardMcpToken::generate();
        let hashed = crate::mcp_auth::hash_token(token.as_str());
        card_mcp_token_set_tx(tx, card.id.as_ref(), &hashed).await?;
        mcp_token_hash = Some(hashed);
        Some(token.into_inner())
    } else {
        None
    };

    let runtime_init = RuntimeInit {
        id: runtime_id.to_string(),
        card_id: card.id.to_string(),
        kind: RuntimeKind::CodexCard,
        agent_provider: Some(AgentProvider::Codex),
        status: RunStatus::Starting,
        terminal_run_id: Some(term.id.clone()),
        thread_id: None,
        session_id: None,
        active_turn_id: None,
        handle_state_json: None,
        lease_owner: None,
        lease_until_ms: None,
        spawn_op_id: spawn_op_id.map(str::to_string),
        now_ms: now_ms(),
    };
    runtime_start_tx(tx, runtime_init).await?;
    if let Some(hashed) = mcp_token_hash.as_deref() {
        session_mcp_token_set_tx(tx, runtime_id, hashed).await?;
    }

    Ok((card, term, mcp_token))
}

/// Atomically create a `claude`-kind worker card AND its associated terminal
/// row. Claude cards are PTY-backed like codex cards, but intentionally have
/// no MCP token/config path; completion observability comes solely from
/// Claude hook events ingested through `/internal/claude/hook`.
#[allow(clippy::too_many_arguments)]
pub async fn card_with_claude_create_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: String,
    runtime_id: &str,
    wave_id: WaveId,
    sort: Option<f64>,
    program: String,
    cwd: String,
    env: serde_json::Value,
    prompt: Option<String>,
    icon_bg: Option<String>,
    icon_fg: Option<String>,
    settings_path: String,
    claude_session_id: String,
    role: CardRole,
    deletable: bool,
    card_role_cache: &CardRoleCache,
    theme: RequestTheme,
) -> Result<(Card, Terminal)> {
    let card = card_create_with_id_tx(
        tx,
        card_id,
        NewCard {
            wave_id,
            kind: "claude".into(),
            sort,
            payload: serde_json::Value::Null,
        },
        role,
        deletable,
        card_role_cache,
    )
    .await?;

    let term = terminal_create_tx(
        tx,
        NewTerminal {
            card_id: card.id.clone(),
            program,
            cwd: cwd.clone(),
            env,
            theme,
        },
    )
    .await?;

    let mut payload = serde_json::Map::new();
    payload.insert(
        "schemaVersion".into(),
        serde_json::Value::from(CLAUDE_PAYLOAD_SCHEMA_VERSION),
    );
    payload.insert(
        "settings_path".into(),
        serde_json::Value::String(settings_path),
    );
    if !cwd.is_empty() {
        payload.insert("cwd".into(), serde_json::Value::String(cwd));
    }
    if let Some(p) = prompt.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        payload.insert("prompt".into(), serde_json::Value::String(p.to_string()));
    }
    if let Some(c) = icon_bg.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        payload.insert("icon_bg".into(), serde_json::Value::String(c.to_string()));
    }
    if let Some(c) = icon_fg.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        payload.insert("icon_fg".into(), serde_json::Value::String(c.to_string()));
    }
    let payload = serde_json::Value::Object(payload);
    validate_card_kind_global("claude", &payload)?;

    let card = card_update_tx(
        tx,
        card.id.as_ref(),
        CardPatch {
            kind: None,
            sort: None,
            payload: Some(payload),
            deletable: None,
        },
    )
    .await?;

    let runtime_init = RuntimeInit {
        id: runtime_id.to_string(),
        card_id: card.id.to_string(),
        kind: RuntimeKind::ClaudeCard,
        agent_provider: Some(AgentProvider::Claude),
        status: RunStatus::Starting,
        terminal_run_id: Some(term.id.clone()),
        thread_id: None,
        session_id: Some(claude_session_id),
        active_turn_id: None,
        handle_state_json: None,
        lease_owner: None,
        lease_until_ms: None,
        spawn_op_id: None,
        now_ms: now_ms(),
    };
    if let Some(existing) = runtime_get_active_for_card_tx(tx, card.id.as_ref()).await? {
        runtime_supersede_tx(tx, &existing.id, runtime_init).await?;
    } else {
        runtime_start_tx(tx, runtime_init).await?;
    }

    Ok((card, term))
}

/// PR7a (#136) — insert (or replace) a per-card MCP token row in the
/// supplied transaction. The raw token is never persisted; the caller
/// passes `hash_token(raw)` and keeps the raw value only in memory long
/// enough to thread it into the env map handed to the codex daemon.
///
/// `card_id` must reference a real row in `cards` — the FK constraint
/// in migration 0010 fails the tx otherwise. The standard call site is
/// `card_with_codex_create_tx`, where the card row is created moments
/// earlier in the same tx, so the FK is satisfied by construction.
pub async fn card_mcp_token_set_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: &str,
    hashed_token: &str,
) -> Result<()> {
    let now = now_ms();
    sqlx::query(
        r#"INSERT INTO card_mcp_tokens (card_id, hashed_token, created_at)
           VALUES (?1, ?2, ?3)
           ON CONFLICT(card_id) DO UPDATE SET
               hashed_token = excluded.hashed_token,
               created_at   = excluded.created_at"#,
    )
    .bind(card_id)
    .bind(hashed_token)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// PR6b (#679) — mirror the per-card MCP hash onto the same-id worker_sessions
/// row. POPULATE-ONLY: never read for authz (the handshake reads
/// card_mcp_tokens). Fail-closed: the same-id mirror row MUST exist
/// (created by runtime_start_tx -> session_start_mirror_tx in the same spawn);
/// a missing row means the dual-write ordering drifted, so fail the spawn
/// rather than silently half-mint.
pub async fn session_mcp_token_set_tx(
    tx: &mut Transaction<'_, Sqlite>,
    session_id: &str,
    hashed_token: &str,
) -> Result<()> {
    let res = sqlx::query("UPDATE worker_sessions SET mcp_token_hash = ?1 WHERE id = ?2")
        .bind(hashed_token)
        .bind(session_id)
        .execute(&mut **tx)
        .await?;
    if res.rows_affected() != 1 {
        return Err(CalmError::Internal(format!(
            "expected 1 worker_sessions mirror row for MCP token session {session_id}, got {}",
            res.rows_affected()
        )));
    }
    Ok(())
}

pub async fn session_mark_wave_root_tx(
    tx: &mut SessionTx<'_>,
    wave_id: &WaveId,
    session_id: &WorkerSessionId,
) -> Result<()> {
    let res = sqlx::query("UPDATE waves SET root_session_id = ?1 WHERE id = ?2")
        .bind(session_id.as_str())
        .bind(wave_id.as_str())
        .execute(&mut **tx)
        .await?;
    if res.rows_affected() != 1 {
        return Err(CalmError::NotFound(format!("wave {wave_id}")));
    }
    Ok(())
}

pub async fn session_get_by_active_token_hash(
    pool: &SqlitePool,
    hashed_token: &str,
) -> Result<Option<WorkerSession>> {
    let row = sqlx::query(
        r#"SELECT id, wave_id, provider, mode, contract, parent_session_id,
                  requester_session_id, state, mcp_token_hash, thread_id,
                  agent_session_id, active_turn_id, terminal_run_id,
                  handle_state_json, liveness, liveness_probed_at_ms,
                  exit_code, exit_interpretation, spawn_op_id,
                  last_activity_ms, last_thread_status, created_at_ms,
                  updated_at_ms, completed_at_ms
           FROM worker_sessions
           WHERE mcp_token_hash = ?1
             AND state IN ('starting', 'running', 'idle', 'turn_pending')"#,
    )
    .bind(hashed_token)
    .fetch_optional(pool)
    .await?;
    row.as_ref().map(worker_session_from_row).transpose()
}

pub async fn session_get_by_id(
    pool: &SqlitePool,
    id: &WorkerSessionId,
) -> Result<Option<WorkerSession>> {
    let row = sqlx::query(
        r#"SELECT id, wave_id, provider, mode, contract, parent_session_id,
                  requester_session_id, state, mcp_token_hash, thread_id,
                  agent_session_id, active_turn_id, terminal_run_id,
                  handle_state_json, liveness, liveness_probed_at_ms,
                  exit_code, exit_interpretation, spawn_op_id,
                  last_activity_ms, last_thread_status, created_at_ms,
                  updated_at_ms, completed_at_ms
           FROM worker_sessions
           WHERE id = ?1"#,
    )
    .bind(id.as_str())
    .fetch_optional(pool)
    .await?;
    row.as_ref().map(worker_session_from_row).transpose()
}

fn runtime_kind_to_db(kind: &RuntimeKind) -> &'static str {
    match kind {
        RuntimeKind::Terminal => "terminal",
        RuntimeKind::CodexCard => "codex",
        RuntimeKind::ClaudeCard => "claude",
        RuntimeKind::SharedSpec => "shared-spec",
    }
}

fn agent_provider_to_db(provider: &AgentProvider) -> &'static str {
    match provider {
        AgentProvider::Codex => "codex",
        AgentProvider::Claude => "claude",
    }
}

fn run_status_to_db(status: &RunStatus) -> &'static str {
    match status {
        RunStatus::Starting => "starting",
        RunStatus::Running => "running",
        RunStatus::Idle => "idle",
        RunStatus::TurnPending => "turn_pending",
        RunStatus::Failed => "failed",
        RunStatus::Exited => "exited",
        RunStatus::Superseded => "superseded",
    }
}

// PR3b-i (#679): derives the provisional NOT NULL worker-session identity
// from the runtime row's own kind. PR6 overwrites these in place at mint.
pub(crate) fn derive_session_identity(
    kind: &RuntimeKind,
) -> (WorkerProviderKind, SessionMode, WorkerContract) {
    let provider = match kind {
        RuntimeKind::Terminal => WorkerProviderKind::Terminal,
        RuntimeKind::CodexCard | RuntimeKind::SharedSpec => WorkerProviderKind::Codex,
        RuntimeKind::ClaudeCard => WorkerProviderKind::Claude,
    };
    let mode = match provider {
        WorkerProviderKind::Codex => SessionMode::Resumable,
        WorkerProviderKind::Claude | WorkerProviderKind::Terminal => SessionMode::Ephemeral,
    };
    let contract = match kind {
        RuntimeKind::SharedSpec => WorkerContract::Planner,
        _ => WorkerContract::Executor,
    };
    (provider, mode, contract)
}

fn worker_session_state_from_run_status(status: &RunStatus) -> WorkerSessionState {
    match status {
        RunStatus::Starting => WorkerSessionState::Starting,
        RunStatus::Running => WorkerSessionState::Running,
        RunStatus::Idle => WorkerSessionState::Idle,
        RunStatus::TurnPending => WorkerSessionState::TurnPending,
        RunStatus::Failed => WorkerSessionState::Failed,
        RunStatus::Exited => WorkerSessionState::Exited,
        RunStatus::Superseded => WorkerSessionState::Superseded,
    }
}

fn runtime_message(message: impl Into<String>) -> RuntimeRepoError {
    RuntimeRepoError::Message {
        message: message.into(),
    }
}

fn runtime_status_transition_allowed(from: &RunStatus, to: &RunStatus) -> bool {
    match from {
        RunStatus::Starting => matches!(
            to,
            RunStatus::Running
                | RunStatus::Idle
                | RunStatus::TurnPending
                | RunStatus::Failed
                | RunStatus::Exited
        ),
        RunStatus::Running => matches!(to, RunStatus::Idle | RunStatus::Failed | RunStatus::Exited),
        RunStatus::Idle => matches!(
            to,
            RunStatus::Running | RunStatus::Failed | RunStatus::Exited
        ),
        RunStatus::TurnPending => {
            matches!(
                to,
                RunStatus::Running | RunStatus::Failed | RunStatus::Exited
            )
        }
        RunStatus::Failed | RunStatus::Exited | RunStatus::Superseded => false,
    }
}

pub fn worker_session_status_transition_allowed(
    from: WorkerSessionState,
    to: WorkerSessionState,
) -> bool {
    match from {
        WorkerSessionState::Starting => matches!(
            to,
            WorkerSessionState::Running
                | WorkerSessionState::Idle
                | WorkerSessionState::TurnPending
                | WorkerSessionState::Failed
                | WorkerSessionState::Exited
        ),
        WorkerSessionState::Running => matches!(
            to,
            WorkerSessionState::Idle | WorkerSessionState::Failed | WorkerSessionState::Exited
        ),
        WorkerSessionState::Idle => matches!(
            to,
            WorkerSessionState::Running | WorkerSessionState::Failed | WorkerSessionState::Exited
        ),
        WorkerSessionState::TurnPending => {
            matches!(
                to,
                WorkerSessionState::Running
                    | WorkerSessionState::Failed
                    | WorkerSessionState::Exited
            )
        }
        WorkerSessionState::Failed
        | WorkerSessionState::Exited
        | WorkerSessionState::Superseded => false,
    }
}

fn worker_session_parse<T>(column: &str, value: String) -> Result<T>
where
    T: TryFrom<String, Error = String>,
{
    T::try_from(value).map_err(|message| {
        CalmError::Internal(format!("invalid worker_sessions.{column}: {message}"))
    })
}

pub(crate) fn worker_session_from_row(row: &SqliteRow) -> Result<WorkerSession> {
    let handle_state_json = row
        .try_get::<Option<String>, _>("handle_state_json")?
        .map(|json| serde_json::from_str(&json))
        .transpose()?;
    Ok(WorkerSession {
        id: WorkerSessionId(row.try_get("id")?),
        wave_id: WaveId(row.try_get("wave_id")?),
        provider: worker_session_parse("provider", row.try_get("provider")?)?,
        mode: worker_session_parse("mode", row.try_get("mode")?)?,
        contract: worker_session_parse("contract", row.try_get("contract")?)?,
        parent_session_id: row
            .try_get::<Option<String>, _>("parent_session_id")?
            .map(WorkerSessionId),
        requester_session_id: row
            .try_get::<Option<String>, _>("requester_session_id")?
            .map(WorkerSessionId),
        state: worker_session_parse("state", row.try_get("state")?)?,
        mcp_token_hash: row.try_get("mcp_token_hash")?,
        thread_id: row.try_get("thread_id")?,
        agent_session_id: row.try_get("agent_session_id")?,
        active_turn_id: row.try_get("active_turn_id")?,
        terminal_run_id: row.try_get("terminal_run_id")?,
        handle_state_json,
        liveness: worker_session_parse("liveness", row.try_get("liveness")?)?,
        liveness_probed_at_ms: row.try_get("liveness_probed_at_ms")?,
        exit_code: row.try_get("exit_code")?,
        exit_interpretation: row.try_get("exit_interpretation")?,
        spawn_op_id: row.try_get("spawn_op_id")?,
        last_activity_ms: row.try_get::<Option<i64>, _>("last_activity_ms")?,
        last_thread_status: row.try_get::<Option<String>, _>("last_thread_status")?,
        created_at_ms: row.try_get("created_at_ms")?,
        updated_at_ms: row.try_get("updated_at_ms")?,
        completed_at_ms: row.try_get("completed_at_ms")?,
    })
}

pub async fn session_get_tx(
    tx: &mut SessionTx<'_>,
    id: &WorkerSessionId,
) -> Result<Option<WorkerSession>> {
    let row = sqlx::query(
        r#"SELECT id, wave_id, provider, mode, contract, parent_session_id,
                  requester_session_id, state, mcp_token_hash, thread_id,
                  agent_session_id, active_turn_id, terminal_run_id,
                  handle_state_json, liveness, liveness_probed_at_ms,
                  exit_code, exit_interpretation, spawn_op_id,
                  last_activity_ms, last_thread_status, created_at_ms,
                  updated_at_ms, completed_at_ms
           FROM worker_sessions
           WHERE id = ?1"#,
    )
    .bind(id.as_str())
    .fetch_optional(&mut **tx)
    .await?;
    row.as_ref().map(worker_session_from_row).transpose()
}

pub async fn session_set_liveness_tx(
    tx: &mut SessionTx<'_>,
    id: &WorkerSessionId,
    liveness: &Liveness,
    probed_at_ms: i64,
) -> Result<Option<WorkerSession>> {
    let tag = LivenessTag::from(liveness);
    let res = sqlx::query(
        r#"UPDATE worker_sessions
              SET liveness = ?1,
                  liveness_probed_at_ms = ?2
            WHERE id = ?3
              AND state IN ('starting', 'running', 'idle', 'turn_pending')"#,
    )
    .bind(tag.as_db_str())
    .bind(probed_at_ms)
    .bind(id.as_str())
    .execute(&mut **tx)
    .await?;
    if res.rows_affected() == 0 {
        tracing::debug!(
            session_id = %id,
            liveness = tag.as_db_str(),
            "worker session liveness observation skipped for non-active or missing row"
        );
        return Ok(None);
    }
    let Some(session) = session_get_tx(tx, id).await? else {
        return Err(CalmError::Internal(format!(
            "worker session {id} missing after liveness update"
        )));
    };
    Ok(Some(session))
}

/// T2 durable codex worker-liveness feeder (#741 §1.3). Stamps the push-fed
/// `last_activity_ms` / `last_thread_status` columns on an *active* session.
///
/// Like `session_set_liveness_tx` these are `worker_sessions`-ONLY columns with
/// no `runtimes` mirror, so this MUST NOT touch `updated_at_ms` (bumping it
/// would break dual-write parity). 0 rows affected is benign — the session is
/// terminal or missing — and returns `Ok(())`.
pub async fn session_record_activity_tx(
    tx: &mut SessionTx<'_>,
    id: &WorkerSessionId,
    last_activity_ms: i64,
    last_thread_status: &str,
) -> Result<()> {
    let res = sqlx::query(
        r#"UPDATE worker_sessions
              SET last_activity_ms = ?1,
                  last_thread_status = ?2
            WHERE id = ?3
              AND state IN ('starting', 'running', 'idle', 'turn_pending')"#,
    )
    .bind(last_activity_ms)
    .bind(last_thread_status)
    .bind(id.as_str())
    .execute(&mut **tx)
    .await?;
    if res.rows_affected() == 0 {
        tracing::debug!(
            session_id = %id,
            last_thread_status,
            "worker session activity observation skipped for non-active or missing row"
        );
    }
    Ok(())
}

/// T2 durable codex worker-liveness feeder (#741 §1.3), keyed by codex
/// `thread_id` instead of the internal session id. The durable notification
/// subscriber sees only thread ids, so this is the path it writes through.
///
/// Like [`session_record_activity_tx`] these are `worker_sessions`-ONLY columns
/// with no `runtimes` mirror, so this MUST NOT touch `updated_at_ms` (bumping it
/// would break dual-write parity). The match is also pinned to `provider='codex'`
/// (thread ids are codex-scoped). 0 rows affected is benign — no active codex
/// session owns the thread — and returns `Ok(())`.
pub async fn session_record_activity_by_thread_tx(
    tx: &mut SessionTx<'_>,
    thread_id: &str,
    last_activity_ms: i64,
    last_thread_status: &str,
) -> Result<()> {
    let res = sqlx::query(
        r#"UPDATE worker_sessions
              SET last_activity_ms = ?1,
                  last_thread_status = ?2
            WHERE thread_id = ?3
              AND provider = 'codex'
              AND state IN ('starting', 'running', 'idle', 'turn_pending')"#,
    )
    .bind(last_activity_ms)
    .bind(last_thread_status)
    .bind(thread_id)
    .execute(&mut **tx)
    .await?;
    if res.rows_affected() == 0 {
        tracing::debug!(
            thread_id,
            last_thread_status,
            "worker session activity-by-thread observation skipped for non-active or missing row"
        );
    }
    Ok(())
}

pub async fn session_insert_tx(
    tx: &mut SessionTx<'_>,
    session: WorkerSession,
) -> Result<WorkerSession> {
    let handle_state_json = session
        .handle_state_json
        .as_ref()
        .map(serde_json::to_string)
        .transpose()?;
    sqlx::query(
        r#"INSERT INTO worker_sessions (
               id, wave_id, provider, mode, contract, parent_session_id,
               requester_session_id, state, mcp_token_hash, thread_id,
               agent_session_id, active_turn_id, terminal_run_id,
               handle_state_json, liveness, liveness_probed_at_ms, exit_code,
               exit_interpretation, spawn_op_id, last_activity_ms,
               last_thread_status, created_at_ms, updated_at_ms,
               completed_at_ms
           )
           VALUES (
               ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
               ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24
           )"#,
    )
    .bind(session.id.as_str())
    .bind(session.wave_id.as_str())
    .bind(session.provider.as_db_str())
    .bind(session.mode.as_db_str())
    .bind(session.contract.as_db_str())
    .bind(
        session
            .parent_session_id
            .as_ref()
            .map(WorkerSessionId::as_str),
    )
    .bind(
        session
            .requester_session_id
            .as_ref()
            .map(WorkerSessionId::as_str),
    )
    .bind(session.state.as_db_str())
    .bind(&session.mcp_token_hash)
    .bind(&session.thread_id)
    .bind(&session.agent_session_id)
    .bind(&session.active_turn_id)
    .bind(&session.terminal_run_id)
    .bind(&handle_state_json)
    .bind(session.liveness.as_db_str())
    .bind(session.liveness_probed_at_ms)
    .bind(session.exit_code)
    .bind(&session.exit_interpretation)
    .bind(&session.spawn_op_id)
    .bind(session.last_activity_ms)
    .bind(&session.last_thread_status)
    .bind(session.created_at_ms)
    .bind(session.updated_at_ms)
    .bind(session.completed_at_ms)
    .execute(&mut **tx)
    .await?;
    session_get_tx(tx, &session.id).await?.ok_or_else(|| {
        CalmError::Internal(format!(
            "worker session {} missing after insert",
            session.id
        ))
    })
}

async fn worker_session_current_state_tx(
    tx: &mut SessionTx<'_>,
    id: &WorkerSessionId,
) -> Result<WorkerSessionState> {
    let row = sqlx::query("SELECT state FROM worker_sessions WHERE id = ?1")
        .bind(id.as_str())
        .fetch_optional(&mut **tx)
        .await?;
    let Some(row) = row else {
        return Err(CalmError::NotFound(format!("worker session {id}")));
    };
    worker_session_parse("state", row.try_get("state")?)
}

pub async fn session_state_transition_tx(
    tx: &mut SessionTx<'_>,
    id: &WorkerSessionId,
    to: WorkerSessionState,
) -> Result<WorkerSession> {
    let now = now_ms();
    let completed_at_ms = to.is_terminal().then_some(now);
    session_state_transition_at_tx(tx, id, to, now, completed_at_ms).await
}

pub async fn session_commit_exit_tx(
    tx: &mut SessionTx<'_>,
    id: &WorkerSessionId,
    to: WorkerSessionState,
    liveness_probed_at_ms: i64,
    exit_code: Option<i32>,
    exit_interpretation: &str,
) -> Result<WorkerSession> {
    let from = worker_session_current_state_tx(tx, id).await?;
    if !worker_session_status_transition_allowed(from, to) {
        return Err(CalmError::Conflict(format!(
            "illegal worker session state transition {id}: {} -> {}",
            from.as_db_str(),
            to.as_db_str()
        )));
    }

    let res = sqlx::query(
        r#"UPDATE worker_sessions
              SET state = ?1,
                  liveness = 'exited',
                  liveness_probed_at_ms = ?2,
                  exit_code = ?3,
                  exit_interpretation = ?4,
                  completed_at_ms = ?2,
                  updated_at_ms = ?2
            WHERE id = ?5
              AND state = ?6"#,
    )
    .bind(to.as_db_str())
    .bind(liveness_probed_at_ms)
    .bind(exit_code)
    .bind(exit_interpretation)
    .bind(id.as_str())
    .bind(from.as_db_str())
    .execute(&mut **tx)
    .await?;
    if res.rows_affected() == 0 {
        return Err(CalmError::Conflict(format!(
            "worker session {id} changed during exit commit"
        )));
    }
    session_get_tx(tx, id).await?.ok_or_else(|| {
        CalmError::Internal(format!("worker session {id} missing after exit commit"))
    })
}

async fn session_state_transition_at_tx(
    tx: &mut SessionTx<'_>,
    id: &WorkerSessionId,
    to: WorkerSessionState,
    now: i64,
    completed_at_ms: Option<i64>,
) -> Result<WorkerSession> {
    let from = worker_session_current_state_tx(tx, id).await?;
    if !worker_session_status_transition_allowed(from, to) {
        return Err(CalmError::Conflict(format!(
            "illegal worker session state transition {id}: {} -> {}",
            from.as_db_str(),
            to.as_db_str()
        )));
    }

    let completed = i64::from(completed_at_ms.is_some());
    let res = sqlx::query(
        r#"UPDATE worker_sessions
              SET state = ?1,
                  updated_at_ms = ?2,
                  completed_at_ms = CASE
                    WHEN ?3 = 1 THEN ?4
                    ELSE completed_at_ms
                  END
            WHERE id = ?5
              AND state = ?6"#,
    )
    .bind(to.as_db_str())
    .bind(now)
    .bind(completed)
    .bind(completed_at_ms)
    .bind(id.as_str())
    .bind(from.as_db_str())
    .execute(&mut **tx)
    .await?;
    if res.rows_affected() == 0 {
        return Err(CalmError::Conflict(format!(
            "worker session {id} changed during state transition"
        )));
    }
    session_get_tx(tx, id)
        .await?
        .ok_or_else(|| CalmError::Internal(format!("worker session {id} missing after transition")))
}

fn ensure_runtime_status_transition(
    id: &RuntimeId,
    from: &RunStatus,
    to: &RunStatus,
) -> RuntimeResult<()> {
    if runtime_status_transition_allowed(from, to) {
        Ok(())
    } else {
        Err(RuntimeRepoError::IllegalStatusTransition {
            id: id.clone(),
            attempted: to.clone(),
        })
    }
}

fn runtime_session_error(err: CalmError) -> RuntimeRepoError {
    runtime_message(err.to_string())
}

async fn worker_session_wave_id_for_card_tx(
    tx: &mut RuntimeTx<'_>,
    card_id: &str,
) -> RuntimeResult<WaveId> {
    let row = sqlx::query("SELECT wave_id FROM cards WHERE id = ?1")
        .bind(card_id)
        .fetch_optional(&mut **tx)
        .await?;
    let Some(row) = row else {
        return Err(runtime_message(format!(
            "card {card_id} missing while mirroring runtime session"
        )));
    };
    Ok(WaveId(row.try_get("wave_id")?))
}

fn worker_session_from_runtime_init(init: &RuntimeInit, wave_id: WaveId) -> WorkerSession {
    let (provider, mode, contract) = derive_session_identity(&init.kind);
    WorkerSession {
        id: WorkerSessionId(init.id.clone()),
        wave_id,
        provider,
        mode,
        contract,
        parent_session_id: None,
        requester_session_id: None,
        state: worker_session_state_from_run_status(&init.status),
        mcp_token_hash: None,
        thread_id: init.thread_id.clone(),
        agent_session_id: init.session_id.clone(),
        active_turn_id: init.active_turn_id.clone(),
        terminal_run_id: init.terminal_run_id.clone(),
        handle_state_json: init.handle_state_json.clone(),
        liveness: LivenessTag::Unknown,
        liveness_probed_at_ms: None,
        exit_code: None,
        exit_interpretation: None,
        spawn_op_id: init.spawn_op_id.clone(),
        last_activity_ms: None,
        last_thread_status: None,
        created_at_ms: init.now_ms,
        updated_at_ms: init.now_ms,
        completed_at_ms: None,
    }
}

fn worker_session_from_card_runtime(runtime: &CardRuntime, wave_id: WaveId) -> WorkerSession {
    let (provider, mode, contract) = derive_session_identity(&runtime.kind);
    WorkerSession {
        id: WorkerSessionId(runtime.id.clone()),
        wave_id,
        provider,
        mode,
        contract,
        parent_session_id: None,
        requester_session_id: None,
        state: worker_session_state_from_run_status(&runtime.status),
        mcp_token_hash: None,
        thread_id: runtime.thread_id.clone(),
        agent_session_id: runtime.session_id.clone(),
        active_turn_id: runtime.active_turn_id.clone(),
        terminal_run_id: runtime.terminal_run_id.clone(),
        handle_state_json: runtime.handle_state_json.clone(),
        liveness: LivenessTag::Unknown,
        liveness_probed_at_ms: None,
        exit_code: None,
        exit_interpretation: None,
        spawn_op_id: None,
        last_activity_ms: None,
        last_thread_status: None,
        created_at_ms: runtime.created_at_ms,
        updated_at_ms: runtime.updated_at_ms,
        completed_at_ms: runtime.completed_at_ms,
    }
}

async fn session_refresh_deferred_planner_tx(
    tx: &mut RuntimeTx<'_>,
    existing: WorkerSession,
    desired: WorkerSession,
) -> RuntimeResult<WorkerSession> {
    if desired.contract != WorkerContract::Planner
        || existing.contract != WorkerContract::Planner
        || existing.state != WorkerSessionState::Starting
        || existing.wave_id != desired.wave_id
        || existing.provider != desired.provider
        || existing.mode != desired.mode
        || existing.parent_session_id.is_some()
        || existing.requester_session_id.is_some()
        || existing.completed_at_ms.is_some()
    {
        return Err(runtime_message(format!(
            "worker session {} already exists and is not a deferred planner placeholder",
            desired.id
        )));
    }

    let handle_state_json = desired
        .handle_state_json
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(|e| runtime_message(e.to_string()))?;
    let res = sqlx::query(
        r#"UPDATE worker_sessions
              SET state = ?1,
                  thread_id = ?2,
                  agent_session_id = ?3,
                  active_turn_id = ?4,
                  terminal_run_id = ?5,
                  handle_state_json = ?6,
                  liveness = ?7,
                  liveness_probed_at_ms = ?8,
                  exit_code = ?9,
                  exit_interpretation = ?10,
                  spawn_op_id = ?11,
                  created_at_ms = ?12,
                  updated_at_ms = ?13,
                  completed_at_ms = ?14
            WHERE id = ?15
              AND contract = 'planner'
              AND state = 'starting'"#,
    )
    .bind(desired.state.as_db_str())
    .bind(&desired.thread_id)
    .bind(&desired.agent_session_id)
    .bind(&desired.active_turn_id)
    .bind(&desired.terminal_run_id)
    .bind(&handle_state_json)
    .bind(desired.liveness.as_db_str())
    .bind(desired.liveness_probed_at_ms)
    .bind(desired.exit_code)
    .bind(&desired.exit_interpretation)
    .bind(&desired.spawn_op_id)
    .bind(desired.created_at_ms)
    .bind(desired.updated_at_ms)
    .bind(desired.completed_at_ms)
    .bind(desired.id.as_str())
    .execute(&mut **tx)
    .await
    .map_err(|e| runtime_message(e.to_string()))?;
    if res.rows_affected() != 1 {
        return Err(runtime_message(format!(
            "deferred planner placeholder {} changed before runtime mirror refresh",
            desired.id
        )));
    }
    session_get_tx(tx, &desired.id)
        .await
        .map_err(runtime_session_error)?
        .ok_or_else(|| {
            runtime_message(format!(
                "worker session {} missing after deferred planner refresh",
                desired.id
            ))
        })
}

async fn session_insert_or_refresh_start_mirror_tx(
    tx: &mut RuntimeTx<'_>,
    session: WorkerSession,
) -> RuntimeResult<WorkerSession> {
    if let Some(existing) = session_get_tx(tx, &session.id)
        .await
        .map_err(runtime_session_error)?
    {
        session_refresh_deferred_planner_tx(tx, existing, session).await
    } else {
        session_insert_tx(tx, session)
            .await
            .map_err(runtime_session_error)
    }
}

async fn card_session_link_tx(
    tx: &mut RuntimeTx<'_>,
    card_id: &str,
    session_id: &WorkerSessionId,
) -> RuntimeResult<()> {
    let res = sqlx::query("UPDATE cards SET session_id = ?1 WHERE id = ?2")
        .bind(session_id.as_str())
        .bind(card_id)
        .execute(&mut **tx)
        .await
        .map_err(|e| runtime_message(e.to_string()))?;
    if res.rows_affected() != 1 {
        return Err(runtime_message(format!(
            "card {card_id} missing while linking worker session {session_id}"
        )));
    }
    Ok(())
}

async fn session_mirror_card_mcp_token_tx(
    tx: &mut RuntimeTx<'_>,
    card_id: &str,
    session: &WorkerSession,
) -> RuntimeResult<()> {
    if !session.state.is_active_authority() || session.mcp_token_hash.is_some() {
        return Ok(());
    }

    let hashed: Option<String> = sqlx::query_scalar(
        r#"SELECT cmt.hashed_token
             FROM card_mcp_tokens cmt
            WHERE cmt.card_id = ?1
              AND 1 = (
                  SELECT COUNT(*)
                    FROM card_mcp_tokens dup
                   WHERE dup.hashed_token = cmt.hashed_token
              )
              AND NOT EXISTS (
                  SELECT 1
                    FROM worker_sessions other
                   WHERE other.id != ?2
                     AND other.mcp_token_hash = cmt.hashed_token
              )
            LIMIT 1"#,
    )
    .bind(card_id)
    .bind(session.id.as_str())
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| runtime_message(e.to_string()))?;

    if let Some(hashed) = hashed {
        session_mcp_token_set_tx(tx, session.id.as_str(), &hashed)
            .await
            .map_err(runtime_session_error)?;
    }
    Ok(())
}

async fn session_repoint_current_links_tx(
    tx: &mut RuntimeTx<'_>,
    card_id: &str,
    session: &WorkerSession,
) -> RuntimeResult<()> {
    // Runtime/session identity invariant: whenever a runtime/session becomes
    // current for a card, cards.session_id must follow it. Active sessions
    // also inherit the card MCP token when doing so cannot violate ws_token_idx.
    // Planner sessions that are live own waves.root_session_id for recorder
    // gating.
    session_mirror_card_mcp_token_tx(tx, card_id, session).await?;
    if session.contract == WorkerContract::Planner && session.state.is_active_authority() {
        session_mark_wave_root_tx(tx, &session.wave_id, &session.id)
            .await
            .map_err(runtime_session_error)?;
    }
    card_session_link_tx(tx, card_id, &session.id).await
}

async fn session_start_mirror_tx(
    tx: &mut RuntimeTx<'_>,
    init: &RuntimeInit,
) -> RuntimeResult<WorkerSession> {
    let wave_id = worker_session_wave_id_for_card_tx(tx, &init.card_id).await?;
    let session = worker_session_from_runtime_init(init, wave_id);
    let session = session_insert_or_refresh_start_mirror_tx(tx, session).await?;
    session_repoint_current_links_tx(tx, &init.card_id, &session).await?;
    Ok(session)
}

pub async fn session_prepare_deferred_spec_tx(
    tx: &mut RuntimeTx<'_>,
    init: &RuntimeInit,
) -> RuntimeResult<WorkerSession> {
    if init.kind != RuntimeKind::SharedSpec || init.status != RunStatus::Starting {
        return Err(runtime_message(
            "deferred spec session placeholders require a starting shared-spec runtime init",
        ));
    }
    if init.thread_id.is_some() || init.terminal_run_id.is_some() || init.session_id.is_some() {
        return Err(runtime_message(
            "deferred spec session placeholders must not have a thread, terminal run, or session",
        ));
    }
    let wave_id = worker_session_wave_id_for_card_tx(tx, &init.card_id).await?;
    let session = worker_session_from_runtime_init(init, wave_id);
    let session = session_insert_or_refresh_start_mirror_tx(tx, session).await?;
    session_repoint_current_links_tx(tx, &init.card_id, &session).await?;
    Ok(session)
}

async fn session_supersede_active_tx(
    tx: &mut RuntimeTx<'_>,
    id: &RuntimeId,
    now: i64,
) -> RuntimeResult<()> {
    let res = sqlx::query(
        r#"UPDATE worker_sessions
              SET state = 'superseded',
                  updated_at_ms = ?1,
                  completed_at_ms = COALESCE(completed_at_ms, ?1)
            WHERE id = ?2
              AND state IN ('starting', 'running', 'idle', 'turn_pending')"#,
    )
    .bind(now)
    .bind(id)
    .execute(&mut **tx)
    .await?;
    if res.rows_affected() == 0 {
        return Err(runtime_message(format!(
            "active worker session {id} not found for supersede"
        )));
    }
    Ok(())
}

async fn session_set_status_mirror_tx(
    tx: &mut RuntimeTx<'_>,
    id: &RuntimeId,
    status: RunStatus,
    now: i64,
) -> RuntimeResult<()> {
    session_state_transition_at_tx(
        tx,
        &WorkerSessionId(id.clone()),
        worker_session_state_from_run_status(&status),
        now,
        None,
    )
    .await
    .map(|_| ())
    .map_err(runtime_session_error)
}

async fn session_complete_mirror_tx(
    tx: &mut RuntimeTx<'_>,
    id: &RuntimeId,
    terminal_status: RunStatus,
    now: i64,
) -> RuntimeResult<()> {
    session_state_transition_at_tx(
        tx,
        &WorkerSessionId(id.clone()),
        worker_session_state_from_run_status(&terminal_status),
        now,
        Some(now),
    )
    .await
    .map(|_| ())
    .map_err(runtime_session_error)
}

async fn session_bind_attribution_mirror_tx(
    tx: &mut RuntimeTx<'_>,
    id: &RuntimeId,
    attr: &ThreadAttribution,
    now: i64,
) -> RuntimeResult<()> {
    let res = sqlx::query(
        r#"UPDATE worker_sessions
              SET thread_id = ?1,
                  agent_session_id = ?2,
                  active_turn_id = ?3,
                  updated_at_ms = ?4
            WHERE id = ?5"#,
    )
    .bind(&attr.thread_id)
    .bind(&attr.session_id)
    .bind(&attr.active_turn_id)
    .bind(now)
    .bind(id)
    .execute(&mut **tx)
    .await?;
    if res.rows_affected() == 0 {
        return Err(runtime_message(format!("worker session {id} not found")));
    }
    Ok(())
}

async fn session_clear_terminal_run_id_mirror_tx(
    tx: &mut RuntimeTx<'_>,
    id: &RuntimeId,
    now: i64,
) -> RuntimeResult<()> {
    let res = sqlx::query(
        r#"UPDATE worker_sessions
              SET terminal_run_id = NULL,
                  updated_at_ms = ?1
            WHERE id = ?2"#,
    )
    .bind(now)
    .bind(id)
    .execute(&mut **tx)
    .await?;
    if res.rows_affected() == 0 {
        return Err(runtime_message(format!("worker session {id} not found")));
    }
    Ok(())
}

async fn session_set_handle_state_mirror_tx(
    tx: &mut RuntimeTx<'_>,
    id: &RuntimeId,
    state_text: &Option<String>,
    now: i64,
) -> RuntimeResult<()> {
    sqlx::query(
        r#"UPDATE worker_sessions
              SET handle_state_json = ?1,
                  updated_at_ms = ?2
            WHERE id = ?3
              AND state IN ('starting', 'running', 'idle', 'turn_pending')"#,
    )
    .bind(state_text)
    .bind(now)
    .bind(id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn session_set_active_turn_mirror_tx(
    tx: &mut RuntimeTx<'_>,
    id: &RuntimeId,
    turn_id: Option<&str>,
    now: i64,
) -> RuntimeResult<()> {
    let res = sqlx::query(
        r#"UPDATE worker_sessions
              SET active_turn_id = ?1,
                  updated_at_ms = ?2
            WHERE id = ?3"#,
    )
    .bind(turn_id)
    .bind(now)
    .bind(id)
    .execute(&mut **tx)
    .await?;
    if res.rows_affected() == 0 {
        return Err(runtime_message(format!("worker session {id} not found")));
    }
    Ok(())
}

async fn session_set_harness_observation_tx(
    tx: &mut RuntimeTx<'_>,
    id: &RuntimeId,
    status: RunStatus,
    thread_id: Option<&str>,
    active_turn_id: Option<&str>,
    now: i64,
) -> RuntimeResult<()> {
    sqlx::query(
        r#"UPDATE worker_sessions
              SET state = ?1,
                  thread_id = COALESCE(?2, thread_id),
                  active_turn_id = ?3,
                  updated_at_ms = ?4
            WHERE id = ?5
              AND state IN ('starting', 'running', 'idle', 'turn_pending')"#,
    )
    .bind(worker_session_state_from_run_status(&status).as_db_str())
    .bind(thread_id)
    .bind(active_turn_id)
    .bind(now)
    .bind(id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn session_fail_if_active_tx(
    tx: &mut RuntimeTx<'_>,
    id: &RuntimeId,
    now: i64,
) -> RuntimeResult<()> {
    sqlx::query(
        r#"UPDATE worker_sessions
              SET state = 'failed',
                  updated_at_ms = ?1,
                  completed_at_ms = ?1
            WHERE id = ?2
              AND state IN ('starting', 'running', 'idle', 'turn_pending')"#,
    )
    .bind(now)
    .bind(id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn session_mark_superseded_tx(
    tx: &mut RuntimeTx<'_>,
    id: &RuntimeId,
    now: i64,
) -> RuntimeResult<()> {
    sqlx::query(
        r#"UPDATE worker_sessions
              SET state = 'superseded',
                  updated_at_ms = ?1,
                  completed_at_ms = COALESCE(completed_at_ms, ?1)
            WHERE id = ?2"#,
    )
    .bind(now)
    .bind(id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn session_get_required_for_runtime_tx(
    tx: &mut RuntimeTx<'_>,
    id: &RuntimeId,
    context: &str,
) -> RuntimeResult<WorkerSession> {
    session_get_tx(tx, &WorkerSessionId(id.clone()))
        .await
        .map_err(runtime_session_error)?
        .ok_or_else(|| runtime_message(format!("worker session {id} missing while {context}")))
}

async fn session_restore_from_superseded_tx(
    tx: &mut RuntimeTx<'_>,
    id: &RuntimeId,
    status: RunStatus,
    now: i64,
) -> RuntimeResult<WorkerSession> {
    let state_db = worker_session_state_from_run_status(&status).as_db_str();
    let res = sqlx::query(
        r#"UPDATE worker_sessions
              SET state = ?1,
                  updated_at_ms = ?2,
                  completed_at_ms = NULL
            WHERE id = ?3
              AND state = 'superseded'"#,
    )
    .bind(state_db)
    .bind(now)
    .bind(id)
    .execute(&mut **tx)
    .await?;
    if res.rows_affected() > 0 {
        return session_get_required_for_runtime_tx(tx, id, "restoring old spec harness session")
            .await;
    }

    let current: Option<(String,)> =
        sqlx::query_as("SELECT state FROM worker_sessions WHERE id = ?1")
            .bind(id)
            .fetch_optional(&mut **tx)
            .await?;
    match current {
        Some((current,)) if current == state_db => {
            session_get_required_for_runtime_tx(tx, id, "restoring old spec harness session").await
        }
        Some((current,)) => Err(runtime_message(format!(
            "worker session {id} has state {current}; cannot restore old spec harness session to {state_db}"
        ))),
        None => Err(runtime_message(format!(
            "worker session {id} missing while restoring old spec harness session"
        ))),
    }
}

async fn runtime_current_status_tx(
    tx: &mut RuntimeTx<'_>,
    id: &RuntimeId,
) -> RuntimeResult<RunStatus> {
    let row = sqlx::query(
        r#"SELECT state FROM worker_sessions ws
           WHERE ws.id = ?1
             AND EXISTS (SELECT 1 FROM runtimes r WHERE r.id = ws.id)"#,
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some(row) = row else {
        return Err(runtime_message(format!("runtime {id} not found")));
    };
    run_status_from_db(row.try_get::<String, _>("state")?.as_str())
}

async fn runtime_get_by_id_from_pool(
    pool: &SqlitePool,
    id: &RuntimeId,
) -> RuntimeResult<Option<CardRuntime>> {
    let sql = format!(
        r#"{WS_BACKED_CARD_RUNTIME_SELECT}
           WHERE ws.id = ?1
             AND EXISTS (SELECT 1 FROM runtimes r WHERE r.id = ws.id)"#
    );
    let row = sqlx::query(&sql).bind(id).fetch_optional(pool).await?;
    row.as_ref().map(card_runtime_from_ws_join_row).transpose()
}

async fn runtime_get_active_for_card_from_pool(
    pool: &SqlitePool,
    card_id: &str,
) -> RuntimeResult<Option<CardRuntime>> {
    let sql = format!(
        r#"{WS_BACKED_CARD_RUNTIME_SELECT}
           WHERE c.id = ?1
             AND ws.state IN ('starting', 'running', 'idle', 'turn_pending')
             AND EXISTS (SELECT 1 FROM runtimes r WHERE r.id = ws.id)
           ORDER BY ws.updated_at_ms DESC, ws.created_at_ms DESC, ws.id DESC
           LIMIT 1"#,
    );
    let row = sqlx::query(&sql).bind(card_id).fetch_optional(pool).await?;
    row.as_ref().map(card_runtime_from_ws_join_row).transpose()
}

async fn runtime_get_projectable_for_card_from_pool(
    pool: &SqlitePool,
    card_id: &str,
) -> RuntimeResult<Option<CardRuntime>> {
    let sql = format!(
        r#"{WS_BACKED_CARD_RUNTIME_SELECT}
           WHERE c.id = ?1
             AND ws.state != 'superseded'
             AND EXISTS (SELECT 1 FROM runtimes r WHERE r.id = ws.id)
           LIMIT 1"#,
    );
    let row = sqlx::query(&sql).bind(card_id).fetch_optional(pool).await?;
    row.as_ref().map(card_runtime_from_ws_join_row).transpose()
}

async fn runtime_get_projectable_for_cards_from_pool(
    pool: &SqlitePool,
    card_ids: &[RuntimeCardId],
) -> RuntimeResult<HashMap<RuntimeCardId, CardRuntime>> {
    if card_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let mut query = projectable_runtimes_for_cards_query(card_ids);
    let rows = query.build().fetch_all(pool).await?;
    projectable_runtimes_for_cards_from_rows(rows)
}

async fn runtime_get_active_by_thread_from_pool(
    pool: &SqlitePool,
    provider: AgentProvider,
    thread_id: &str,
) -> RuntimeResult<Option<CardRuntime>> {
    let sql = format!(
        r#"{WS_BACKED_CARD_RUNTIME_SELECT}
           WHERE ws.provider = ?1 AND ws.thread_id = ?2
             AND ws.state IN ('starting','running','idle','turn_pending')
           ORDER BY ws.updated_at_ms DESC, ws.created_at_ms DESC, ws.id DESC
           LIMIT 1"#,
    );
    let row = sqlx::query(&sql)
        .bind(agent_provider_to_db(&provider))
        .bind(thread_id)
        .fetch_optional(pool)
        .await?;
    row.as_ref().map(card_runtime_from_ws_join_row).transpose()
}

async fn runtime_get_active_by_session_from_pool(
    pool: &SqlitePool,
    provider: AgentProvider,
    session_id: &str,
) -> RuntimeResult<Option<CardRuntime>> {
    let sql = format!(
        r#"{WS_BACKED_CARD_RUNTIME_SELECT}
           WHERE ws.provider = ?1 AND ws.agent_session_id = ?2
             AND ws.state IN ('starting','running','idle','turn_pending')
           ORDER BY ws.updated_at_ms DESC, ws.created_at_ms DESC, ws.id DESC
           LIMIT 1"#,
    );
    let row = sqlx::query(&sql)
        .bind(agent_provider_to_db(&provider))
        .bind(session_id)
        .fetch_optional(pool)
        .await?;
    row.as_ref().map(card_runtime_from_ws_join_row).transpose()
}

async fn runtime_active_shared_thread_attribution_from_pool(
    pool: &SqlitePool,
) -> RuntimeResult<Vec<(String, String)>> {
    sqlx::query_as::<_, (String, String)>(
        r#"SELECT ws.thread_id, c.id AS card_id
           FROM worker_sessions ws JOIN cards c ON c.session_id = ws.id
           WHERE ws.provider = 'codex' AND ws.thread_id IS NOT NULL
             AND ws.state IN ('starting','running','idle','turn_pending')
           ORDER BY ws.created_at_ms ASC, c.id ASC"#,
    )
    .fetch_all(pool)
    .await
    .map_err(Into::into)
}

async fn runtimes_active_for_kind_from_pool(
    pool: &SqlitePool,
    kind: RuntimeKind,
) -> RuntimeResult<Vec<CardRuntime>> {
    let (provider, _mode, contract) = derive_session_identity(&kind);
    let sql = format!(
        r#"{WS_BACKED_CARD_RUNTIME_SELECT}
           WHERE ws.provider = ?1
             AND ws.contract = ?2
             AND ws.state IN ('starting', 'running', 'idle', 'turn_pending')
             AND EXISTS (SELECT 1 FROM runtimes r WHERE r.id = ws.id)
           ORDER BY ws.created_at_ms ASC, c.id ASC"#
    );
    let rows = sqlx::query(&sql)
        .bind(provider.as_db_str())
        .bind(contract.as_db_str())
        .fetch_all(pool)
        .await?;
    rows.iter().map(card_runtime_from_ws_join_row).collect()
}

pub async fn runtime_get_by_id_tx(
    tx: &mut RuntimeTx<'_>,
    id: &RuntimeId,
) -> RuntimeResult<Option<CardRuntime>> {
    let row = sqlx::query(
        r#"SELECT id, card_id, kind, agent_provider, status, terminal_run_id,
                  thread_id, session_id, active_turn_id, handle_state_json,
                  lease_owner, lease_until_ms, created_at_ms, updated_at_ms,
                  completed_at_ms
           FROM runtimes
           WHERE id = ?1"#,
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?;
    row.as_ref().map(card_runtime_from_row).transpose()
}

pub async fn runtime_get_active_for_card_tx(
    tx: &mut RuntimeTx<'_>,
    card_id: &str,
) -> RuntimeResult<Option<CardRuntime>> {
    let row = sqlx::query(
        r#"SELECT id, card_id, kind, agent_provider, status, terminal_run_id,
                  thread_id, session_id, active_turn_id, handle_state_json,
                  lease_owner, lease_until_ms, created_at_ms, updated_at_ms,
                  completed_at_ms
           FROM runtimes
           WHERE card_id = ?1
             AND status IN ('starting', 'running', 'idle', 'turn_pending')
           ORDER BY updated_at_ms DESC, created_at_ms DESC, id DESC
           LIMIT 1"#,
    )
    .bind(card_id)
    .fetch_optional(&mut **tx)
    .await?;
    row.as_ref().map(card_runtime_from_row).transpose()
}

pub async fn runtime_start_tx(
    tx: &mut RuntimeTx<'_>,
    init: RuntimeInit,
) -> RuntimeResult<CardRuntime> {
    let kind = runtime_kind_to_db(&init.kind);
    let agent_provider = init.agent_provider.as_ref().map(agent_provider_to_db);
    let status = run_status_to_db(&init.status);
    let handle_state_json = init
        .handle_state_json
        .as_ref()
        .map(serde_json::to_string)
        .transpose()?;

    sqlx::query(
        r#"INSERT INTO runtimes (
               id, card_id, kind, agent_provider, status, terminal_run_id,
               thread_id, session_id, active_turn_id, handle_state_json,
               lease_owner, lease_until_ms, created_at_ms, updated_at_ms,
               completed_at_ms
           )
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?13, NULL)"#,
    )
    .bind(&init.id)
    .bind(&init.card_id)
    .bind(kind)
    .bind(agent_provider)
    .bind(status)
    .bind(&init.terminal_run_id)
    .bind(&init.thread_id)
    .bind(&init.session_id)
    .bind(&init.active_turn_id)
    .bind(&handle_state_json)
    .bind(&init.lease_owner)
    .bind(init.lease_until_ms)
    .bind(init.now_ms)
    .execute(&mut **tx)
    .await?;

    session_start_mirror_tx(tx, &init).await?;

    runtime_get_by_id_tx(tx, &init.id)
        .await?
        .ok_or_else(|| runtime_message(format!("runtime {} missing after insert", init.id)))
}

pub async fn runtime_supersede_tx(
    tx: &mut RuntimeTx<'_>,
    id: &RuntimeId,
    new_init: RuntimeInit,
) -> RuntimeResult<CardRuntime> {
    let res = sqlx::query(
        r#"UPDATE runtimes
              SET status = 'superseded',
                  updated_at_ms = ?1,
                  completed_at_ms = COALESCE(completed_at_ms, ?1)
            WHERE id = ?2
              AND status IN ('starting', 'running', 'idle', 'turn_pending')"#,
    )
    .bind(new_init.now_ms)
    .bind(id)
    .execute(&mut **tx)
    .await?;
    if res.rows_affected() == 0 {
        return Err(runtime_message(format!(
            "active runtime {id} not found for supersede"
        )));
    }

    session_supersede_active_tx(tx, id, new_init.now_ms).await?;
    runtime_start_tx(tx, new_init).await
}

pub async fn runtime_set_status_tx(
    tx: &mut RuntimeTx<'_>,
    id: &RuntimeId,
    status: RunStatus,
) -> RuntimeResult<()> {
    if status == RunStatus::Superseded {
        return Err(RuntimeRepoError::IllegalStatusTransition {
            id: id.clone(),
            attempted: status,
        });
    }

    let current = runtime_current_status_tx(tx, id).await?;
    ensure_runtime_status_transition(id, &current, &status)?;

    let now = now_ms();
    let res = sqlx::query(
        r#"UPDATE runtimes
              SET status = ?1,
                  updated_at_ms = ?2
            WHERE id = ?3"#,
    )
    .bind(run_status_to_db(&status))
    .bind(now)
    .bind(id)
    .execute(&mut **tx)
    .await?;
    if res.rows_affected() == 0 {
        return Err(runtime_message(format!("runtime {id} not found")));
    }
    session_set_status_mirror_tx(tx, id, status, now).await?;
    Ok(())
}

pub async fn runtime_set_status_for_card_tx(
    tx: &mut RuntimeTx<'_>,
    card_id: &str,
    status: RunStatus,
) -> RuntimeResult<()> {
    let Some(runtime) = runtime_get_active_for_card_tx(tx, card_id).await? else {
        return Ok(());
    };
    runtime_set_status_tx(tx, &runtime.id, status).await
}

pub async fn runtime_bind_attribution_tx(
    tx: &mut RuntimeTx<'_>,
    id: &RuntimeId,
    attr: ThreadAttribution,
) -> RuntimeResult<()> {
    if &attr.runtime_id != id {
        return Err(runtime_message(format!(
            "runtime attribution id mismatch: arg={id}, attr={}",
            attr.runtime_id
        )));
    }

    let now = now_ms();
    let res = sqlx::query(
        r#"UPDATE runtimes
              SET agent_provider = ?1,
                  thread_id = ?2,
                  session_id = ?3,
                  active_turn_id = ?4,
                  updated_at_ms = ?5
            WHERE id = ?6"#,
    )
    .bind(agent_provider_to_db(&attr.provider))
    .bind(&attr.thread_id)
    .bind(&attr.session_id)
    .bind(&attr.active_turn_id)
    .bind(now)
    .bind(id)
    .execute(&mut **tx)
    .await?;
    if res.rows_affected() == 0 {
        return Err(runtime_message(format!("runtime {id} not found")));
    }
    session_bind_attribution_mirror_tx(tx, id, &attr, now).await?;
    Ok(())
}

pub async fn runtime_clear_terminal_run_id_tx(
    tx: &mut RuntimeTx<'_>,
    id: &RuntimeId,
) -> RuntimeResult<()> {
    let now = now_ms();
    let res = sqlx::query(
        r#"UPDATE runtimes
              SET terminal_run_id = NULL, updated_at_ms = ?1
            WHERE id = ?2"#,
    )
    .bind(now)
    .bind(id)
    .execute(&mut **tx)
    .await?;
    if res.rows_affected() == 0 {
        return Err(runtime_message(format!("runtime {id} not found")));
    }
    session_clear_terminal_run_id_mirror_tx(tx, id, now).await?;
    Ok(())
}

pub async fn runtime_set_handle_state_tx(
    tx: &mut RuntimeTx<'_>,
    id: &RuntimeId,
    state: Option<serde_json::Value>,
) -> RuntimeResult<()> {
    let state_text = state.as_ref().map(serde_json::to_string).transpose()?;
    let now = now_ms();
    sqlx::query(
        r#"UPDATE runtimes
              SET handle_state_json = ?1,
                  updated_at_ms = ?2
            WHERE id = ?3
              AND status IN ('starting', 'running', 'idle', 'turn_pending')"#,
    )
    .bind(&state_text)
    .bind(now)
    .bind(id)
    .execute(&mut **tx)
    .await?;
    session_set_handle_state_mirror_tx(tx, id, &state_text, now).await?;
    Ok(())
}

pub async fn runtime_set_active_turn_tx(
    tx: &mut RuntimeTx<'_>,
    id: &RuntimeId,
    turn_id: Option<&str>,
) -> RuntimeResult<()> {
    let now = now_ms();
    let res = sqlx::query(
        r#"UPDATE runtimes
              SET active_turn_id = ?1,
                  updated_at_ms = ?2
            WHERE id = ?3"#,
    )
    .bind(turn_id)
    .bind(now)
    .bind(id)
    .execute(&mut **tx)
    .await?;
    if res.rows_affected() == 0 {
        return Err(runtime_message(format!("runtime {id} not found")));
    }
    session_set_active_turn_mirror_tx(tx, id, turn_id, now).await?;
    Ok(())
}

/// Tolerant harness phase-mirror / compensation write; deliberately skips the
/// runtime status matrix and emits no event.
pub async fn runtime_set_harness_observation_tx(
    tx: &mut RuntimeTx<'_>,
    id: &RuntimeId,
    status: RunStatus,
    thread_id: Option<&str>,
    active_turn_id: Option<&str>,
) -> RuntimeResult<()> {
    let now = now_ms();
    sqlx::query(
        r#"UPDATE runtimes
              SET status = ?1,
                  thread_id = COALESCE(?2, thread_id),
                  active_turn_id = ?3,
                  updated_at_ms = ?4
            WHERE id = ?5
              AND status IN ('starting', 'running', 'idle', 'turn_pending')"#,
    )
    .bind(run_status_to_db(&status))
    .bind(thread_id)
    .bind(active_turn_id)
    .bind(now)
    .bind(id)
    .execute(&mut **tx)
    .await?;
    session_set_harness_observation_tx(tx, id, status, thread_id, active_turn_id, now).await?;
    Ok(())
}

/// Tolerant harness phase-mirror / compensation write; deliberately skips the
/// runtime status matrix and emits no event.
pub async fn runtime_fail_if_active_tx(
    tx: &mut RuntimeTx<'_>,
    id: &RuntimeId,
) -> RuntimeResult<()> {
    let now = now_ms();
    sqlx::query(
        r#"UPDATE runtimes
              SET status = 'failed',
                  updated_at_ms = ?1,
                  completed_at_ms = ?1
            WHERE id = ?2
              AND status IN ('starting', 'running', 'idle', 'turn_pending')"#,
    )
    .bind(now)
    .bind(id)
    .execute(&mut **tx)
    .await?;
    session_fail_if_active_tx(tx, id, now).await?;
    Ok(())
}

/// Tolerant harness phase-mirror / compensation write; deliberately skips the
/// runtime status matrix and emits no event.
pub async fn runtime_mark_superseded_tx(
    tx: &mut RuntimeTx<'_>,
    id: &RuntimeId,
) -> RuntimeResult<()> {
    let now = now_ms();
    sqlx::query(
        r#"UPDATE runtimes
              SET status = 'superseded',
                  updated_at_ms = ?1,
                  completed_at_ms = COALESCE(completed_at_ms, ?1)
            WHERE id = ?2"#,
    )
    .bind(now)
    .bind(id)
    .execute(&mut **tx)
    .await?;
    session_mark_superseded_tx(tx, id, now).await?;
    Ok(())
}

/// Tolerant harness phase-mirror / compensation write; deliberately skips the
/// runtime status matrix and emits no event.
pub async fn runtime_restore_from_superseded_tx(
    tx: &mut RuntimeTx<'_>,
    id: &RuntimeId,
    status: RunStatus,
) -> RuntimeResult<()> {
    let status_db = run_status_to_db(&status);
    let now = now_ms();
    let res = sqlx::query(
        r#"UPDATE runtimes
              SET status = ?1,
                  updated_at_ms = ?2,
                  completed_at_ms = NULL
            WHERE id = ?3
              AND status = 'superseded'"#,
    )
    .bind(status_db)
    .bind(now)
    .bind(id)
    .execute(&mut **tx)
    .await?;
    if res.rows_affected() > 0 {
        let session = session_restore_from_superseded_tx(tx, id, status, now).await?;
        let runtime = runtime_get_by_id_tx(tx, id)
            .await?
            .ok_or_else(|| runtime_message(format!("runtime {id} missing after restore")))?;
        session_repoint_current_links_tx(tx, &runtime.card_id, &session).await?;
        return Ok(());
    }

    let current: Option<(String,)> = sqlx::query_as("SELECT status FROM runtimes WHERE id = ?1")
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?;
    match current {
        Some((current,)) if current == status_db => {
            let session = session_restore_from_superseded_tx(tx, id, status, now).await?;
            let runtime = runtime_get_by_id_tx(tx, id)
                .await?
                .ok_or_else(|| runtime_message(format!("runtime {id} missing after restore")))?;
            session_repoint_current_links_tx(tx, &runtime.card_id, &session).await
        }
        Some((current,)) => Err(runtime_message(format!(
            "runtime {id} has status {current}; cannot restore old spec harness runtime to {status_db}"
        ))),
        None => Err(runtime_message(format!(
            "runtime {id} missing while restoring old spec harness runtime"
        ))),
    }
}

pub async fn runtime_complete_tx(
    tx: &mut RuntimeTx<'_>,
    id: &RuntimeId,
    terminal_status: RunStatus,
) -> RuntimeResult<()> {
    if !matches!(terminal_status, RunStatus::Failed | RunStatus::Exited) {
        return Err(RuntimeRepoError::IllegalStatusTransition {
            id: id.clone(),
            attempted: terminal_status,
        });
    }

    let current = runtime_current_status_tx(tx, id).await?;
    ensure_runtime_status_transition(id, &current, &terminal_status)?;

    let now = now_ms();
    let res = sqlx::query(
        r#"UPDATE runtimes
              SET status = ?1,
                  updated_at_ms = ?2,
                  completed_at_ms = ?2
            WHERE id = ?3"#,
    )
    .bind(run_status_to_db(&terminal_status))
    .bind(now)
    .bind(id)
    .execute(&mut **tx)
    .await?;
    if res.rows_affected() == 0 {
        return Err(runtime_message(format!("runtime {id} not found")));
    }
    session_complete_mirror_tx(tx, id, terminal_status, now).await?;
    Ok(())
}

/// MUST be called only inside `session_commit_exit`, AFTER the session-side
/// exit CAS in the same tx — it stamps `runtimes.updated_at_ms` from the
/// just-written `worker_sessions` row; standalone use falls back to `now_ms()`
/// and breaks parity.
pub async fn runtime_status_flip_tx(
    tx: &mut RuntimeTx<'_>,
    id: &RuntimeId,
    terminal_status: RunStatus,
) -> RuntimeResult<()> {
    if !matches!(terminal_status, RunStatus::Failed | RunStatus::Exited) {
        return Err(RuntimeRepoError::IllegalStatusTransition {
            id: id.clone(),
            attempted: terminal_status,
        });
    }

    let current = runtime_current_status_tx(tx, id).await?;
    if current != terminal_status {
        ensure_runtime_status_transition(id, &current, &terminal_status)?;
    }

    let lockstep_ms: Option<i64> =
        sqlx::query_scalar("SELECT updated_at_ms FROM worker_sessions WHERE id = ?1")
            .bind(id)
            .fetch_optional(&mut **tx)
            .await?;
    let lockstep_ms = lockstep_ms.unwrap_or_else(now_ms);
    let res = sqlx::query(
        r#"UPDATE runtimes
              SET status = ?1,
                  updated_at_ms = ?2,
                  completed_at_ms = ?2
            WHERE id = ?3"#,
    )
    .bind(run_status_to_db(&terminal_status))
    .bind(lockstep_ms)
    .bind(id)
    .execute(&mut **tx)
    .await?;
    if res.rows_affected() == 0 {
        return Err(runtime_message(format!("runtime {id} not found")));
    }
    Ok(())
}

pub async fn runtime_complete_for_card_tx(
    tx: &mut RuntimeTx<'_>,
    card_id: &str,
    terminal_status: RunStatus,
) -> RuntimeResult<()> {
    let Some(runtime) = runtime_get_active_for_card_tx(tx, card_id).await? else {
        return Ok(());
    };
    runtime_complete_tx(tx, &runtime.id, terminal_status).await
}

pub async fn runtime_get_active_for_terminal_tx(
    tx: &mut RuntimeTx<'_>,
    terminal_id: &str,
) -> RuntimeResult<Option<CardRuntime>> {
    let sql = format!(
        r#"{WS_BACKED_CARD_RUNTIME_SELECT}
           WHERE ws.terminal_run_id = ?1
             AND ws.state IN ('starting','running','idle','turn_pending')
           ORDER BY ws.updated_at_ms DESC, ws.created_at_ms DESC, ws.id DESC
           LIMIT 1"#,
    );
    let row = sqlx::query(&sql)
        .bind(terminal_id)
        .fetch_optional(&mut **tx)
        .await?;
    row.as_ref().map(card_runtime_from_ws_join_row).transpose()
}

pub async fn runtime_complete_for_terminal_tx(
    tx: &mut RuntimeTx<'_>,
    terminal_id: &str,
    terminal_status: RunStatus,
) -> RuntimeResult<()> {
    let Some(runtime) = runtime_get_active_for_terminal_tx(tx, terminal_id).await? else {
        return Ok(());
    };
    runtime_complete_tx(tx, &runtime.id, terminal_status).await
}

pub async fn backfill_worker_sessions_from_runtimes(
    tx: &mut RuntimeTx<'_>,
) -> RuntimeResult<usize> {
    let rows = sqlx::query(
        r#"SELECT r.id, r.card_id, r.kind, r.agent_provider, r.status, r.terminal_run_id,
                  r.thread_id, r.session_id, r.active_turn_id, r.handle_state_json,
                  r.lease_owner, r.lease_until_ms, r.created_at_ms, r.updated_at_ms,
                  r.completed_at_ms, c.wave_id AS wave_id
           FROM runtimes r
           JOIN cards c ON c.id = r.card_id
           WHERE NOT EXISTS (
               SELECT 1 FROM worker_sessions ws WHERE ws.id = r.id
           )
           ORDER BY r.created_at_ms ASC, r.id ASC"#,
    )
    .fetch_all(&mut **tx)
    .await?;

    let mut inserted = 0usize;
    for row in rows {
        let runtime = card_runtime_from_row(&row)?;
        let wave_id = WaveId(row.try_get("wave_id")?);
        let session = worker_session_from_card_runtime(&runtime, wave_id);
        let session = session_insert_tx(tx, session)
            .await
            .map_err(runtime_session_error)?;
        if session.state.is_active_authority() {
            session_repoint_current_links_tx(tx, &runtime.card_id, &session).await?;
        }
        inserted += 1;
    }
    Ok(inserted)
}

pub async fn overlay_upsert_tx(tx: &mut Transaction<'_, Sqlite>, p: NewOverlay) -> Result<Overlay> {
    let now = now_ms();
    let new_id_str = new_id();
    let payload_text = serde_json::to_string(&p.payload)?;
    let row = sqlx::query_as::<_, crate::db::rows::OverlayRow>(
        r#"INSERT INTO overlays
               (id, plugin_id, entity_kind, entity_id, kind, payload, updated_at)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
           ON CONFLICT(plugin_id, entity_kind, entity_id, kind)
             DO UPDATE SET payload = excluded.payload,
                           updated_at = excluded.updated_at
           RETURNING id, plugin_id, entity_kind, entity_id, kind, payload, updated_at"#,
    )
    .bind(&new_id_str)
    .bind(&p.plugin_id)
    .bind(&p.entity_kind)
    .bind(&p.entity_id)
    .bind(&p.kind)
    .bind(&payload_text)
    .bind(now)
    .fetch_one(&mut **tx)
    .await?;
    Ok(Overlay::from(row))
}

pub async fn overlay_delete_tx(
    tx: &mut Transaction<'_, Sqlite>,
    plugin_id: &str,
    entity_kind: &str,
    entity_id: &str,
    kind: &str,
) -> Result<()> {
    let res = sqlx::query(
        r#"DELETE FROM overlays
           WHERE plugin_id = ?1 AND entity_kind = ?2 AND entity_id = ?3 AND kind = ?4"#,
    )
    .bind(plugin_id)
    .bind(entity_kind)
    .bind(entity_id)
    .bind(kind)
    .execute(&mut **tx)
    .await?;
    if res.rows_affected() == 0 {
        return Err(CalmError::NotFound("overlay"));
    }
    Ok(())
}

/// Drop every overlay row addressed at the given `(entity_kind, entity_id)`,
/// across all `plugin_id`s and all `kind`s. Used by the card / wave / cove
/// delete paths to keep the table from growing orphans; the `overlays`
/// schema has no FK because SQLite can't express polymorphic ones.
pub async fn overlay_delete_by_entity_tx(
    tx: &mut Transaction<'_, Sqlite>,
    entity_kind: &str,
    entity_id: &str,
) -> Result<u64> {
    let res = sqlx::query("DELETE FROM overlays WHERE entity_kind = ?1 AND entity_id = ?2")
        .bind(entity_kind)
        .bind(entity_id)
        .execute(&mut **tx)
        .await?;
    Ok(res.rows_affected())
}

/// Drop every card-scoped overlay for cards still belonging to the given
/// wave, in the caller's transaction. Race-safe replacement for "read
/// cards_by_wave, loop, sweep" — the IN subquery sees the same DB state
/// the subsequent `wave_delete_tx` cascade will see, so a card+overlay
/// created between an outside-tx snapshot and the txn commit is still
/// caught.
pub async fn overlay_delete_card_overlays_by_wave_tx(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &str,
) -> Result<u64> {
    let res = sqlx::query(
        r#"DELETE FROM overlays
           WHERE entity_kind = 'card'
             AND entity_id IN (SELECT id FROM cards WHERE wave_id = ?1)"#,
    )
    .bind(wave_id)
    .execute(&mut **tx)
    .await?;
    Ok(res.rows_affected())
}

/// Drop every card-scoped and wave-scoped (`'wave'` + `'view'`) overlay
/// under the cove, in the caller's transaction. Same race-safe shape as
/// `overlay_delete_card_overlays_by_wave_tx`. Caller still needs to
/// sweep `('cove', cove_id)` separately — plugins may address overlays
/// at the cove kind.
pub async fn overlay_delete_subtree_by_cove_tx(
    tx: &mut Transaction<'_, Sqlite>,
    cove_id: &str,
) -> Result<u64> {
    // Three statements, accumulated count. Could be one nested query but
    // splitting keeps each delete legible and individually optimizable.
    let cards = sqlx::query(
        r#"DELETE FROM overlays
           WHERE entity_kind = 'card'
             AND entity_id IN (
               SELECT c.id FROM cards c
               JOIN waves w ON w.id = c.wave_id
               WHERE w.cove_id = ?1
             )"#,
    )
    .bind(cove_id)
    .execute(&mut **tx)
    .await?
    .rows_affected();
    let waves = sqlx::query(
        r#"DELETE FROM overlays
           WHERE entity_kind IN ('wave', 'view')
             AND entity_id IN (SELECT id FROM waves WHERE cove_id = ?1)"#,
    )
    .bind(cove_id)
    .execute(&mut **tx)
    .await?
    .rows_affected();
    Ok(cards + waves)
}

// ---------------------------------------------------------------------------
// Sub-trait impls — thin pool-wrapping wrappers around the `_tx` helpers,
// plus the read-side methods that don't need transaction composition.
//
// `Repo` (and `RouteRepo`) are picked up via the blanket impls in `db/mod`
// once all four sub-traits are implemented.
// ---------------------------------------------------------------------------

#[async_trait]
impl RepoRead for SqlxRepo {
    // ---------------------------------------------------------------- coves
    async fn coves_list(&self) -> Result<Vec<Cove>> {
        let rows = sqlx::query_as::<_, crate::db::rows::CoveRow>(
            r#"SELECT id, name, color, sort, kind, created_at, updated_at
               FROM coves ORDER BY sort ASC"#,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Cove::from).collect())
    }

    async fn coves_list_user_visible(&self) -> Result<Vec<Cove>> {
        // Issue #175 — default surface for `GET /api/coves`. Filters out
        // the singleton system cove that hosts the default Today
        // terminal's wave + card. Pre-#175 callers that want every row
        // (debug surfaces, integration tests asserting on the system
        // cove's existence) use `coves_list` directly.
        let rows = sqlx::query_as::<_, crate::db::rows::CoveRow>(
            r#"SELECT id, name, color, sort, kind, created_at, updated_at
               FROM coves WHERE kind = 'user' ORDER BY sort ASC"#,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Cove::from).collect())
    }

    async fn cove_get(&self, id: &str) -> Result<Option<Cove>> {
        let row = sqlx::query_as::<_, crate::db::rows::CoveRow>(
            r#"SELECT id, name, color, sort, kind, created_at, updated_at
               FROM coves WHERE id = ?1"#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(Cove::from))
    }

    async fn cove_get_system(&self) -> Result<Option<Cove>> {
        // Issue #175 — return the singleton system cove if it exists,
        // `None` before the first call to the `POST /api/coves/system`
        // upsert endpoint. Backed by the partial unique index on
        // `coves(kind) WHERE kind = 'system'` from migration 0009 —
        // there is at most one such row.
        let row = sqlx::query_as::<_, crate::db::rows::CoveRow>(
            r#"SELECT id, name, color, sort, kind, created_at, updated_at
               FROM coves WHERE kind = 'system' LIMIT 1"#,
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(Cove::from))
    }

    // -------------------------------------------------------- cove_folders
    async fn cove_folders_by_cove(&self, cove_id: &str) -> Result<Vec<CoveFolder>> {
        let rows = sqlx::query_as::<_, crate::db::rows::CoveFolderRow>(
            r#"SELECT id, cove_id, path, created_at
               FROM cove_folders WHERE cove_id = ?1 ORDER BY path ASC"#,
        )
        .bind(cove_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(CoveFolder::from).collect())
    }

    async fn cove_folders_list_all(&self) -> Result<Vec<CoveFolder>> {
        let rows = sqlx::query_as::<_, crate::db::rows::CoveFolderRow>(
            r#"SELECT id, cove_id, path, created_at
               FROM cove_folders ORDER BY path ASC"#,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(CoveFolder::from).collect())
    }

    async fn cove_folder_get(&self, id: i64) -> Result<Option<CoveFolder>> {
        let row = sqlx::query_as::<_, crate::db::rows::CoveFolderRow>(
            r#"SELECT id, cove_id, path, created_at
               FROM cove_folders WHERE id = ?1"#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(CoveFolder::from))
    }

    // ---------------------------------------------------------------- waves
    async fn waves_by_cove(&self, cove_id: &str) -> Result<Vec<Wave>> {
        let rows = sqlx::query_as::<_, crate::db::rows::WaveRow>(
            r#"SELECT id, cove_id, title, sort, archived_at, pinned_at, lifecycle, cwd, terminal_at, created_at, updated_at
               FROM waves WHERE cove_id = ?1 ORDER BY sort ASC"#,
        )
        .bind(cove_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Wave::from).collect())
    }

    async fn wave_get(&self, id: &str) -> Result<Option<Wave>> {
        let row = sqlx::query_as::<_, crate::db::rows::WaveRow>(
            r#"SELECT id, cove_id, title, sort, archived_at, pinned_at, lifecycle, cwd, terminal_at, created_at, updated_at
               FROM waves WHERE id = ?1"#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(Wave::from))
    }

    async fn waves_window(
        &self,
        cove_id: Option<&str>,
        since: Option<i64>,
        until: Option<i64>,
    ) -> Result<Vec<Wave>> {
        // Build the WHERE clause dynamically because sqlx doesn't have
        // good "optional bind" ergonomics — every binding has to be
        // either materialized or excluded from the query string. The
        // three predicates compose in any combination:
        //   * `cove_id`     : `cove_id = ?`
        //   * `until`       : `created_at <= ?`
        //   * `since`       : `(terminal_at IS NULL OR terminal_at >= ?)`
        let mut sql = String::from(
            "SELECT id, cove_id, title, sort, archived_at, pinned_at, lifecycle, cwd, terminal_at, \
             created_at, updated_at FROM waves",
        );
        let mut where_clauses: Vec<&str> = Vec::new();
        if cove_id.is_some() {
            where_clauses.push("cove_id = ?");
        }
        if until.is_some() {
            where_clauses.push("created_at <= ?");
        }
        if since.is_some() {
            where_clauses.push("(terminal_at IS NULL OR terminal_at >= ?)");
        }
        if !where_clauses.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&where_clauses.join(" AND "));
        }
        sql.push_str(" ORDER BY created_at ASC, id ASC");

        let mut q = sqlx::query_as::<_, crate::db::rows::WaveRow>(&sql);
        if let Some(c) = cove_id {
            q = q.bind(c);
        }
        if let Some(u) = until {
            q = q.bind(u);
        }
        if let Some(s) = since {
            q = q.bind(s);
        }
        Ok(q.fetch_all(&self.pool)
            .await?
            .into_iter()
            .map(Wave::from)
            .collect())
    }

    async fn wave_detail(&self, id: &str) -> Result<Option<WaveDetail>> {
        let mut tx = self.pool.begin().await?;
        let wave = sqlx::query_as::<_, crate::db::rows::WaveRow>(
            r#"SELECT id, cove_id, title, sort, archived_at, pinned_at, lifecycle, cwd, terminal_at, created_at, updated_at
               FROM waves WHERE id = ?1"#,
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(wave) = wave else {
            return Ok(None);
        };

        let cards = sqlx::query_as::<_, crate::db::rows::CardRow>(
            r#"SELECT id, wave_id, kind, sort, payload, deletable, created_at, updated_at
               FROM cards WHERE wave_id = ?1 ORDER BY sort ASC"#,
        )
        .bind(id)
        .fetch_all(&mut *tx)
        .await?;

        // Overlays scoped to this wave or any of its cards. One query: a
        // wave-scoped row plus an IN-list on card ids built at the SQL level
        // using a `cards` subquery so we avoid a parameter explosion.
        let overlays = sqlx::query_as::<_, crate::db::rows::OverlayRow>(
            r#"SELECT id, plugin_id, entity_kind, entity_id, kind, payload, updated_at
               FROM overlays
               WHERE (entity_kind = 'wave' AND entity_id = ?1)
                  OR (entity_kind = 'card'
                      AND entity_id IN (SELECT id FROM cards WHERE wave_id = ?1))"#,
        )
        .bind(id)
        .fetch_all(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(Some(WaveDetail {
            wave: Wave::from(wave),
            cards: cards.into_iter().map(Card::from).collect(),
            overlays: overlays.into_iter().map(Overlay::from).collect(),
        }))
    }

    // ---------------------------------------------------------------- tasks
    async fn tasks_by_wave(&self, wave_id: &str) -> Result<Vec<Task>> {
        let sql = format!(
            "SELECT {TASK_COLUMNS} FROM tasks WHERE wave_id = ?1 \
             ORDER BY priority DESC, created_at_ms ASC, key ASC"
        );
        let rows = sqlx::query_as::<_, Task>(&sql)
            .bind(wave_id)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows)
    }

    async fn task_get(&self, id: &str) -> Result<Option<Task>> {
        let sql = format!("SELECT {TASK_COLUMNS} FROM tasks WHERE id = ?1");
        let row = sqlx::query_as::<_, Task>(&sql)
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row)
    }

    async fn tasks_nonterminal(&self) -> Result<Vec<Task>> {
        let sql = format!(
            "SELECT {TASK_COLUMNS} FROM tasks \
             WHERE status IN ('pending', 'dispatched', 'running', 'verifying') \
             ORDER BY wave_id ASC, priority DESC, created_at_ms ASC, key ASC"
        );
        let rows = sqlx::query_as::<_, Task>(&sql)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows)
    }

    async fn operation_idempotency_key_by_id(&self, op_id: &str) -> Result<Option<String>> {
        let row: Option<Option<String>> =
            sqlx::query_scalar("SELECT idempotency_key FROM operations WHERE id = ?1")
                .bind(op_id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.flatten())
    }

    // ---------------------------------------------------------------- cards
    async fn cards_by_wave(&self, wave_id: &str) -> Result<Vec<Card>> {
        // Keep this ORDER BY aligned with wave_vcs::cards_for_wave_tx; tests pin
        // the sort ASC, id ASC tie-break for duplicate worker run keys.
        let rows = sqlx::query_as::<_, crate::db::rows::CardRow>(
            r#"SELECT id, wave_id, kind, sort, payload, deletable, created_at, updated_at
               FROM cards WHERE wave_id = ?1 ORDER BY sort ASC, id ASC"#,
        )
        .bind(wave_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Card::from).collect())
    }

    async fn card_get(&self, id: &str) -> Result<Option<Card>> {
        let row = sqlx::query_as::<_, crate::db::rows::CardRow>(
            r#"SELECT id, wave_id, kind, sort, payload, deletable, created_at, updated_at
               FROM cards WHERE id = ?1"#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(Card::from))
    }

    async fn card_role_get(&self, id: &str) -> Result<Option<CardRole>> {
        // #679 PR1 — `CardRole` lost its `sqlx::Type` derive when it moved
        // to calm-types; decode TEXT and parse via `TryFrom<String>`.
        let row: Option<(String,)> = sqlx::query_as("SELECT role FROM cards WHERE id = ?1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.map(|(role,)| {
            CardRole::try_from(role)
                .map_err(|e| CalmError::Internal(format!("cards.role decode: {e}")))
        })
        .transpose()
    }

    async fn harness_item_list_by_card(
        &self,
        card_id: &str,
        after_id: i64,
        limit: i64,
        descending: bool,
    ) -> Result<Vec<HarnessItem>> {
        let (sql, cursor) = if descending {
            (
                r#"SELECT id, runtime_id, card_id, wave_id, thread_id, turn_id,
                          item_uuid, item_type, method, params, created_at_ms
                   FROM harness_items
                   WHERE card_id = ?1 AND id < ?2
                   ORDER BY id DESC
                   LIMIT ?3"#,
                if after_id == 0 { i64::MAX } else { after_id },
            )
        } else {
            (
                r#"SELECT id, runtime_id, card_id, wave_id, thread_id, turn_id,
                          item_uuid, item_type, method, params, created_at_ms
                   FROM harness_items
                   WHERE card_id = ?1 AND id > ?2
                   ORDER BY id ASC
                   LIMIT ?3"#,
                after_id,
            )
        };
        let mut rows = sqlx::query_as::<_, crate::db::rows::HarnessItemRow>(sql)
            .bind(card_id)
            .bind(cursor)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;
        if descending {
            rows.reverse();
        }
        Ok(rows.into_iter().map(HarnessItem::from).collect())
    }

    async fn worker_flow_item_list_by_card(
        &self,
        card_id: &str,
        after_id: i64,
        limit: i64,
        descending: bool,
    ) -> Result<Vec<crate::db::rows::WorkerFlowItemRow>> {
        // Clamp the page size to a defensible ceiling so a caller passing a
        // huge (or non-positive) limit cannot scan the whole table.
        let limit = limit.clamp(1, 500);
        let (sql, cursor) = if descending {
            (
                r#"SELECT id, card_id, runtime_id, wave_id, worker_session_id,
                          kind, payload, created_at_ms
                   FROM worker_flow_items
                   WHERE card_id = ?1 AND id < ?2
                   ORDER BY id DESC
                   LIMIT ?3"#,
                if after_id == 0 { i64::MAX } else { after_id },
            )
        } else {
            (
                r#"SELECT id, card_id, runtime_id, wave_id, worker_session_id,
                          kind, payload, created_at_ms
                   FROM worker_flow_items
                   WHERE card_id = ?1 AND id > ?2
                   ORDER BY id ASC
                   LIMIT ?3"#,
                after_id,
            )
        };
        let mut rows = sqlx::query_as::<_, crate::db::rows::WorkerFlowItemRow>(sql)
            .bind(card_id)
            .bind(cursor)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;
        if descending {
            rows.reverse();
        }
        Ok(rows)
    }

    async fn worker_flow_cursor_get(
        &self,
        card_id: &str,
        source_kind: &str,
    ) -> Result<Option<crate::db::rows::WorkerFlowCursor>> {
        let row = sqlx::query_as::<_, crate::db::rows::WorkerFlowCursor>(
            r#"SELECT card_id, source_kind, source_path, record_index,
                      byte_offset, last_source_uuid, last_line_hash, updated_at_ms
               FROM worker_flow_cursors
               WHERE card_id = ?1 AND source_kind = ?2"#,
        )
        .bind(card_id)
        .bind(source_kind)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    async fn shared_daemon_runtime_get(&self) -> Result<SharedCodexDaemonRecord> {
        let row = sqlx::query_as::<
            _,
            (
                String,
                Option<i32>,
                Option<i32>,
                Option<String>,
                Option<String>,
                Option<i64>,
                Option<String>,
                Option<i64>,
                i64,
                i64,
                Option<String>,
                Option<String>,
            ),
        >(
            r#"SELECT state, pid, pgid, sock_path, codex_home_path, process_start_time,
                      boot_id, started_at, updated_at, restart_count, last_error,
                      daemon_env_signature
               FROM shared_codex_daemon
               WHERE id = 1"#,
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(SharedCodexDaemonRecord {
            state: row.0,
            pid: row.1,
            pgid: row.2,
            sock_path: row.3,
            codex_home_path: row.4,
            process_start_time: row.5.and_then(|v| u64::try_from(v).ok()),
            boot_id: row.6,
            started_at: row.7,
            updated_at: row.8,
            restart_count: row.9,
            last_error: row.10,
            daemon_env_signature: row.11,
        })
    }

    // -------------------------------------------------------------- overlays
    async fn overlays_for(&self, entity_kind: &str, entity_id: &str) -> Result<Vec<Overlay>> {
        let rows = sqlx::query_as::<_, crate::db::rows::OverlayRow>(
            r#"SELECT id, plugin_id, entity_kind, entity_id, kind, payload, updated_at
               FROM overlays WHERE entity_kind = ?1 AND entity_id = ?2"#,
        )
        .bind(entity_kind)
        .bind(entity_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Overlay::from).collect())
    }

    async fn overlays_by_kind(&self, entity_kind: &str) -> Result<Vec<Overlay>> {
        let rows = sqlx::query_as::<_, crate::db::rows::OverlayRow>(
            r#"SELECT id, plugin_id, entity_kind, entity_id, kind, payload, updated_at
               FROM overlays WHERE entity_kind = ?1"#,
        )
        .bind(entity_kind)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Overlay::from).collect())
    }

    // ------------------------------------------------------------- terminals
    async fn terminal_get(&self, id: &str) -> Result<Option<Terminal>> {
        let row = sqlx::query_as::<_, Terminal>(
            r#"SELECT id, card_id, program, cwd, env, pid,
                      theme_fg, theme_bg, exit_code, signal_killed, created_at
               FROM terminals WHERE id = ?1"#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    async fn terminal_get_by_card(&self, card_id: &str) -> Result<Option<Terminal>> {
        let row = sqlx::query_as::<_, Terminal>(
            r#"SELECT id, card_id, program, cwd, env, pid,
                      theme_fg, theme_bg, exit_code, signal_killed, created_at
               FROM terminals WHERE card_id = ?1"#,
        )
        .bind(card_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    async fn terminals_orphaned(&self, grace_seconds: i64) -> Result<Vec<Terminal>> {
        // Orphan: this terminal's card has no active runtime, AND the row
        // was created more than `grace_seconds` ago.
        //
        // `created_at` is unix ms; the grace bound is `now_ms - grace_seconds * 1000`.
        let cutoff = now_ms() - grace_seconds.saturating_mul(1000);
        let rows = sqlx::query_as::<_, Terminal>(
            r#"SELECT t.id, t.card_id, t.program, t.cwd, t.env,
                      t.pid,
                      t.theme_fg, t.theme_bg,
                      t.exit_code, t.signal_killed,
                      t.created_at
               FROM terminals t
               WHERE NOT EXISTS (
                   SELECT 1 FROM runtimes r
                   WHERE r.card_id = t.card_id
                     AND r.status IN ('starting', 'running', 'idle', 'turn_pending')
               )
               AND t.created_at < ?1"#,
        )
        .bind(cutoff)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    async fn terminals_running(&self) -> Result<Vec<Terminal>> {
        let rows = sqlx::query_as::<_, Terminal>(
            r#"SELECT id, card_id, program, cwd, env,
                      pid,
                      theme_fg, theme_bg,
                      exit_code, signal_killed,
                      created_at
               FROM terminals
               WHERE exit_code IS NULL AND signal_killed = 0"#,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    async fn shared_spec_cards_for_initial_prompt_takeover(
        &self,
    ) -> Result<Vec<(String, String, String, i64)>> {
        let (provider, _mode, contract) = derive_session_identity(&RuntimeKind::SharedSpec);
        // Join `terminals` and require a LIVE row so a card whose TUI was
        // already reaped (reconcile_supervisor_on_boot marked it exited,
        // or a SIGKILL set signal_killed=1) is NOT re-registered into the
        // pending FIFO. A dead TUI can never emit thread/started, so
        // re-registering would leave the entry stranded until TTL expiry
        // — and worse, the entry would absorb a later thread/started
        // attribution intended for a different empty card (until
        // on_thread_started's stale-front-drop catches it). This was the
        // R7 P2 #1 followup; CI reproduced it because the terminal gets
        // reaped before the next boot's takeover query runs.
        let rows: Vec<(String, String, String, i64)> = sqlx::query_as(
            r#"SELECT c.id,
                      c.wave_id,
                      ws.terminal_run_id,
                      0
               FROM cards c
               JOIN waves w ON w.id = c.wave_id
               JOIN worker_sessions ws ON ws.id = c.session_id
                   AND ws.provider = ?1
                   AND ws.contract = ?2
                   AND ws.thread_id IS NULL
                   AND ws.state IN ('starting','running','idle','turn_pending')
                   AND EXISTS (SELECT 1 FROM runtimes r WHERE r.id = ws.id)
               JOIN terminals t ON t.id = ws.terminal_run_id
               WHERE c.role = 'spec'
                 AND t.exit_code IS NULL
                 AND COALESCE(t.signal_killed, 0) = 0
                 AND NOT EXISTS (
                       SELECT 1
                         FROM worker_sessions hws
                         JOIN cards hc ON hc.session_id = hws.id
                        WHERE hc.id = c.id
                          AND hws.provider = ?3
                          AND hws.contract = ?4
                          AND hws.state IN ('starting','running','idle','turn_pending')
                          AND hws.handle_state_json IS NOT NULL
                          AND json_extract(hws.handle_state_json, '$.mode') = 'harness'
                          AND EXISTS (SELECT 1 FROM runtimes r WHERE r.id = hws.id)
                 )
                 AND w.lifecycle NOT IN ('done', 'canceled', 'failed')
               ORDER BY c.created_at ASC, c.id ASC"#,
        )
        .bind(provider.as_db_str())
        .bind(contract.as_db_str())
        .bind(provider.as_db_str())
        .bind(contract.as_db_str())
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    // --------------------------------------------------------------- plugins
    async fn plugins_list(&self) -> Result<Vec<Plugin>> {
        self.plugins_list_all().await
    }

    async fn plugins_list_all(&self) -> Result<Vec<Plugin>> {
        let rows = sqlx::query_as::<_, Plugin>(
            r#"SELECT id, version, install_path, manifest, enabled, user_config,
                      installed_at, updated_at
               FROM plugins
               ORDER BY id ASC"#,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    async fn plugin_get_by_id(&self, id: &str) -> Result<Option<Plugin>> {
        let row = sqlx::query_as::<_, Plugin>(
            r#"SELECT id, version, install_path, manifest, enabled, user_config,
                      installed_at, updated_at
               FROM plugins WHERE id = ?1"#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    async fn plugin_token_get(&self, plugin_id: &str) -> Result<Option<(String, i64)>> {
        let row: Option<(String, i64)> = sqlx::query_as(
            r#"SELECT hashed_token, expires_at FROM plugin_tokens WHERE plugin_id = ?1"#,
        )
        .bind(plugin_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    async fn plugin_kv_get(&self, plugin_id: &str, key: &str) -> Result<Option<serde_json::Value>> {
        let row: Option<(String,)> =
            sqlx::query_as(r#"SELECT value FROM plugin_kv WHERE plugin_id = ?1 AND key = ?2"#)
                .bind(plugin_id)
                .bind(key)
                .fetch_optional(&self.pool)
                .await?;
        match row {
            Some((text,)) => Ok(Some(serde_json::from_str(&text)?)),
            None => Ok(None),
        }
    }

    async fn plugin_kv_list(
        &self,
        plugin_id: &str,
        prefix: &str,
    ) -> Result<Vec<(String, serde_json::Value)>> {
        let mut escaped = String::with_capacity(prefix.len() + 2);
        for ch in prefix.chars() {
            if ch == '%' || ch == '_' || ch == '\\' {
                escaped.push('\\');
            }
            escaped.push(ch);
        }
        escaped.push('%');
        let rows: Vec<(String, String)> = sqlx::query_as(
            r#"SELECT key, value FROM plugin_kv
               WHERE plugin_id = ?1 AND key LIKE ?2 ESCAPE '\'
               ORDER BY key ASC"#,
        )
        .bind(plugin_id)
        .bind(&escaped)
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for (k, v) in rows {
            out.push((k, serde_json::from_str(&v)?));
        }
        Ok(out)
    }

    // -------------------------------------------------------------- settings
    async fn settings_get_all(&self) -> Result<Vec<(String, String)>> {
        let rows: Vec<(String, String)> =
            sqlx::query_as(r#"SELECT key, value FROM settings ORDER BY key ASC"#)
                .fetch_all(&self.pool)
                .await?;
        Ok(rows)
    }

    // ------------------------------------------------------------ role cache
    async fn seed_card_role_cache(&self, cache: &CardRoleCache) -> Result<()> {
        cache.seed_from_db(&self.pool).await
    }

    // ------------------------------------------------------- wave-cove cache
    async fn seed_wave_cove_cache(&self, cache: &WaveCoveCache) -> Result<()> {
        cache.seed_from_db(&self.pool).await
    }

    // ----------------------------------------------------------- mcp tokens
    async fn card_mcp_token_lookup_by_hash(
        &self,
        hashed_token: &str,
    ) -> Result<Option<(String, String)>> {
        // PR7a.1 (#136 followup) — return `(card_id, hashed_token)` so
        // the handshake can run a constant-time compare on the stored
        // hash. The `WHERE` clause already filtered on the hash, so the
        // returned column is the same value the caller passed in; we
        // still echo it back rather than hand off the input — that way
        // a future migration that changes column storage (e.g. hex →
        // bytes) doesn't break the contract silently.
        let row: Option<(String, String)> = sqlx::query_as(
            r#"SELECT card_id, hashed_token FROM card_mcp_tokens WHERE hashed_token = ?1"#,
        )
        .bind(hashed_token)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    async fn card_identity_get_by_session(
        &self,
        session_id: &str,
    ) -> Result<Option<SessionCardIdentity>> {
        let rows = sqlx::query(
            r#"SELECT c.id, c.role, c.wave_id, w.cove_id
               FROM cards c
               JOIN waves w ON w.id = c.wave_id
              WHERE c.session_id = ?1
              ORDER BY c.updated_at DESC, c.created_at DESC, c.id DESC
              LIMIT 2"#,
        )
        .bind(session_id)
        .fetch_all(&self.pool)
        .await?;
        match rows.as_slice() {
            [] => Ok(None),
            [row] => {
                let role = CardRole::try_from(row.try_get::<String, _>("role")?)
                    .map_err(|e| CalmError::Internal(format!("cards.role decode: {e}")))?;
                Ok(Some(SessionCardIdentity {
                    card_id: CardId(row.try_get("id")?),
                    role,
                    wave_id: WaveId(row.try_get("wave_id")?),
                    cove_id: CoveId(row.try_get("cove_id")?),
                }))
            }
            _ => Err(CalmError::Internal(format!(
                "multiple cards linked to worker session {session_id}"
            ))),
        }
    }

    async fn session_get_by_active_token_hash(
        &self,
        hashed_token: &str,
    ) -> Result<Option<WorkerSession>> {
        session_get_by_active_token_hash(&self.pool, hashed_token).await
    }

    async fn session_get_by_id(&self, id: &WorkerSessionId) -> Result<Option<WorkerSession>> {
        session_get_by_id(&self.pool, id).await
    }

    async fn card_mcp_token_exists_for_card(&self, card_id: &str) -> Result<bool> {
        let row: Option<(i64,)> =
            sqlx::query_as(r#"SELECT 1 FROM card_mcp_tokens WHERE card_id = ?1 LIMIT 1"#)
                .bind(card_id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.is_some())
    }
}

#[async_trait]
impl RuntimeRepo for SqlxRepo {
    async fn runtime_get_active_by_thread(
        &self,
        provider: AgentProvider,
        thread_id: &str,
    ) -> RuntimeResult<Option<CardRuntime>> {
        runtime_get_active_by_thread_from_pool(&self.pool, provider, thread_id).await
    }

    async fn runtime_get_active_by_session(
        &self,
        provider: AgentProvider,
        session_id: &str,
    ) -> RuntimeResult<Option<CardRuntime>> {
        runtime_get_active_by_session_from_pool(&self.pool, provider, session_id).await
    }

    async fn runtime_get_active_for_card(
        &self,
        card_id: &crate::runtime_repo::CardId,
    ) -> RuntimeResult<Option<CardRuntime>> {
        runtime_get_active_for_card_from_pool(&self.pool, card_id).await
    }

    async fn runtime_get_projectable_for_card(
        &self,
        card_id: &crate::runtime_repo::CardId,
    ) -> RuntimeResult<Option<CardRuntime>> {
        runtime_get_projectable_for_card_from_pool(&self.pool, card_id).await
    }

    async fn runtime_get_projectable_for_cards(
        &self,
        card_ids: &[crate::runtime_repo::CardId],
    ) -> RuntimeResult<HashMap<crate::runtime_repo::CardId, CardRuntime>> {
        runtime_get_projectable_for_cards_from_pool(&self.pool, card_ids).await
    }

    async fn runtime_active_shared_thread_attribution(
        &self,
    ) -> RuntimeResult<Vec<(String, String)>> {
        runtime_active_shared_thread_attribution_from_pool(&self.pool).await
    }

    async fn runtimes_active_for_kind(&self, kind: RuntimeKind) -> RuntimeResult<Vec<CardRuntime>> {
        runtimes_active_for_kind_from_pool(&self.pool, kind).await
    }

    async fn runtime_get_by_id(&self, id: &RuntimeId) -> RuntimeResult<Option<CardRuntime>> {
        runtime_get_by_id_from_pool(&self.pool, id).await
    }

    async fn runtime_start_tx(
        &self,
        tx: &mut RuntimeTx<'_>,
        init: RuntimeInit,
    ) -> RuntimeResult<CardRuntime> {
        runtime_start_tx(tx, init).await
    }

    async fn runtime_supersede_tx(
        &self,
        tx: &mut RuntimeTx<'_>,
        id: &RuntimeId,
        new_init: RuntimeInit,
    ) -> RuntimeResult<CardRuntime> {
        runtime_supersede_tx(tx, id, new_init).await
    }

    async fn runtime_set_status_tx(
        &self,
        tx: &mut RuntimeTx<'_>,
        id: &RuntimeId,
        status: RunStatus,
    ) -> RuntimeResult<()> {
        runtime_set_status_tx(tx, id, status).await
    }

    async fn runtime_set_status_for_card(
        &self,
        card_id: &str,
        status: RunStatus,
    ) -> RuntimeResult<()> {
        let mut tx = self.pool.begin().await?;
        runtime_set_status_for_card_tx(&mut tx, card_id, status).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn runtime_set_status_for_card_tx(
        &self,
        tx: &mut RuntimeTx<'_>,
        card_id: &str,
        status: RunStatus,
    ) -> RuntimeResult<()> {
        runtime_set_status_for_card_tx(tx, card_id, status).await
    }

    async fn runtime_bind_attribution_tx(
        &self,
        tx: &mut RuntimeTx<'_>,
        id: &RuntimeId,
        attr: ThreadAttribution,
    ) -> RuntimeResult<()> {
        runtime_bind_attribution_tx(tx, id, attr).await
    }

    async fn runtime_clear_terminal_run_id_tx(
        &self,
        tx: &mut RuntimeTx<'_>,
        id: &RuntimeId,
    ) -> RuntimeResult<()> {
        runtime_clear_terminal_run_id_tx(tx, id).await
    }

    async fn runtime_set_handle_state_tx(
        &self,
        tx: &mut RuntimeTx<'_>,
        id: &RuntimeId,
        state: Option<serde_json::Value>,
    ) -> RuntimeResult<()> {
        runtime_set_handle_state_tx(tx, id, state).await
    }

    async fn runtime_set_active_turn_tx(
        &self,
        tx: &mut RuntimeTx<'_>,
        id: &RuntimeId,
        turn_id: Option<&str>,
    ) -> RuntimeResult<()> {
        runtime_set_active_turn_tx(tx, id, turn_id).await
    }

    async fn runtime_complete_tx(
        &self,
        tx: &mut RuntimeTx<'_>,
        id: &RuntimeId,
        terminal_status: RunStatus,
    ) -> RuntimeResult<()> {
        runtime_complete_tx(tx, id, terminal_status).await
    }

    async fn runtime_complete_for_card(
        &self,
        card_id: &str,
        terminal_status: RunStatus,
    ) -> RuntimeResult<()> {
        let mut tx = self.pool.begin().await?;
        runtime_complete_for_card_tx(&mut tx, card_id, terminal_status).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn runtime_complete_for_card_tx(
        &self,
        tx: &mut RuntimeTx<'_>,
        card_id: &str,
        terminal_status: RunStatus,
    ) -> RuntimeResult<()> {
        runtime_complete_for_card_tx(tx, card_id, terminal_status).await
    }

    async fn runtime_complete_for_terminal(
        &self,
        terminal_id: &str,
        terminal_status: RunStatus,
    ) -> RuntimeResult<()> {
        let mut tx = self.pool.begin().await?;
        runtime_complete_for_terminal_tx(&mut tx, terminal_id, terminal_status).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn runtime_complete_for_terminal_tx(
        &self,
        tx: &mut RuntimeTx<'_>,
        terminal_id: &str,
        terminal_status: RunStatus,
    ) -> RuntimeResult<()> {
        runtime_complete_for_terminal_tx(tx, terminal_id, terminal_status).await
    }

    /// Returns runtimes with an expired lease (lease_owner set,
    /// lease_until_ms in the past). Non-leased runtimes have no orphan signal
    /// without a heartbeat; they are out of scope for now.
    async fn runtimes_recover_orphans_on_boot(&self) -> RuntimeResult<Vec<CardRuntime>> {
        let now = now_ms();
        let rows = sqlx::query(
            r#"SELECT id, card_id, kind, agent_provider, status, terminal_run_id,
                      thread_id, session_id, active_turn_id, handle_state_json,
                      lease_owner, lease_until_ms, created_at_ms, updated_at_ms,
                      completed_at_ms
               FROM runtimes
               WHERE status IN ('starting', 'running', 'idle', 'turn_pending')
                 AND lease_owner IS NOT NULL
                 AND lease_until_ms IS NOT NULL
                 AND lease_until_ms < ?1
               ORDER BY updated_at_ms ASC"#,
        )
        .bind(now)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(card_runtime_from_row).collect()
    }

    async fn backfill_worker_sessions_from_runtimes(&self) -> RuntimeResult<usize> {
        let mut tx = self.pool.begin().await?;
        let inserted = backfill_worker_sessions_from_runtimes(&mut tx).await?;
        tx.commit().await?;
        Ok(inserted)
    }

    async fn runtimes_recover_harnesses_on_boot(&self) -> RuntimeResult<Vec<CardRuntime>> {
        let (provider, _mode, contract) = derive_session_identity(&RuntimeKind::SharedSpec);
        let sql = format!(
            r#"{WS_BACKED_CARD_RUNTIME_SELECT}
               JOIN waves w ON w.id = c.wave_id
               WHERE ws.provider = ?1
                 AND ws.contract = ?2
                 AND ws.state IN ('starting','running','idle','turn_pending')
                 AND ws.handle_state_json IS NOT NULL
                 AND json_extract(ws.handle_state_json, '$.mode') = 'harness'
                 -- Keep harness boot recovery aligned with the legacy
                 -- takeover filters above: terminal waves must stay inert.
                 AND w.lifecycle NOT IN ('done', 'canceled', 'failed')
                 AND EXISTS (SELECT 1 FROM runtimes r WHERE r.id = ws.id)
               ORDER BY ws.created_at_ms ASC, c.id ASC"#
        );
        let rows = sqlx::query(&sql)
            .bind(provider.as_db_str())
            .bind(contract.as_db_str())
            .fetch_all(&self.pool)
            .await?;
        rows.iter()
            .map(card_runtime_from_ws_join_row)
            .collect::<RuntimeResult<Vec<_>>>()
    }
}

fn is_session_conflict(err: &CalmError) -> bool {
    matches!(
        err,
        CalmError::Core(calm_types::error::CoreError::Conflict(_))
    )
}

fn runtime_status_for_exit_session_state(
    id: &WorkerSessionId,
    to: WorkerSessionState,
) -> Result<RunStatus> {
    match to {
        WorkerSessionState::Exited => Ok(RunStatus::Exited),
        WorkerSessionState::Failed => Ok(RunStatus::Failed),
        _ => Err(CalmError::BadRequest(format!(
            "session exit commit {id} requires exited or failed target, got {}",
            to.as_db_str()
        ))),
    }
}

#[async_trait]
impl SessionRepo for SqlxRepo {
    async fn session_insert_tx(
        &self,
        tx: &mut SessionTx<'_>,
        session: WorkerSession,
    ) -> Result<WorkerSession> {
        session_insert_tx(tx, session).await
    }

    async fn session_get(&self, id: &WorkerSessionId) -> Result<Option<WorkerSession>> {
        let row = sqlx::query(
            r#"SELECT id, wave_id, provider, mode, contract, parent_session_id,
                      requester_session_id, state, mcp_token_hash, thread_id,
                      agent_session_id, active_turn_id, terminal_run_id,
                      handle_state_json, liveness, liveness_probed_at_ms,
                      exit_code, exit_interpretation, spawn_op_id,
                      last_activity_ms, last_thread_status, created_at_ms,
                      updated_at_ms, completed_at_ms
               FROM worker_sessions
               WHERE id = ?1"#,
        )
        .bind(id.as_str())
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(worker_session_from_row).transpose()
    }

    async fn sessions_nonterminal(&self) -> Result<Vec<WorkerSession>> {
        let rows = sqlx::query(
            r#"SELECT id, wave_id, provider, mode, contract, parent_session_id,
                      requester_session_id, state, mcp_token_hash, thread_id,
                      agent_session_id, active_turn_id, terminal_run_id,
                      handle_state_json, liveness, liveness_probed_at_ms,
                      exit_code, exit_interpretation, spawn_op_id,
                      last_activity_ms, last_thread_status, created_at_ms,
                      updated_at_ms, completed_at_ms
               FROM worker_sessions
               WHERE state IN ('starting', 'running', 'idle', 'turn_pending')
               ORDER BY wave_id ASC, created_at_ms ASC, id ASC"#,
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(worker_session_from_row).collect()
    }

    async fn session_set_liveness(
        &self,
        id: &WorkerSessionId,
        liveness: &Liveness,
        probed_at_ms: i64,
    ) -> Result<Option<WorkerSession>> {
        let mut tx = begin_immediate_tx(&self.pool).await?;
        let out = session_set_liveness_tx(&mut tx, id, liveness, probed_at_ms).await?;
        tx.commit().await?;
        Ok(out)
    }

    async fn session_record_activity(
        &self,
        id: &WorkerSessionId,
        last_activity_ms: i64,
        last_thread_status: &str,
    ) -> Result<()> {
        let mut tx = begin_immediate_tx(&self.pool).await?;
        session_record_activity_tx(&mut tx, id, last_activity_ms, last_thread_status).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn session_record_activity_by_thread(
        &self,
        thread_id: &str,
        last_activity_ms: i64,
        last_thread_status: &str,
    ) -> Result<()> {
        let mut tx = begin_immediate_tx(&self.pool).await?;
        session_record_activity_by_thread_tx(
            &mut tx,
            thread_id,
            last_activity_ms,
            last_thread_status,
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn session_state_transition_tx(
        &self,
        tx: &mut SessionTx<'_>,
        id: &WorkerSessionId,
        to: WorkerSessionState,
    ) -> Result<WorkerSession> {
        session_state_transition_tx(tx, id, to).await
    }

    async fn session_commit_exit(
        &self,
        id: &WorkerSessionId,
        to: WorkerSessionState,
        liveness_probed_at_ms: i64,
        exit_code: Option<i32>,
        exit_interpretation: &str,
    ) -> Result<CommitExitOutcome> {
        let runtime_status = runtime_status_for_exit_session_state(id, to)?;
        let mut tx = begin_immediate_tx(&self.pool).await?;
        let session = match session_commit_exit_tx(
            &mut tx,
            id,
            to,
            liveness_probed_at_ms,
            exit_code,
            exit_interpretation,
        )
        .await
        {
            Ok(session) => session,
            Err(err) if is_session_conflict(&err) => return Ok(CommitExitOutcome::Absorbed),
            Err(err) => return Err(err),
        };

        match runtime_status_flip_tx(&mut tx, &id.0, runtime_status).await {
            Ok(()) => {}
            Err(RuntimeRepoError::IllegalStatusTransition { .. }) => {
                return Ok(CommitExitOutcome::Absorbed);
            }
            Err(err) => return Err(err.into()),
        }

        tx.commit().await?;
        Ok(CommitExitOutcome::Committed(session))
    }

    async fn session_list_by_wave(&self, wave_id: &WaveId) -> Result<Vec<WorkerSession>> {
        let rows = sqlx::query(
            r#"SELECT id, wave_id, provider, mode, contract, parent_session_id,
                      requester_session_id, state, mcp_token_hash, thread_id,
                      agent_session_id, active_turn_id, terminal_run_id,
                      handle_state_json, liveness, liveness_probed_at_ms,
                      exit_code, exit_interpretation, spawn_op_id,
                      last_activity_ms, last_thread_status, created_at_ms,
                      updated_at_ms, completed_at_ms
               FROM worker_sessions
               WHERE wave_id = ?1
               ORDER BY created_at_ms ASC, id ASC"#,
        )
        .bind(wave_id.as_str())
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(worker_session_from_row).collect()
    }

    async fn dead_root_candidates(&self) -> Result<Vec<DeadRootCandidate>> {
        // The soundness predicate lives entirely here (#741-4 DR-4). Two arms,
        // both gated on a POSITIVE dead signal AND the mid-respawn exclusion
        // (no active planner-contract session). NEVER converges on absence or
        // a just-created wave.
        //
        //  * Failed-start (Draft): the wave is still `draft` AND its
        //    *most-recent* `spec-harness-start` operation resolved to
        //    `phase='failed'`. The op→wave link is the immutable
        //    `payload_json.wave_id` (`idempotency_key` is None and
        //    `target_type/id` is later rewritten to the spec card, so neither
        //    is a reliable key — the payload is stamped once at insert and
        //    never changes). Start/reset re-submit `spec-harness-start` with a
        //    FRESH op id, so a wave can carry a STALE `failed` start-op AND a
        //    NEWER retry (`pending`/`running`/`succeeded`) start-op at once;
        //    during the retry's setup window (new op submitted, planner session
        //    not yet created) `no_active_planner` is momentarily true. Keying
        //    on the LATEST start-op — `rowid = MAX(rowid)` over this wave's
        //    start-ops — closes that hole: `rowid` is SQLite's monotonic
        //    insertion order (the `operations` table is rowid-backed, not
        //    `WITHOUT ROWID`; `id` is a random uuid-v4 and `created_at_ms` is
        //    wall-clock ms that can tie, so neither orders insertions
        //    reliably). If the latest start-op is non-failed (retry in flight
        //    or a success), or there is no start-op row yet, the signal is NOT
        //    positive ⇒ left.
        //  * Lost-root (Planning): the wave is `planning` AND its root session
        //    is NULL or points at a terminal/missing session. A `Resumable`
        //    (codex) root that is still alive is `is_active_authority` ⇒ caught
        //    by the active-planner exclusion below, so a codex root is never
        //    declared dead on a bare PTY-`Exited` — only via its terminal
        //    `worker_sessions.state` (set by the worker reaper's S1/S2 arbiter).
        //
        // Dispatching/Blocked are intentionally OUT OF SCOPE (no DR-1 edge).
        let active = "('starting', 'running', 'idle', 'turn_pending')";
        let no_active_planner = format!(
            "NOT EXISTS (SELECT 1 FROM worker_sessions ws \
               WHERE ws.wave_id = w.id AND ws.contract = 'planner' \
                 AND ws.state IN {active})"
        );
        let sql = format!(
            r#"SELECT w.id AS wave_id, w.cove_id AS cove_id, w.lifecycle AS lifecycle
                 FROM waves w
                WHERE w.lifecycle = 'draft'
                  AND EXISTS (
                      SELECT 1 FROM operations o
                       WHERE o.kind = 'spec-harness-start'
                         AND o.phase = 'failed'
                         AND json_extract(o.payload_json, '$.wave_id') = w.id
                         AND o.rowid = (
                             SELECT MAX(o2.rowid) FROM operations o2
                              WHERE o2.kind = 'spec-harness-start'
                                AND json_extract(o2.payload_json, '$.wave_id') = w.id
                         )
                  )
                  AND {no_active_planner}
               UNION ALL
               SELECT w.id AS wave_id, w.cove_id AS cove_id, w.lifecycle AS lifecycle
                 FROM waves w
                WHERE w.lifecycle = 'planning'
                  AND (
                      w.root_session_id IS NULL
                      OR NOT EXISTS (
                          SELECT 1 FROM worker_sessions rs
                           WHERE rs.id = w.root_session_id
                             AND rs.state IN {active}
                      )
                  )
                  AND {no_active_planner}
               ORDER BY wave_id ASC"#
        );
        let rows = sqlx::query(&sql).fetch_all(&self.pool).await?;
        rows.into_iter()
            .map(|row| {
                let wave_id: String = row.try_get("wave_id")?;
                let cove_id: String = row.try_get("cove_id")?;
                let lifecycle_raw: String = row.try_get("lifecycle")?;
                let lifecycle = WaveLifecycle::try_from(lifecycle_raw.clone()).map_err(|e| {
                    CalmError::Internal(format!(
                        "dead_root_candidates: unknown wave lifecycle {lifecycle_raw:?}: {e}"
                    ))
                })?;
                Ok(DeadRootCandidate {
                    wave_id: WaveId::from(wave_id),
                    cove_id: CoveId::from(cove_id),
                    lifecycle,
                })
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// RepoSyncDomainRaw — raw entity writes for the in-scope sync domain.
// Gated: not reachable via the `RouteRepo` trait object that handlers see;
// only callable via the explicit `AppState::raw_repo()` escape hatch.
// ---------------------------------------------------------------------------

#[async_trait]
impl RepoSyncDomainRaw for SqlxRepo {
    // ---------------------------------------------------------------- coves
    async fn cove_create(&self, p: NewCove) -> Result<Cove> {
        let mut tx = self.pool.begin().await?;
        let out = cove_create_tx(&mut tx, p).await?;
        tx.commit().await?;
        Ok(out)
    }

    async fn cove_update(&self, id: &str, p: CovePatch) -> Result<Cove> {
        let mut tx = self.pool.begin().await?;
        let out = cove_update_tx(&mut tx, id, p).await?;
        tx.commit().await?;
        Ok(out)
    }

    async fn cove_delete(&self, id: &str) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        overlay_delete_subtree_by_cove_tx(&mut tx, id).await?;
        overlay_delete_by_entity_tx(&mut tx, "cove", id).await?;
        cove_delete_tx(&mut tx, id).await?;
        tx.commit().await?;
        Ok(())
    }

    // ---------------------------------------------------------------- waves
    async fn wave_create(&self, p: NewWave) -> Result<Wave> {
        let mut tx = self.pool.begin().await?;
        let out = wave_create_tx(&mut tx, p, &self.wave_cove_cache).await?;
        tx.commit().await?;
        Ok(out)
    }

    async fn wave_update(&self, id: &str, p: WavePatch) -> Result<Wave> {
        let mut tx = self.pool.begin().await?;
        let out = wave_update_tx(&mut tx, id, p).await?;
        tx.commit().await?;
        Ok(out)
    }

    async fn wave_delete(&self, id: &str) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        overlay_delete_card_overlays_by_wave_tx(&mut tx, id).await?;
        overlay_delete_by_entity_tx(&mut tx, "wave", id).await?;
        overlay_delete_by_entity_tx(&mut tx, "view", id).await?;
        wave_delete_tx(&mut tx, id, &self.wave_cove_cache).await?;
        tx.commit().await?;
        Ok(())
    }

    // ---------------------------------------------------------------- cards
    async fn card_create(&self, p: NewCard) -> Result<Card> {
        let mut tx = self.pool.begin().await?;
        let out = card_create_tx(&mut tx, p, &self.card_role_cache).await?;
        tx.commit().await?;
        Ok(out)
    }

    async fn card_update(&self, id: &str, p: CardPatch) -> Result<Card> {
        let mut tx = self.pool.begin().await?;
        let out = card_update_tx(&mut tx, id, p).await?;
        tx.commit().await?;
        Ok(out)
    }

    async fn card_delete(&self, id: &str) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        card_delete_tx(&mut tx, id, &self.card_role_cache).await?;
        tx.commit().await?;
        Ok(())
    }

    // -------------------------------------------------------------- overlays
    async fn overlay_upsert(&self, p: NewOverlay) -> Result<Overlay> {
        let mut tx = self.pool.begin().await?;
        let out = overlay_upsert_tx(&mut tx, p).await?;
        tx.commit().await?;
        Ok(out)
    }

    async fn overlay_delete(
        &self,
        plugin_id: &str,
        entity_kind: &str,
        entity_id: &str,
        kind: &str,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        overlay_delete_tx(&mut tx, plugin_id, entity_kind, entity_id, kind).await?;
        tx.commit().await?;
        Ok(())
    }
}

pub async fn harness_items_delete_by_card_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: &str,
) -> Result<()> {
    sqlx::query("DELETE FROM harness_items WHERE card_id = ?1")
        .bind(card_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// #695 PR2 — append one `worker_flow_items` row inside an open transaction,
/// returning the new row id. Free fn (mirroring the harness `_tx` helpers) so
/// PR3's `WorkerFlowItemSink` can call it from inside `commit_decision`'s
/// closure. The `RepoOutOfDomain::worker_flow_item_insert` trait method wraps
/// this in its own short transaction for standalone callers.
#[allow(clippy::too_many_arguments)]
pub async fn worker_flow_item_insert_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: Option<&str>,
    runtime_id: Option<&str>,
    wave_id: Option<&str>,
    worker_session_id: Option<&str>,
    kind: &str,
    payload: &str,
    created_at_ms: i64,
) -> Result<i64> {
    let row = sqlx::query(
        r#"INSERT INTO worker_flow_items (
               card_id, runtime_id, wave_id, worker_session_id,
               kind, payload, created_at_ms
           )
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
           RETURNING id"#,
    )
    .bind(card_id)
    .bind(runtime_id)
    .bind(wave_id)
    .bind(worker_session_id)
    .bind(kind)
    .bind(payload)
    .bind(created_at_ms)
    .fetch_one(&mut **tx)
    .await?;
    Ok(row.get::<i64, _>("id"))
}

/// #695 PR2 — hard-delete every `worker_flow_items` row for a card. Mirror of
/// [`harness_items_delete_by_card_tx`]. Unlike the FK's `ON DELETE SET NULL`
/// (which preserves the transcript when the *card* is deleted), this is the
/// explicit "purge this card's captured flow" path a caller can invoke
/// directly inside a transaction.
pub async fn worker_flow_items_delete_by_card_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: &str,
) -> Result<()> {
    sqlx::query("DELETE FROM worker_flow_items WHERE card_id = ?1")
        .bind(card_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// RepoOutOfDomain — operational writes that intentionally bypass the event
// log: terminal lifecycle, plugin install/config, app-global settings. See
// db/mod.rs module doc for the sync-domain vs. out-of-domain split.
// ---------------------------------------------------------------------------

#[async_trait]
impl RepoOutOfDomain for SqlxRepo {
    // ------------------------------------------------------------- terminals
    async fn terminal_create(&self, p: NewTerminal) -> Result<Terminal> {
        // Parent card must exist; surface as NotFound to mirror MockRepo.
        let exists: Option<(String,)> = sqlx::query_as("SELECT id FROM cards WHERE id = ?1")
            .bind(p.card_id.as_str())
            .fetch_optional(&self.pool)
            .await?;
        if exists.is_none() {
            return Err(CalmError::NotFound(format!("card {}", p.card_id)));
        }
        // Per-card uniqueness — surface as Conflict to mirror MockRepo
        // (the schema also enforces this via UNIQUE on terminals.card_id).
        let dup: Option<(String,)> = sqlx::query_as("SELECT id FROM terminals WHERE card_id = ?1")
            .bind(p.card_id.as_str())
            .fetch_optional(&self.pool)
            .await?;
        if dup.is_some() {
            return Err(CalmError::Conflict(format!(
                "terminal already exists for card {}",
                p.card_id
            )));
        }

        let now = now_ms();
        let id = new_id();
        let env_text = serde_json::to_string(&p.env)?;
        // #177 — render theme RGB once at row-creation; persisted in
        // comma-decimal form so every spawn-path read is a zero-alloc
        // string slice.
        let theme_fg = p.theme.fg_arg();
        let theme_bg = p.theme.bg_arg();
        sqlx::query(
            r#"INSERT INTO terminals
                   (id, card_id, program, cwd, env, pid, theme_fg, theme_bg, created_at)
               VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6, ?7, ?8)"#,
        )
        .bind(&id)
        .bind(p.card_id.as_str())
        .bind(&p.program)
        .bind(&p.cwd)
        .bind(&env_text)
        .bind(&theme_fg)
        .bind(&theme_bg)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(Terminal {
            id,
            card_id: p.card_id,
            program: p.program,
            cwd: p.cwd,
            env: p.env,
            pid: None,
            theme_fg,
            theme_bg,
            exit_code: None,
            signal_killed: false,
            created_at: now,
        })
    }

    async fn terminal_set_pid(&self, id: &str, pid: Option<u32>) -> Result<()> {
        // Cast to i64 for sqlite's INTEGER affinity; u32 is well within range.
        let pid_i64: Option<i64> = pid.map(|p| p as i64);
        let res = sqlx::query("UPDATE terminals SET pid = ?1 WHERE id = ?2")
            .bind(pid_i64)
            .bind(id)
            .execute(&self.pool)
            .await?;
        if res.rows_affected() == 0 {
            return Err(CalmError::NotFound(format!("terminal {id}")));
        }
        Ok(())
    }

    async fn terminal_set_exit(
        &self,
        id: &str,
        exit_code: Option<i32>,
        signal_killed: bool,
    ) -> Result<()> {
        // #306 — single UPDATE; the two columns are written together so
        // a reader never sees a mismatched intermediate state. The
        // mutual-exclusion invariant (signal_killed=true ⇒ exit_code=None)
        // is the writer's responsibility — see daemon `spawn_child_waiter`.
        let res =
            sqlx::query("UPDATE terminals SET exit_code = ?1, signal_killed = ?2 WHERE id = ?3")
                .bind(exit_code)
                .bind(if signal_killed { 1_i64 } else { 0_i64 })
                .bind(id)
                .execute(&self.pool)
                .await?;
        if res.rows_affected() == 0 {
            return Err(CalmError::NotFound(format!("terminal {id}")));
        }
        Ok(())
    }

    async fn terminal_clear_exit_for_spawn(&self, id: &str) -> Result<()> {
        let res = sqlx::query(
            "UPDATE terminals SET pid = NULL, exit_code = NULL, signal_killed = 0 WHERE id = ?1",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        if res.rows_affected() == 0 {
            return Err(CalmError::NotFound(format!("terminal {id}")));
        }
        Ok(())
    }

    async fn terminal_delete(&self, id: &str) -> Result<()> {
        let res = sqlx::query("DELETE FROM terminals WHERE id = ?1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        if res.rows_affected() == 0 {
            return Err(CalmError::NotFound(format!("terminal {id}")));
        }
        Ok(())
    }

    async fn shared_daemon_runtime_set(&self, update: SharedCodexDaemonUpdate) -> Result<()> {
        let now = now_ms();
        let start_time = update
            .process_start_time
            .and_then(|v| i64::try_from(v).ok());
        sqlx::query(
            r#"INSERT INTO shared_codex_daemon
                   (id, state, pid, pgid, sock_path, codex_home_path, process_start_time,
                    boot_id, started_at, updated_at, restart_count, last_error,
                    daemon_env_signature)
               VALUES
                   (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9,
                    CASE WHEN ?10 THEN 1 ELSE 0 END, ?11, ?12)
               ON CONFLICT(id) DO UPDATE SET
                   state = excluded.state,
                   pid = excluded.pid,
                   pgid = excluded.pgid,
                   sock_path = excluded.sock_path,
                   codex_home_path = excluded.codex_home_path,
                   process_start_time = excluded.process_start_time,
                   boot_id = excluded.boot_id,
                   started_at = excluded.started_at,
                   updated_at = excluded.updated_at,
                   restart_count = shared_codex_daemon.restart_count
                       + CASE WHEN ?10 THEN 1 ELSE 0 END,
                   last_error = excluded.last_error,
                   daemon_env_signature = excluded.daemon_env_signature"#,
        )
        .bind(&update.state)
        .bind(update.pid)
        .bind(update.pgid)
        .bind(&update.sock_path)
        .bind(&update.codex_home_path)
        .bind(start_time)
        .bind(&update.boot_id)
        .bind(update.started_at)
        .bind(now)
        .bind(update.increment_restart_count)
        .bind(&update.last_error)
        .bind(&update.daemon_env_signature)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn shared_daemon_record_event(&self, action: &str, error: Option<&str>) -> Result<()> {
        let now = now_ms();
        let last_error = error.map(|e| format!("{action}: {e}"));
        sqlx::query(
            r#"UPDATE shared_codex_daemon
                  SET updated_at = ?1,
                      last_error = COALESCE(?2, last_error)
                WHERE id = 1"#,
        )
        .bind(now)
        .bind(last_error)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // ---- spec harness item stream (#510 PR-ui C1) -----------------------

    #[allow(clippy::too_many_arguments)]
    async fn harness_item_insert(
        &self,
        runtime_id: &str,
        card_id: &str,
        wave_id: &str,
        thread_id: &str,
        turn_id: Option<&str>,
        item_uuid: Option<&str>,
        item_type: Option<&str>,
        method: &str,
        params: &str,
    ) -> Result<i64> {
        let row = sqlx::query(
            r#"INSERT INTO harness_items (
                   runtime_id, card_id, wave_id, thread_id, turn_id,
                   item_uuid, item_type, method, params, created_at_ms
               )
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
               RETURNING id"#,
        )
        .bind(runtime_id)
        .bind(card_id)
        .bind(wave_id)
        .bind(thread_id)
        .bind(turn_id)
        .bind(item_uuid)
        .bind(item_type)
        .bind(method)
        .bind(params)
        .bind(now_ms())
        .fetch_one(&self.pool)
        .await?;
        Ok(row.get::<i64, _>("id"))
    }

    // ---- worker message-flow capture (#695 PR2) -------------------------

    #[allow(clippy::too_many_arguments)]
    async fn worker_flow_item_insert(
        &self,
        card_id: Option<&str>,
        runtime_id: Option<&str>,
        wave_id: Option<&str>,
        worker_session_id: Option<&str>,
        kind: &str,
        payload: &str,
        created_at_ms: i64,
    ) -> Result<i64> {
        let mut tx = self.pool.begin().await?;
        let id = worker_flow_item_insert_tx(
            &mut tx,
            card_id,
            runtime_id,
            wave_id,
            worker_session_id,
            kind,
            payload,
            created_at_ms,
        )
        .await?;
        tx.commit().await?;
        Ok(id)
    }

    #[allow(clippy::too_many_arguments)]
    async fn worker_flow_cursor_upsert(
        &self,
        card_id: &str,
        source_kind: &str,
        source_path: &str,
        record_index: i64,
        byte_offset: i64,
        last_source_uuid: Option<&str>,
        last_line_hash: Option<&str>,
        updated_at_ms: i64,
    ) -> Result<()> {
        sqlx::query(
            r#"INSERT INTO worker_flow_cursors (
                   card_id, source_kind, source_path, record_index,
                   byte_offset, last_source_uuid, last_line_hash, updated_at_ms
               )
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
               ON CONFLICT(card_id, source_kind) DO UPDATE SET
                   source_path = excluded.source_path,
                   record_index = excluded.record_index,
                   byte_offset = excluded.byte_offset,
                   last_source_uuid = excluded.last_source_uuid,
                   last_line_hash = excluded.last_line_hash,
                   updated_at_ms = excluded.updated_at_ms"#,
        )
        .bind(card_id)
        .bind(source_kind)
        .bind(source_path)
        .bind(record_index)
        .bind(byte_offset)
        .bind(last_source_uuid)
        .bind(last_line_hash)
        .bind(updated_at_ms)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // --------------------------------------------------------------- plugins
    async fn plugin_install(&self, p: NewPlugin) -> Result<Plugin> {
        let manifest_text = serde_json::to_string(&p.manifest)?;
        let user_config_text = serde_json::to_string(&p.user_config)?;
        let now = now_ms();
        let row = sqlx::query_as::<_, Plugin>(
            r#"INSERT INTO plugins
                   (id, version, install_path, manifest, enabled, user_config,
                    installed_at, updated_at)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
               ON CONFLICT(id) DO UPDATE SET
                   version      = excluded.version,
                   install_path = excluded.install_path,
                   manifest     = excluded.manifest,
                   enabled      = excluded.enabled,
                   user_config  = excluded.user_config,
                   updated_at   = excluded.updated_at
               RETURNING id, version, install_path, manifest, enabled, user_config,
                         installed_at, updated_at"#,
        )
        .bind(&p.id)
        .bind(&p.version)
        .bind(&p.install_path)
        .bind(&manifest_text)
        .bind(p.enabled)
        .bind(&user_config_text)
        .bind(now)
        .fetch_one(&self.pool)
        .await?;
        Ok(row)
    }

    async fn plugin_update_enabled(&self, id: &str, enabled: bool) -> Result<Plugin> {
        let now = now_ms();
        let res = sqlx::query(r#"UPDATE plugins SET enabled = ?1, updated_at = ?2 WHERE id = ?3"#)
            .bind(enabled)
            .bind(now)
            .bind(id)
            .execute(&self.pool)
            .await?;
        if res.rows_affected() == 0 {
            return Err(CalmError::NotFound(format!("plugin {id}")));
        }
        self.plugin_get_by_id(id)
            .await?
            .ok_or_else(|| CalmError::NotFound(format!("plugin {id}")))
    }

    async fn plugin_update_user_config(
        &self,
        id: &str,
        user_config: serde_json::Value,
    ) -> Result<Plugin> {
        let now = now_ms();
        let user_config_text = serde_json::to_string(&user_config)?;
        let res =
            sqlx::query(r#"UPDATE plugins SET user_config = ?1, updated_at = ?2 WHERE id = ?3"#)
                .bind(&user_config_text)
                .bind(now)
                .bind(id)
                .execute(&self.pool)
                .await?;
        if res.rows_affected() == 0 {
            return Err(CalmError::NotFound(format!("plugin {id}")));
        }
        self.plugin_get_by_id(id)
            .await?
            .ok_or_else(|| CalmError::NotFound(format!("plugin {id}")))
    }

    async fn plugin_update_manifest(
        &self,
        id: &str,
        manifest: serde_json::Value,
    ) -> Result<Plugin> {
        let now = now_ms();
        let manifest_text = serde_json::to_string(&manifest)?;
        let res = sqlx::query(r#"UPDATE plugins SET manifest = ?1, updated_at = ?2 WHERE id = ?3"#)
            .bind(&manifest_text)
            .bind(now)
            .bind(id)
            .execute(&self.pool)
            .await?;
        if res.rows_affected() == 0 {
            return Err(CalmError::NotFound(format!("plugin {id}")));
        }
        self.plugin_get_by_id(id)
            .await?
            .ok_or_else(|| CalmError::NotFound(format!("plugin {id}")))
    }

    async fn plugin_delete(&self, id: &str) -> Result<()> {
        let res = sqlx::query("DELETE FROM plugins WHERE id = ?1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        if res.rows_affected() == 0 {
            return Err(CalmError::NotFound(format!("plugin {id}")));
        }
        Ok(())
    }

    async fn overlays_clear_by_plugin(&self, plugin_id: &str) -> Result<()> {
        sqlx::query("DELETE FROM overlays WHERE plugin_id = ?1")
            .bind(plugin_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn plugin_kv_clear(&self, plugin_id: &str) -> Result<()> {
        sqlx::query("DELETE FROM plugin_kv WHERE plugin_id = ?1")
            .bind(plugin_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // -------------------------------------------------------- plugin tokens
    async fn plugin_token_set(
        &self,
        plugin_id: &str,
        hashed_token: &str,
        expires_at: i64,
    ) -> Result<()> {
        sqlx::query(
            r#"INSERT INTO plugin_tokens (plugin_id, hashed_token, expires_at)
               VALUES (?1, ?2, ?3)
               ON CONFLICT(plugin_id) DO UPDATE SET
                   hashed_token = excluded.hashed_token,
                   expires_at   = excluded.expires_at"#,
        )
        .bind(plugin_id)
        .bind(hashed_token)
        .bind(expires_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn plugin_token_delete(&self, plugin_id: &str) -> Result<()> {
        sqlx::query("DELETE FROM plugin_tokens WHERE plugin_id = ?1")
            .bind(plugin_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // -------------------------------------------------------- plugin kv
    async fn plugin_kv_set(
        &self,
        plugin_id: &str,
        key: &str,
        value: &serde_json::Value,
    ) -> Result<()> {
        let text = serde_json::to_string(value)?;
        let now = now_ms();
        sqlx::query(
            r#"INSERT INTO plugin_kv (plugin_id, key, value, updated_at)
               VALUES (?1, ?2, ?3, ?4)
               ON CONFLICT(plugin_id, key) DO UPDATE SET
                   value      = excluded.value,
                   updated_at = excluded.updated_at"#,
        )
        .bind(plugin_id)
        .bind(key)
        .bind(&text)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn plugin_kv_delete(&self, plugin_id: &str, key: &str) -> Result<()> {
        sqlx::query("DELETE FROM plugin_kv WHERE plugin_id = ?1 AND key = ?2")
            .bind(plugin_id)
            .bind(key)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // -------------------------------------------------------------- settings
    async fn settings_upsert(&self, key: &str, value: &str) -> Result<()> {
        let now = now_ms();
        sqlx::query(
            r#"INSERT INTO settings (key, value, updated_at)
               VALUES (?1, ?2, ?3)
               ON CONFLICT(key) DO UPDATE SET
                   value      = excluded.value,
                   updated_at = excluded.updated_at"#,
        )
        .bind(key)
        .bind(value)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn settings_delete(&self, key: &str) -> Result<()> {
        sqlx::query("DELETE FROM settings WHERE key = ?1")
            .bind(key)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // ----------------------------------------------------- cove_folders
    async fn cove_folder_create(&self, cove_id: &str, path: &str) -> Result<CoveFolder> {
        // Parent cove must exist; surface as NotFound to mirror the
        // terminal_create precedent above (FK error message would be
        // less actionable for the REST caller).
        let exists: Option<(String,)> = sqlx::query_as("SELECT id FROM coves WHERE id = ?1")
            .bind(cove_id)
            .fetch_optional(&self.pool)
            .await?;
        if exists.is_none() {
            return Err(CalmError::NotFound(format!("cove {cove_id}")));
        }
        let now = now_ms();
        // The UNIQUE constraint on `path` is the backstop here. The
        // route layer has already done equality / ancestor / descendant
        // conflict detection so a real-world INSERT failing the
        // UNIQUE is a race (concurrent claim of the same path). Bubble
        // it up as the generic Conflict so the surface is honest.
        let res =
            sqlx::query("INSERT INTO cove_folders (cove_id, path, created_at) VALUES (?1, ?2, ?3)")
                .bind(cove_id)
                .bind(path)
                .bind(now)
                .execute(&self.pool)
                .await;
        match res {
            Ok(out) => Ok(CoveFolder {
                id: out.last_insert_rowid(),
                cove_id: cove_id.to_string().into(),
                path: path.to_string(),
                created_at: now,
            }),
            Err(sqlx::Error::Database(dbe)) if dbe.message().contains("UNIQUE") => Err(
                CalmError::Conflict(format!("cove_folders.path already claims `{path}`")),
            ),
            Err(e) => Err(e.into()),
        }
    }

    async fn cove_folder_delete(&self, id: i64) -> Result<()> {
        let res = sqlx::query("DELETE FROM cove_folders WHERE id = ?1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        if res.rows_affected() == 0 {
            return Err(CalmError::NotFound(format!("cove_folder {id}")));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// RepoEventWrite — the eventized write path. Every public write that the
// sync engine cares about lands here: `write_with_event` (atomic entity-
// write + event-log), `log_pure_event` (entity-less event log), and the
// `events_*` cursor queries used by replay.
// ---------------------------------------------------------------------------

#[allow(deprecated)]
#[async_trait]
impl RepoEventWrite for SqlxRepo {
    async fn write_with_event(
        &self,
        actor: ActorId,
        scope: EventScope,
        correlation: Option<&str>,
        bus: &EventBus,
        write: &crate::state::WriteContext,
        f: WriteWithEventFn<'_>,
    ) -> Result<i64> {
        // BEGIN IMMEDIATE takes the writer lock at tx start; deferred SELECT-then-UPDATE upgrades can hit SQLITE_BUSY_SNAPSHOT, which busy_timeout does not cover.
        let mut tx = begin_immediate_tx(&self.pool).await?;
        // Run the caller-supplied entity write.
        let fut: BoxFuture<'_, Result<Event>> = f(&mut tx);
        let event = match fut.await {
            Ok(ev) => ev,
            Err(e) => {
                // Rollback is implicit on `tx` drop, but be explicit so the
                // intent reads clearly.
                let _ = tx.rollback().await;
                return Err(e);
            }
        };
        // PR3 (#136) — authorization gate. Runs after the closure
        // produces an event so the closure can mint per-row roles
        // (e.g. `card_create_with_id_tx` writes through the cache)
        // before the gate checks them. Violations roll back: no
        // entity write, no event row, no broadcast.
        if let Err(violation) = crate::role_gate::enforce_role(
            &actor,
            &event,
            &scope,
            write.role_cache(),
            write.cove_cache(),
        ) {
            let _ = tx.rollback().await;
            return Err(CalmError::Forbidden(violation.to_string()));
        }
        // Persist the event in the same txn.
        let event_id =
            match Self::event_append_in_tx(&mut tx, &actor, &scope, correlation, &event).await {
                Ok(id) => id,
                Err(e) => {
                    let _ = tx.rollback().await;
                    return Err(e);
                }
            };
        if let Some(wave_id) = scope.wave_id()
            && let Err(e) = wave_vcs::commit_in_tx(
                &mut tx,
                wave_id,
                &actor,
                event_id,
                &event,
                wave_vcs::MANIFEST_SCHEMA_VERSION,
            )
            .await
        {
            let _ = tx.rollback().await;
            return Err(e);
        }
        // Commit before any externally-visible side effect.
        tx.commit().await?;
        // Commit-then-emit invariant: now (and only now) do we broadcast.
        bus.emit_envelope(BroadcastEnvelope {
            id: event_id,
            event_version: SYNC_EVENT_VERSION,
            actor,
            scope,
            event,
        });
        Ok(event_id)
    }

    async fn write_with_events(
        &self,
        actor: ActorId,
        correlation: Option<&str>,
        bus: &EventBus,
        write: &crate::state::WriteContext,
        f: WriteWithEventsFn<'_>,
    ) -> Result<Vec<i64>> {
        // BEGIN IMMEDIATE takes the writer lock at tx start; deferred SELECT-then-UPDATE upgrades can hit SQLITE_BUSY_SNAPSHOT, which busy_timeout does not cover.
        let mut tx = begin_immediate_tx(&self.pool).await?;
        // Run the caller-supplied entity write — closure returns one
        // or more (scope, event) pairs for this tx.
        let fut: BoxFuture<'_, Result<Vec<(EventScope, Event)>>> = f(&mut tx);
        let events = match fut.await {
            Ok(v) => v,
            Err(e) => {
                let _ = tx.rollback().await;
                return Err(e);
            }
        };
        // Contract: at least one event per tx. An empty vec is a
        // caller bug — refuse to commit so the closure's writes
        // disappear with the rollback.
        if events.is_empty() {
            let _ = tx.rollback().await;
            return Err(CalmError::Internal(
                "write_with_events: closure returned an empty event batch".into(),
            ));
        }
        // PR3 (#136) — authorization gate, per event. The cache is
        // already write-through for any role insert the closure
        // performed, so a wave-create-with-spec-card batch can mint
        // the spec card in the closure and immediately have its
        // role visible to the `WaveUpdated` enforce_role call below.
        for (scope, event) in &events {
            if let Err(violation) = crate::role_gate::enforce_role(
                &actor,
                event,
                scope,
                write.role_cache(),
                write.cove_cache(),
            ) {
                let _ = tx.rollback().await;
                return Err(CalmError::Forbidden(violation.to_string()));
            }
        }
        // Persist every event in the same txn, in order.
        let mut event_ids: Vec<i64> = Vec::with_capacity(events.len());
        for (scope, event) in &events {
            match Self::event_append_in_tx(&mut tx, &actor, scope, correlation, event).await {
                Ok(id) => event_ids.push(id),
                Err(e) => {
                    let _ = tx.rollback().await;
                    return Err(e);
                }
            }
        }
        let mut wave_events = HashMap::<WaveId, (i64, Vec<Event>)>::new();
        for ((scope, event), event_id) in events.iter().zip(event_ids.iter()) {
            if let Some(wave_id) = scope.wave_id() {
                let entry = wave_events
                    .entry(wave_id.clone())
                    .or_insert_with(|| (*event_id, Vec::new()));
                entry.0 = *event_id;
                entry.1.push(event.clone());
            }
        }
        for (wave_id, (event_id, events_for_wave)) in &wave_events {
            if let Err(e) = wave_vcs::commit_events_in_tx(
                &mut tx,
                wave_id,
                &actor,
                *event_id,
                events_for_wave,
                wave_vcs::MANIFEST_SCHEMA_VERSION,
            )
            .await
            {
                let _ = tx.rollback().await;
                return Err(e);
            }
        }
        // Commit before any externally-visible side effect.
        tx.commit().await?;
        // Commit-then-emit invariant: broadcast in the same order the
        // closure produced.
        for (id, (scope, event)) in event_ids.iter().zip(events) {
            bus.emit_envelope(BroadcastEnvelope {
                id: *id,
                event_version: SYNC_EVENT_VERSION,
                actor: actor.clone(),
                scope,
                event,
            });
        }
        Ok(event_ids)
    }

    async fn write_with_actor_events(
        &self,
        correlation: Option<&str>,
        bus: &EventBus,
        write: &crate::state::WriteContext,
        f: WriteWithActorEventsFn<'_>,
    ) -> Result<Vec<i64>> {
        // BEGIN IMMEDIATE takes the writer lock at tx start; deferred SELECT-then-UPDATE upgrades can hit SQLITE_BUSY_SNAPSHOT, which busy_timeout does not cover.
        let mut tx = begin_immediate_tx(&self.pool).await?;
        let fut: BoxFuture<'_, Result<Vec<(ActorId, EventScope, Event)>>> = f(&mut tx);
        let events = match fut.await {
            Ok(v) => v,
            Err(e) => {
                let _ = tx.rollback().await;
                return Err(e);
            }
        };
        if events.is_empty() {
            let _ = tx.rollback().await;
            return Err(CalmError::Internal(
                "write_with_actor_events: closure returned an empty event batch".into(),
            ));
        }
        for (actor, scope, event) in &events {
            if let Err(violation) = crate::role_gate::enforce_role(
                actor,
                event,
                scope,
                write.role_cache(),
                write.cove_cache(),
            ) {
                let _ = tx.rollback().await;
                return Err(CalmError::Forbidden(violation.to_string()));
            }
        }
        let mut event_ids: Vec<i64> = Vec::with_capacity(events.len());
        for (actor, scope, event) in &events {
            match Self::event_append_in_tx(&mut tx, actor, scope, correlation, event).await {
                Ok(id) => event_ids.push(id),
                Err(e) => {
                    let _ = tx.rollback().await;
                    return Err(e);
                }
            }
        }
        let mut wave_events = HashMap::<WaveId, (i64, Option<ActorId>, Vec<Event>)>::new();
        for ((actor, scope, event), event_id) in events.iter().zip(event_ids.iter()) {
            if let Some(wave_id) = scope.wave_id() {
                let entry = wave_events
                    .entry(wave_id.clone())
                    .or_insert_with(|| (*event_id, Some(actor.clone()), Vec::new()));
                // Commit author is exact only for a single-actor wave batch; mixed actor batches
                // are stored as NULL so the diff renderer leaves them unattributed.
                entry.0 = *event_id;
                if !matches!(&entry.1, Some(existing) if existing == actor) {
                    entry.1 = None;
                }
                entry.2.push(event.clone());
            }
        }
        for (wave_id, (event_id, author, events_for_wave)) in &wave_events {
            if let Err(e) = wave_vcs::commit_events_with_author_in_tx(
                &mut tx,
                wave_id,
                author.as_ref(),
                *event_id,
                events_for_wave,
                wave_vcs::MANIFEST_SCHEMA_VERSION,
            )
            .await
            {
                let _ = tx.rollback().await;
                return Err(e);
            }
        }
        tx.commit().await?;
        for (id, (actor, scope, event)) in event_ids.iter().zip(events) {
            bus.emit_envelope(BroadcastEnvelope {
                id: *id,
                event_version: SYNC_EVENT_VERSION,
                actor,
                scope,
                event,
            });
        }
        Ok(event_ids)
    }

    async fn log_pure_event(
        &self,
        actor: ActorId,
        scope: EventScope,
        correlation: Option<&str>,
        bus: &EventBus,
        card_role_cache: &CardRoleCache,
        wave_cove_cache: &WaveCoveCache,
        event: Event,
    ) -> Result<i64> {
        // BEGIN IMMEDIATE takes the writer lock at tx start; deferred SELECT-then-UPDATE upgrades can hit SQLITE_BUSY_SNAPSHOT, which busy_timeout does not cover.
        let mut tx = begin_immediate_tx(&self.pool).await?;
        // PR3 (#136) — gate. Pure events don't have an entity write to
        // populate the cache from, so the role lookup uses the cache's
        // current contents. `log_pure_event` callers (codex hook
        // ingest, plugin state transitions) always supply a real actor
        // identity; the gate's defense-in-depth checks (empty
        // CardId, unknown card) still apply.
        if let Err(violation) =
            crate::role_gate::enforce_role(&actor, &event, &scope, card_role_cache, wave_cove_cache)
        {
            let _ = tx.rollback().await;
            return Err(CalmError::Forbidden(violation.to_string()));
        }
        let event_id =
            match Self::event_append_in_tx(&mut tx, &actor, &scope, correlation, &event).await {
                Ok(id) => id,
                Err(e) => {
                    let _ = tx.rollback().await;
                    return Err(e);
                }
            };
        if let Some(wave_id) = scope.wave_id()
            && let Err(e) = wave_vcs::commit_in_tx(
                &mut tx,
                wave_id,
                &actor,
                event_id,
                &event,
                wave_vcs::MANIFEST_SCHEMA_VERSION,
            )
            .await
        {
            let _ = tx.rollback().await;
            return Err(e);
        }
        tx.commit().await?;
        bus.emit_envelope(BroadcastEnvelope {
            id: event_id,
            event_version: SYNC_EVENT_VERSION,
            actor,
            scope,
            event,
        });
        Ok(event_id)
    }

    /// Issue #310 — event-less tx wrapper. Runs the caller-supplied
    /// closure inside one sqlx transaction; commits on `Ok(())`, rolls
    /// back on `Err(_)`. No event row is appended to the `events` log;
    /// no broadcast is emitted. The caller is responsible for
    /// broadcasting any downstream event via `log_pure_event` after
    /// this returns. See [`crate::db::WriteInTxFn`] for the rationale.
    async fn write_in_tx(&self, f: WriteInTxFn<'_>) -> Result<()> {
        // BEGIN IMMEDIATE takes the writer lock at tx start; deferred SELECT-then-UPDATE upgrades can hit SQLITE_BUSY_SNAPSHOT, which busy_timeout does not cover.
        let mut tx = begin_immediate_tx(&self.pool).await?;
        let fut: BoxFuture<'_, Result<()>> = f(&mut tx);
        match fut.await {
            Ok(()) => {}
            Err(e) => {
                let _ = tx.rollback().await;
                return Err(e);
            }
        }
        tx.commit().await?;
        Ok(())
    }

    async fn events_since(
        &self,
        since_id: i64,
        limit: Option<i64>,
    ) -> Result<Vec<(i64, u32, EventScope, Event)>> {
        // `LIMIT -1` is sqlite's "no limit" sentinel; using `?` binding lets
        // us keep one SQL string regardless of caller intent. Callers that
        // pass `None` want every row > since_id.
        let cap = limit.unwrap_or(-1);
        // `event_version` is selected so the replay path can stamp the
        // envelope with the version persisted on the row, not the current
        // `SYNC_EVENT_VERSION` constant — old rows that predate migration
        // 0006 backfill to `1` via the column default, and any future row
        // written under a newer envelope schema must round-trip its own
        // version, not the kernel's.
        //
        // `scope_*` columns (migration 0007) reconstruct the typed
        // `EventScope`. Rows that predate the migration carry
        // `scope_kind='system'` (column default) with NULL ancestor cols,
        // which `EventScope::from_row` collapses to `EventScope::System`.
        // The same fallback covers any malformed row whose declared
        // `scope_kind` doesn't line up with its ancestor cols — replay
        // never strands a client on a malformed scope.
        type ScopeRow = (
            i64,            // id
            String,         // kind
            String,         // payload
            u32,            // event_version
            Option<String>, // scope_kind
            Option<String>, // scope_cove
            Option<String>, // scope_wave
            Option<String>, // scope_card
        );
        let rows: Vec<ScopeRow> = sqlx::query_as(
            r#"SELECT id, kind, payload, event_version,
                      scope_kind, scope_cove, scope_wave, scope_card
               FROM events
               WHERE id > ?1
               ORDER BY id ASC
               LIMIT ?2"#,
        )
        .bind(since_id)
        .bind(cap)
        .fetch_all(&self.pool)
        .await?;

        let mut out = Vec::with_capacity(rows.len());
        for (id, kind, payload_text, event_version, sk, sc, sw, scard) in rows {
            let payload: serde_json::Value = match serde_json::from_str(&payload_text) {
                Ok(v) => v,
                Err(e) => {
                    tracing::error!(
                        id, kind = %kind, error = %e,
                        "events_since: skipping row with malformed payload JSON",
                    );
                    continue;
                }
            };
            let scope = EventScope::from_row(
                sk.as_deref(),
                sc.as_deref(),
                sw.as_deref(),
                scard.as_deref(),
            );
            match Event::from_kind_and_payload(&kind, payload) {
                Ok(ev) => out.push((id, event_version, scope, ev)),
                Err(e) => {
                    tracing::error!(
                        id, kind = %kind, error = %e,
                        "events_since: skipping row that no longer matches Event enum",
                    );
                }
            }
        }
        Ok(out)
    }

    async fn events_for_wave(
        &self,
        wave_id: &str,
        kinds: &[&str],
        since_id: Option<i64>,
    ) -> Result<Vec<WaveEvent>> {
        if kinds.is_empty() {
            return Ok(Vec::new());
        }

        type ScopeRow = (
            i64,            // id
            String,         // kind
            String,         // payload
            String,         // actor
            i64,            // at
            Option<String>, // scope_kind
            Option<String>, // scope_cove
            Option<String>, // scope_wave
            Option<String>, // scope_card
        );

        let mut query = QueryBuilder::<Sqlite>::new(
            r#"SELECT id, kind, payload, actor, at,
                      scope_kind, scope_cove, scope_wave, scope_card
               FROM events
               WHERE scope_wave = "#,
        );
        query.push_bind(wave_id);
        if let Some(since_id) = since_id {
            query.push(" AND id > ");
            query.push_bind(since_id);
        }
        query.push(" AND kind IN (");
        let mut separated = query.separated(", ");
        for kind in kinds {
            separated.push_bind(*kind);
        }
        separated.push_unseparated(") ORDER BY id ASC");

        let rows: Vec<ScopeRow> = query.build_query_as().fetch_all(&self.pool).await?;

        let mut out = Vec::with_capacity(rows.len());
        for (id, kind, payload_text, actor_text, at, sk, sc, sw, scard) in rows {
            let payload: serde_json::Value = match serde_json::from_str(&payload_text) {
                Ok(v) => v,
                Err(e) => {
                    tracing::error!(
                        id, kind = %kind, error = %e,
                        "events_for_wave: skipping row with malformed payload JSON",
                    );
                    continue;
                }
            };
            let actor: ActorId = match serde_json::from_str(&actor_text) {
                Ok(v) => v,
                Err(e) => {
                    tracing::error!(
                        id, kind = %kind, error = %e,
                        "events_for_wave: skipping row with malformed actor JSON",
                    );
                    continue;
                }
            };
            let scope = EventScope::from_row(
                sk.as_deref(),
                sc.as_deref(),
                sw.as_deref(),
                scard.as_deref(),
            );
            match Event::from_kind_and_payload(&kind, payload) {
                Ok(event) => out.push(WaveEvent {
                    id,
                    at,
                    actor,
                    scope,
                    event,
                }),
                Err(e) => {
                    tracing::error!(
                        id, kind = %kind, error = %e,
                        "events_for_wave: skipping row that no longer matches Event enum",
                    );
                }
            }
        }
        Ok(out)
    }

    async fn events_earliest_id(&self) -> Result<Option<i64>> {
        // `MIN(id)` over an empty table returns a single `NULL` row. Reading
        // the column as `Option<i64>` surfaces that as `None`; non-empty
        // tables return `Some(min)`.
        let row: (Option<i64>,) = sqlx::query_as("SELECT MIN(id) FROM events")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.0)
    }

    async fn events_latest_id(&self) -> Result<Option<i64>> {
        // Mirror of `events_earliest_id`: `MAX(id)` over an empty table
        // returns a single `NULL` row, surfaced as `None` here. Used by
        // the WS handler to detect a client cursor that's ahead of the
        // server's actual log tip (see the `events_latest_id` trait
        // docstring for the reset detection contract). Issue #290.
        let row: (Option<i64>,) = sqlx::query_as("SELECT MAX(id) FROM events")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.0)
    }
}

#[cfg(test)]
mod tests {
    use super::{derive_session_identity, is_sqlite_busy_code};
    use crate::runtime_repo::RuntimeKind;
    use calm_types::worker::{SessionMode, WorkerContract, WorkerProviderKind};

    #[test]
    fn sqlite_busy_code_matches_primary_and_extended_codes() {
        for code in ["5", "6", "261", "262", "517", "SQLITE_BUSY_SNAPSHOT"] {
            assert!(is_sqlite_busy_code(code), "code {code}");
        }
        for code in ["0", "1", "SQLITE_CONSTRAINT"] {
            assert!(!is_sqlite_busy_code(code), "code {code}");
        }
    }

    #[test]
    fn derive_session_identity_frozen_table_satisfies_0045_checks() {
        let cases = [
            (
                RuntimeKind::Terminal,
                (
                    WorkerProviderKind::Terminal,
                    SessionMode::Ephemeral,
                    WorkerContract::Executor,
                ),
            ),
            (
                RuntimeKind::CodexCard,
                (
                    WorkerProviderKind::Codex,
                    SessionMode::Resumable,
                    WorkerContract::Executor,
                ),
            ),
            (
                RuntimeKind::ClaudeCard,
                (
                    WorkerProviderKind::Claude,
                    SessionMode::Ephemeral,
                    WorkerContract::Executor,
                ),
            ),
            (
                RuntimeKind::SharedSpec,
                (
                    WorkerProviderKind::Codex,
                    SessionMode::Resumable,
                    WorkerContract::Planner,
                ),
            ),
        ];

        for (kind, expected) in cases {
            let actual = derive_session_identity(&kind);
            assert_eq!(actual, expected, "kind {kind:?}");
            assert!(matches!(
                actual.0.as_db_str(),
                "codex" | "claude" | "terminal"
            ));
            assert!(matches!(actual.1.as_db_str(), "ephemeral" | "resumable"));
            assert!(matches!(
                actual.2.as_db_str(),
                "planner" | "executor" | "validator"
            ));
        }
    }
}

#[cfg(test)]
mod runtime_read_flip_tests {
    use super::*;
    use crate::model::{CardRole, NewCard, NewCove, NewWave, RequestTheme, new_id};
    use crate::runtime_repo::RuntimeRepoError;
    use serde_json::json;

    #[derive(Clone)]
    struct RuntimeReadCase {
        label: &'static str,
        card_kind: &'static str,
        kind: RuntimeKind,
        agent_provider: Option<AgentProvider>,
        status: RunStatus,
    }

    struct KeyedRuntimeSeed {
        label: &'static str,
        card_kind: &'static str,
        kind: RuntimeKind,
        agent_provider: Option<AgentProvider>,
        thread_id: Option<&'static str>,
        session_id: Option<&'static str>,
        now_ms: i64,
    }

    fn runtime_read_cases() -> Vec<RuntimeReadCase> {
        vec![
            RuntimeReadCase {
                label: "terminal",
                card_kind: "terminal",
                kind: RuntimeKind::Terminal,
                agent_provider: None,
                status: RunStatus::Starting,
            },
            RuntimeReadCase {
                label: "codex-card",
                card_kind: "codex",
                kind: RuntimeKind::CodexCard,
                agent_provider: Some(AgentProvider::Codex),
                status: RunStatus::Running,
            },
            RuntimeReadCase {
                label: "claude-card",
                card_kind: "claude",
                kind: RuntimeKind::ClaudeCard,
                agent_provider: Some(AgentProvider::Claude),
                status: RunStatus::Idle,
            },
            RuntimeReadCase {
                label: "shared-spec",
                card_kind: "codex",
                kind: RuntimeKind::SharedSpec,
                agent_provider: Some(AgentProvider::Codex),
                status: RunStatus::TurnPending,
            },
        ]
    }

    async fn fresh_repo() -> SqlxRepo {
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite repo")
    }

    async fn create_card_in_tx(
        repo: &SqlxRepo,
        tx: &mut RuntimeTx<'_>,
        label: &str,
        card_kind: &str,
    ) -> String {
        let cove = cove_create_tx(
            tx,
            NewCove {
                name: format!("read flip {label}"),
                color: "#101010".into(),
                sort: None,
            },
        )
        .await
        .expect("create cove");
        let wave = wave_create_tx(
            tx,
            NewWave {
                cove_id: cove.id,
                title: format!("read flip {label}"),
                sort: None,
                cwd: "/tmp".into(),
                attach_folder: false,
                theme: RequestTheme::default_dark(),
            },
            repo.wave_cove_cache(),
        )
        .await
        .expect("create wave");
        let card_id = format!("card-read-flip-{label}");
        let card = card_create_with_id_tx(
            tx,
            card_id,
            NewCard {
                wave_id: wave.id,
                kind: card_kind.into(),
                sort: None,
                payload: json!({"schemaVersion": 1, "case": label}),
            },
            CardRole::Worker,
            true,
            repo.card_role_cache(),
        )
        .await
        .expect("create card");
        card.id.to_string()
    }

    async fn seed_runtime(repo: &SqlxRepo, case: RuntimeReadCase, now_ms: i64) -> CardRuntime {
        let mut tx = repo.pool().begin().await.expect("begin seed tx");
        let card_id = create_card_in_tx(repo, &mut tx, case.label, case.card_kind).await;
        let runtime = runtime_start_tx(
            &mut tx,
            RuntimeInit {
                id: format!("rt-read-flip-{}", case.label),
                card_id,
                kind: case.kind,
                agent_provider: case.agent_provider,
                status: case.status,
                terminal_run_id: None,
                thread_id: Some(format!("thread-{}", case.label)),
                session_id: Some(format!("agent-session-{}", case.label)),
                active_turn_id: Some(format!("turn-{}", case.label)),
                handle_state_json: Some(json!({"case": case.label})),
                lease_owner: None,
                lease_until_ms: None,
                spawn_op_id: None,
                now_ms,
            },
        )
        .await
        .expect("start runtime");
        tx.commit().await.expect("commit seed tx");
        runtime
    }

    async fn seed_runtime_with_keys(repo: &SqlxRepo, seed: KeyedRuntimeSeed) -> CardRuntime {
        let mut tx = repo.pool().begin().await.expect("begin keyed seed tx");
        let card_id = create_card_in_tx(repo, &mut tx, seed.label, seed.card_kind).await;
        let runtime = runtime_start_tx(
            &mut tx,
            RuntimeInit {
                id: format!("rt-read-flip-{}", seed.label),
                card_id,
                kind: seed.kind,
                agent_provider: seed.agent_provider,
                status: RunStatus::Running,
                terminal_run_id: None,
                thread_id: seed.thread_id.map(str::to_string),
                session_id: seed.session_id.map(str::to_string),
                active_turn_id: Some(format!("turn-{}", seed.label)),
                handle_state_json: Some(json!({"case": seed.label})),
                lease_owner: None,
                lease_until_ms: None,
                spawn_op_id: None,
                now_ms: seed.now_ms,
            },
        )
        .await
        .expect("start keyed runtime");
        tx.commit().await.expect("commit keyed seed tx");
        runtime
    }

    async fn seed_terminal_runtime(repo: &SqlxRepo, label: &'static str) -> (CardRuntime, String) {
        let mut tx = repo.pool().begin().await.expect("begin terminal seed tx");
        let cove = cove_create_tx(
            &mut tx,
            NewCove {
                name: format!("read flip {label}"),
                color: "#101010".into(),
                sort: None,
            },
        )
        .await
        .expect("create terminal cove");
        let wave = wave_create_tx(
            &mut tx,
            NewWave {
                cove_id: cove.id,
                title: format!("read flip {label}"),
                sort: None,
                cwd: "/tmp".into(),
                attach_folder: false,
                theme: RequestTheme::default_dark(),
            },
            repo.wave_cove_cache(),
        )
        .await
        .expect("create terminal wave");
        let runtime_id = format!("rt-read-flip-{label}");
        let (_card, terminal) = card_with_terminal_create_tx(
            &mut tx,
            format!("card-read-flip-{label}"),
            &runtime_id,
            None,
            wave.id,
            None,
            "bash".into(),
            "/tmp".into(),
            json!({}),
            CardRole::Worker,
            true,
            repo.card_role_cache(),
            RequestTheme::default_dark(),
        )
        .await
        .expect("create terminal card");
        let runtime = runtime_get_by_id_tx(&mut tx, &runtime_id)
            .await
            .expect("read seeded terminal runtime")
            .expect("seeded terminal runtime exists");
        tx.commit().await.expect("commit terminal seed tx");
        (runtime, terminal.id)
    }

    struct ProjectableHistory {
        card_id: String,
        superseded: CardRuntime,
        exited: CardRuntime,
        active: Option<CardRuntime>,
    }

    fn projectable_runtime_init(
        card_id: &str,
        label: &str,
        slot: &str,
        status: RunStatus,
        now_ms: i64,
    ) -> RuntimeInit {
        RuntimeInit {
            id: format!("rt-projectable-{label}-{slot}"),
            card_id: card_id.to_string(),
            kind: RuntimeKind::CodexCard,
            agent_provider: Some(AgentProvider::Codex),
            status,
            terminal_run_id: None,
            thread_id: Some(format!("thread-{label}-{slot}")),
            session_id: Some(format!("agent-session-{label}-{slot}")),
            active_turn_id: Some(format!("turn-{label}-{slot}")),
            handle_state_json: Some(json!({"label": label, "slot": slot})),
            lease_owner: None,
            lease_until_ms: None,
            spawn_op_id: None,
            now_ms,
        }
    }

    fn deferred_projectable_placeholder_init(
        card_id: &str,
        placeholder_id: &str,
        now_ms: i64,
    ) -> RuntimeInit {
        RuntimeInit {
            id: placeholder_id.to_string(),
            card_id: card_id.to_string(),
            kind: RuntimeKind::SharedSpec,
            agent_provider: Some(AgentProvider::Codex),
            status: RunStatus::Starting,
            terminal_run_id: None,
            thread_id: None,
            session_id: None,
            active_turn_id: None,
            handle_state_json: None,
            lease_owner: None,
            lease_until_ms: None,
            spawn_op_id: None,
            now_ms,
        }
    }

    async fn seed_projectable_history(
        repo: &SqlxRepo,
        label: &'static str,
        include_active: bool,
    ) -> ProjectableHistory {
        let mut tx = repo.pool().begin().await.expect("begin projectable tx");
        let card_id = create_card_in_tx(repo, &mut tx, label, "codex").await;
        let older = runtime_start_tx(
            &mut tx,
            projectable_runtime_init(&card_id, label, "older", RunStatus::Running, 10_000),
        )
        .await
        .expect("start older runtime");
        let exited = runtime_supersede_tx(
            &mut tx,
            &older.id,
            projectable_runtime_init(&card_id, label, "exited", RunStatus::Exited, 20_000),
        )
        .await
        .expect("supersede older runtime with exited runtime");
        let superseded = runtime_get_by_id_tx(&mut tx, &older.id)
            .await
            .expect("read superseded runtime")
            .expect("superseded runtime row");
        let active = if include_active {
            Some(
                runtime_start_tx(
                    &mut tx,
                    projectable_runtime_init(&card_id, label, "active", RunStatus::Running, 30_000),
                )
                .await
                .expect("start active runtime"),
            )
        } else {
            None
        };
        tx.commit().await.expect("commit projectable tx");

        ProjectableHistory {
            card_id,
            superseded,
            exited,
            active,
        }
    }

    async fn seed_deferred_projectable_placeholder(
        repo: &SqlxRepo,
        label: &'static str,
    ) -> (String, String) {
        let placeholder_id = format!("rt-projectable-placeholder-{label}-{}", new_id());
        let mut tx = repo.pool().begin().await.expect("begin placeholder tx");
        let card_id = create_card_in_tx(repo, &mut tx, label, "codex").await;
        session_prepare_deferred_spec_tx(
            &mut tx,
            &deferred_projectable_placeholder_init(&card_id, &placeholder_id, 40_000),
        )
        .await
        .expect("prepare deferred projectable placeholder");
        tx.commit().await.expect("commit placeholder tx");
        (card_id, placeholder_id)
    }

    async fn runtime_get_projectable_for_card_from_runtimes_reference(
        pool: &SqlitePool,
        card_id: &str,
    ) -> RuntimeResult<Option<CardRuntime>> {
        let row = sqlx::query(
            r#"SELECT id, card_id, kind, agent_provider, status, terminal_run_id,
                  thread_id, session_id, active_turn_id, handle_state_json,
                  lease_owner, lease_until_ms, created_at_ms, updated_at_ms,
                  completed_at_ms
           FROM runtimes
           WHERE card_id = ?1
             AND status != 'superseded'
           ORDER BY
             CASE
                 WHEN status IN ('starting', 'running', 'idle', 'turn_pending') THEN 0
                 ELSE 1
             END ASC,
             updated_at_ms DESC, created_at_ms DESC, id DESC
           LIMIT 1"#,
        )
        .bind(card_id)
        .fetch_optional(pool)
        .await?;
        row.as_ref().map(card_runtime_from_row).transpose()
    }

    async fn runtime_get_active_by_thread_from_runtimes_reference(
        pool: &SqlitePool,
        provider: AgentProvider,
        thread_id: &str,
    ) -> RuntimeResult<Option<CardRuntime>> {
        let row = sqlx::query(
            r#"SELECT id, card_id, kind, agent_provider, status, terminal_run_id,
                  thread_id, session_id, active_turn_id, handle_state_json,
                  lease_owner, lease_until_ms, created_at_ms, updated_at_ms,
                  completed_at_ms
           FROM runtimes
           WHERE agent_provider = ?1
             AND thread_id = ?2
             AND status IN ('starting', 'running', 'idle', 'turn_pending')
           ORDER BY updated_at_ms DESC, created_at_ms DESC, id DESC
           LIMIT 1"#,
        )
        .bind(agent_provider_to_db(&provider))
        .bind(thread_id)
        .fetch_optional(pool)
        .await?;
        row.as_ref().map(card_runtime_from_row).transpose()
    }

    async fn runtime_get_active_by_session_from_runtimes_reference(
        pool: &SqlitePool,
        provider: AgentProvider,
        session_id: &str,
    ) -> RuntimeResult<Option<CardRuntime>> {
        let row = sqlx::query(
            r#"SELECT id, card_id, kind, agent_provider, status, terminal_run_id,
                  thread_id, session_id, active_turn_id, handle_state_json,
                  lease_owner, lease_until_ms, created_at_ms, updated_at_ms,
                  completed_at_ms
           FROM runtimes
           WHERE agent_provider = ?1
             AND session_id = ?2
             AND status IN ('starting', 'running', 'idle', 'turn_pending')
           ORDER BY updated_at_ms DESC, created_at_ms DESC, id DESC
           LIMIT 1"#,
        )
        .bind(agent_provider_to_db(&provider))
        .bind(session_id)
        .fetch_optional(pool)
        .await?;
        row.as_ref().map(card_runtime_from_row).transpose()
    }

    async fn runtime_active_shared_thread_attribution_from_runtimes_reference(
        pool: &SqlitePool,
    ) -> RuntimeResult<Vec<(String, String)>> {
        sqlx::query_as::<_, (String, String)>(
            r#"SELECT thread_id, card_id
           FROM runtimes
           WHERE kind IN ('shared-spec', 'codex')
             AND agent_provider = 'codex'
             AND thread_id IS NOT NULL
             AND status IN ('starting', 'running', 'idle', 'turn_pending')
           ORDER BY created_at_ms ASC, card_id ASC"#,
        )
        .fetch_all(pool)
        .await
        .map_err(Into::into)
    }

    async fn runtime_get_active_for_terminal_from_runtimes_reference_tx(
        tx: &mut RuntimeTx<'_>,
        terminal_id: &str,
    ) -> RuntimeResult<Option<CardRuntime>> {
        let row = sqlx::query(
            r#"SELECT id, card_id, kind, agent_provider, status, terminal_run_id,
                  thread_id, session_id, active_turn_id, handle_state_json,
                  lease_owner, lease_until_ms, created_at_ms, updated_at_ms,
                  completed_at_ms
           FROM runtimes
           WHERE terminal_run_id = ?1
             AND status IN ('starting', 'running', 'idle', 'turn_pending')
           ORDER BY updated_at_ms DESC, created_at_ms DESC, id DESC
           LIMIT 1"#,
        )
        .bind(terminal_id)
        .fetch_optional(&mut **tx)
        .await?;
        row.as_ref().map(card_runtime_from_row).transpose()
    }

    fn assert_ws_backed_projection(expected: &CardRuntime, actual: &CardRuntime) {
        assert_eq!(actual, expected);
        assert!(actual.terminal_ref.is_none());
        assert!(actual.lease_owner.is_none());
        assert!(actual.lease_until_ms.is_none());
        if matches!(&expected.kind, RuntimeKind::Terminal) {
            assert!(actual.agent_provider.is_none());
        }
    }

    fn assert_optional_ws_backed_projection(
        expected: Option<CardRuntime>,
        actual: Option<CardRuntime>,
    ) {
        match (expected, actual) {
            (Some(expected), Some(actual)) => assert_ws_backed_projection(&expected, &actual),
            (None, None) => {}
            (expected, actual) => panic!("runtime projection mismatch: {expected:?} != {actual:?}"),
        }
    }

    async fn assert_projectable_card_matches_runtimes_reference(
        repo: &SqlxRepo,
        history: &ProjectableHistory,
        expected_winner_id: &str,
    ) {
        let expected =
            runtime_get_projectable_for_card_from_runtimes_reference(repo.pool(), &history.card_id)
                .await
                .expect("runtimes-backed projectable reference")
                .expect("reference projectable runtime");
        assert_eq!(expected.id, expected_winner_id);

        let actual = runtime_get_projectable_for_card_from_pool(repo.pool(), &history.card_id)
            .await
            .expect("worker-session projectable read")
            .expect("projectable runtime from worker_sessions");
        assert_ws_backed_projection(&expected, &actual);
        assert_ne!(actual.id, history.superseded.id);
        assert_ne!(actual.status, RunStatus::Superseded);
    }

    #[tokio::test]
    async fn runtime_current_status_tx_matches_runtimes_backed_start_for_all_kinds() {
        let repo = fresh_repo().await;
        for (index, case) in runtime_read_cases().into_iter().enumerate() {
            let runtime = seed_runtime(&repo, case, 1_000 + index as i64).await;
            let mut tx = repo.pool().begin().await.expect("begin read tx");
            let actual = runtime_current_status_tx(&mut tx, &runtime.id)
                .await
                .expect("status from worker_sessions");
            tx.commit().await.expect("commit read tx");
            assert_eq!(actual, runtime.status, "runtime {}", runtime.id);
        }
    }

    #[tokio::test]
    async fn runtime_get_by_id_from_pool_matches_runtimes_backed_for_all_kinds() {
        let repo = fresh_repo().await;
        for (index, case) in runtime_read_cases().into_iter().enumerate() {
            let runtime = seed_runtime(&repo, case, 2_000 + index as i64).await;
            let mut tx = repo.pool().begin().await.expect("begin reference tx");
            let expected = runtime_get_by_id_tx(&mut tx, &runtime.id)
                .await
                .expect("reference by-id read")
                .expect("runtime row");
            tx.commit().await.expect("commit reference tx");

            let actual = runtime_get_by_id_from_pool(repo.pool(), &runtime.id)
                .await
                .expect("worker-session by-id read")
                .expect("runtime from worker_sessions");
            assert_ws_backed_projection(&expected, &actual);
        }
    }

    #[tokio::test]
    async fn runtime_get_active_for_card_from_pool_matches_runtimes_backed_for_all_kinds() {
        let repo = fresh_repo().await;
        for (index, case) in runtime_read_cases().into_iter().enumerate() {
            let runtime = seed_runtime(&repo, case, 3_000 + index as i64).await;
            let mut tx = repo.pool().begin().await.expect("begin reference tx");
            let expected = runtime_get_active_for_card_tx(&mut tx, &runtime.card_id)
                .await
                .expect("reference active-for-card read")
                .expect("active runtime");
            tx.commit().await.expect("commit reference tx");

            let actual = runtime_get_active_for_card_from_pool(repo.pool(), &runtime.card_id)
                .await
                .expect("worker-session active-for-card read")
                .expect("active runtime from worker_sessions");
            assert_ws_backed_projection(&expected, &actual);
        }
    }

    #[tokio::test]
    async fn runtimes_active_for_kind_from_pool_matches_runtimes_backed_for_all_kinds() {
        let repo = fresh_repo().await;
        let mut expected = Vec::new();
        for (index, case) in runtime_read_cases().into_iter().enumerate() {
            expected.push(seed_runtime(&repo, case, 4_000 + index as i64).await);
        }

        for runtime in expected {
            let actual = runtimes_active_for_kind_from_pool(repo.pool(), runtime.kind.clone())
                .await
                .expect("worker-session active-for-kind read");
            assert_eq!(
                actual.len(),
                1,
                "kind {:?} should not collapse with other contracts/providers",
                runtime.kind
            );
            assert_ws_backed_projection(&runtime, &actual[0]);
        }
    }

    #[tokio::test]
    async fn runtime_get_active_by_thread_from_pool_matches_reference_and_uses_thread_key() {
        let repo = fresh_repo().await;
        let codex = seed_runtime_with_keys(
            &repo,
            KeyedRuntimeSeed {
                label: "thread-codex",
                card_kind: "codex",
                kind: RuntimeKind::CodexCard,
                agent_provider: Some(AgentProvider::Codex),
                thread_id: Some("cohort-b-thread"),
                session_id: Some("codex-agent-session"),
                now_ms: 10_000,
            },
        )
        .await;
        seed_runtime_with_keys(
            &repo,
            KeyedRuntimeSeed {
                label: "thread-claude",
                card_kind: "claude",
                kind: RuntimeKind::ClaudeCard,
                agent_provider: Some(AgentProvider::Claude),
                thread_id: Some("claude-real-thread"),
                session_id: Some("cohort-b-thread"),
                now_ms: 20_000,
            },
        )
        .await;

        let expected = runtime_get_active_by_thread_from_runtimes_reference(
            repo.pool(),
            AgentProvider::Codex,
            "cohort-b-thread",
        )
        .await
        .expect("runtimes-backed by-thread reference");
        assert_eq!(
            expected.as_ref().map(|runtime| runtime.id.as_str()),
            Some(codex.id.as_str())
        );
        let actual = runtime_get_active_by_thread_from_pool(
            repo.pool(),
            AgentProvider::Codex,
            "cohort-b-thread",
        )
        .await
        .expect("worker-session by-thread read");
        assert_optional_ws_backed_projection(expected, actual);

        let claude_reference = runtime_get_active_by_thread_from_runtimes_reference(
            repo.pool(),
            AgentProvider::Claude,
            "cohort-b-thread",
        )
        .await
        .expect("runtimes-backed claude by-thread reference");
        let claude_actual = runtime_get_active_by_thread_from_pool(
            repo.pool(),
            AgentProvider::Claude,
            "cohort-b-thread",
        )
        .await
        .expect("worker-session claude by-thread read");
        assert_eq!(claude_reference, None);
        assert_eq!(claude_actual, None);
    }

    #[tokio::test]
    async fn runtime_get_active_by_session_from_pool_matches_reference_and_uses_agent_session_key()
    {
        let repo = fresh_repo().await;
        let claude = seed_runtime_with_keys(
            &repo,
            KeyedRuntimeSeed {
                label: "session-claude",
                card_kind: "claude",
                kind: RuntimeKind::ClaudeCard,
                agent_provider: Some(AgentProvider::Claude),
                thread_id: Some("claude-thread-not-session"),
                session_id: Some("cohort-b-claude-session"),
                now_ms: 10_000,
            },
        )
        .await;
        seed_runtime_with_keys(
            &repo,
            KeyedRuntimeSeed {
                label: "session-codex",
                card_kind: "codex",
                kind: RuntimeKind::CodexCard,
                agent_provider: Some(AgentProvider::Codex),
                thread_id: Some("cohort-b-codex-thread"),
                session_id: Some("codex-agent-session"),
                now_ms: 20_000,
            },
        )
        .await;

        let expected = runtime_get_active_by_session_from_runtimes_reference(
            repo.pool(),
            AgentProvider::Claude,
            "cohort-b-claude-session",
        )
        .await
        .expect("runtimes-backed by-session reference");
        assert_eq!(
            expected.as_ref().map(|runtime| runtime.id.as_str()),
            Some(claude.id.as_str())
        );
        let actual = runtime_get_active_by_session_from_pool(
            repo.pool(),
            AgentProvider::Claude,
            "cohort-b-claude-session",
        )
        .await
        .expect("worker-session by-session read");
        assert_optional_ws_backed_projection(expected, actual);

        let codex_thread_reference = runtime_get_active_by_session_from_runtimes_reference(
            repo.pool(),
            AgentProvider::Codex,
            "cohort-b-codex-thread",
        )
        .await
        .expect("runtimes-backed codex by-session reference");
        let codex_thread_actual = runtime_get_active_by_session_from_pool(
            repo.pool(),
            AgentProvider::Codex,
            "cohort-b-codex-thread",
        )
        .await
        .expect("worker-session codex by-session read");
        assert_eq!(codex_thread_reference, None);
        assert_eq!(codex_thread_actual, None);
    }

    #[tokio::test]
    async fn runtime_active_shared_thread_attribution_from_pool_matches_reference_ordering() {
        let repo = fresh_repo().await;
        let shared = seed_runtime_with_keys(
            &repo,
            KeyedRuntimeSeed {
                label: "attribution-shared",
                card_kind: "codex",
                kind: RuntimeKind::SharedSpec,
                agent_provider: Some(AgentProvider::Codex),
                thread_id: Some("thread-shared"),
                session_id: Some("session-shared"),
                now_ms: 10_000,
            },
        )
        .await;
        let codex = seed_runtime_with_keys(
            &repo,
            KeyedRuntimeSeed {
                label: "attribution-codex",
                card_kind: "codex",
                kind: RuntimeKind::CodexCard,
                agent_provider: Some(AgentProvider::Codex),
                thread_id: Some("thread-codex"),
                session_id: Some("session-codex"),
                now_ms: 20_000,
            },
        )
        .await;
        seed_runtime_with_keys(
            &repo,
            KeyedRuntimeSeed {
                label: "attribution-no-thread",
                card_kind: "codex",
                kind: RuntimeKind::CodexCard,
                agent_provider: Some(AgentProvider::Codex),
                thread_id: None,
                session_id: Some("session-no-thread"),
                now_ms: 30_000,
            },
        )
        .await;
        seed_runtime_with_keys(
            &repo,
            KeyedRuntimeSeed {
                label: "attribution-claude",
                card_kind: "claude",
                kind: RuntimeKind::ClaudeCard,
                agent_provider: Some(AgentProvider::Claude),
                thread_id: Some("thread-claude"),
                session_id: Some("session-claude"),
                now_ms: 15_000,
            },
        )
        .await;

        let expected =
            runtime_active_shared_thread_attribution_from_runtimes_reference(repo.pool())
                .await
                .expect("runtimes-backed attribution reference");
        let actual = runtime_active_shared_thread_attribution_from_pool(repo.pool())
            .await
            .expect("worker-session attribution read");

        assert_eq!(
            expected,
            vec![
                ("thread-shared".to_string(), shared.card_id.clone()),
                ("thread-codex".to_string(), codex.card_id.clone()),
            ]
        );
        assert_eq!(actual, expected);
    }

    #[tokio::test]
    async fn runtime_get_active_for_terminal_tx_matches_reference_inside_tx() {
        let repo = fresh_repo().await;
        let (runtime, terminal_id) = seed_terminal_runtime(&repo, "terminal-key").await;
        let mut tx = repo.pool().begin().await.expect("begin terminal read tx");
        let expected =
            runtime_get_active_for_terminal_from_runtimes_reference_tx(&mut tx, &terminal_id)
                .await
                .expect("runtimes-backed terminal reference");
        assert_eq!(
            expected.as_ref().map(|runtime| runtime.id.as_str()),
            Some(runtime.id.as_str())
        );
        let actual = runtime_get_active_for_terminal_tx(&mut tx, &terminal_id)
            .await
            .expect("worker-session terminal read");
        tx.commit().await.expect("commit terminal read tx");
        assert_optional_ws_backed_projection(expected, actual);
    }

    #[tokio::test]
    async fn runtime_get_projectable_for_card_from_pool_matches_reference_for_active_history() {
        let repo = fresh_repo().await;
        let history = seed_projectable_history(&repo, "projectable-active", true).await;
        let active = history.active.as_ref().expect("active runtime");

        assert_eq!(history.superseded.status, RunStatus::Superseded);
        assert_eq!(history.exited.status, RunStatus::Exited);
        assert_projectable_card_matches_runtimes_reference(&repo, &history, &active.id).await;
    }

    #[tokio::test]
    async fn runtime_get_projectable_for_card_from_pool_matches_reference_without_active_history() {
        let repo = fresh_repo().await;
        let history = seed_projectable_history(&repo, "projectable-no-active", false).await;

        assert_eq!(history.superseded.status, RunStatus::Superseded);
        assert!(history.active.is_none());
        assert_projectable_card_matches_runtimes_reference(&repo, &history, &history.exited.id)
            .await;
    }

    #[tokio::test]
    async fn runtime_get_projectable_for_card_from_pool_skips_deferred_spec_placeholder() {
        let repo = fresh_repo().await;
        let (card_id, _placeholder_id) =
            seed_deferred_projectable_placeholder(&repo, "projectable-placeholder").await;

        let expected =
            runtime_get_projectable_for_card_from_runtimes_reference(repo.pool(), &card_id)
                .await
                .expect("runtimes-backed projectable reference");
        assert_eq!(expected, None);

        let actual = runtime_get_projectable_for_card_from_pool(repo.pool(), &card_id)
            .await
            .expect("worker-session projectable read");
        assert_eq!(actual, None);
    }

    // This pins the intended Hole-1 / deferred-spec "eager session row, no
    // card-fallback window" behavior (PR7 + PR9a): the flipped pool read
    // deliberately returns None for the runtime-less placeholder even when a
    // pre-existing active runtime exists. This is safe because the double-spawn
    // gate uses the unflipped in-tx runtime_get_active_for_card_tx; the
    // divergence self-retires at PR9b.
    #[tokio::test]
    async fn projectable_deferred_spec_gap_with_active_runtime_returns_none_by_design() {
        let repo = fresh_repo().await;
        let label = "projectable-gap";
        let placeholder_id = format!("rt-projectable-placeholder-{label}-{}", new_id());
        let mut tx = repo.pool().begin().await.expect("begin gap tx");
        let card_id = create_card_in_tx(&repo, &mut tx, label, "codex").await;
        let mut active_init =
            projectable_runtime_init(&card_id, label, "active", RunStatus::Running, 30_000);
        active_init.kind = RuntimeKind::SharedSpec;
        let active = runtime_start_tx(&mut tx, active_init)
            .await
            .expect("start active shared-spec runtime");
        session_prepare_deferred_spec_tx(
            &mut tx,
            &deferred_projectable_placeholder_init(&card_id, &placeholder_id, 40_000),
        )
        .await
        .expect("prepare deferred projectable placeholder");
        tx.commit().await.expect("commit gap tx");

        let flipped = runtime_get_projectable_for_card_from_pool(repo.pool(), &card_id)
            .await
            .expect("worker-session projectable read");
        assert_eq!(flipped, None);

        let batch = runtime_get_projectable_for_cards_from_pool(
            repo.pool(),
            std::slice::from_ref(&card_id),
        )
        .await
        .expect("worker-session batch projectable read");
        assert!(!batch.contains_key(&card_id));

        let reference =
            runtime_get_projectable_for_card_from_runtimes_reference(repo.pool(), &card_id)
                .await
                .expect("runtimes-backed projectable reference")
                .expect("pre-existing active runtime remains projectable");
        assert_eq!(reference.id, active.id);
        assert_eq!(reference.status, RunStatus::Running);
    }

    #[tokio::test]
    async fn runtime_get_projectable_for_cards_from_pool_matches_reference_for_pointer_histories() {
        let repo = fresh_repo().await;
        let active_history =
            seed_projectable_history(&repo, "projectable-batch-active", true).await;
        let no_active_history =
            seed_projectable_history(&repo, "projectable-batch-no-active", false).await;
        let (placeholder_card_id, _placeholder_id) =
            seed_deferred_projectable_placeholder(&repo, "projectable-batch-placeholder").await;
        let active = active_history.active.as_ref().expect("active runtime");

        let card_ids = vec![
            active_history.card_id.clone(),
            no_active_history.card_id.clone(),
            placeholder_card_id.clone(),
        ];
        let actual = runtime_get_projectable_for_cards_from_pool(repo.pool(), &card_ids)
            .await
            .expect("worker-session batch projectable read");

        assert_eq!(actual.len(), 2);
        let expected_active = runtime_get_projectable_for_card_from_runtimes_reference(
            repo.pool(),
            &active_history.card_id,
        )
        .await
        .expect("active runtimes-backed reference")
        .expect("active reference runtime");
        let expected_no_active = runtime_get_projectable_for_card_from_runtimes_reference(
            repo.pool(),
            &no_active_history.card_id,
        )
        .await
        .expect("no-active runtimes-backed reference")
        .expect("no-active reference runtime");

        assert_eq!(expected_active.id, active.id);
        assert_eq!(expected_no_active.id, no_active_history.exited.id);
        assert_ws_backed_projection(
            &expected_active,
            actual
                .get(&active_history.card_id)
                .expect("active card batch runtime"),
        );
        assert_ws_backed_projection(
            &expected_no_active,
            actual
                .get(&no_active_history.card_id)
                .expect("no-active card batch runtime"),
        );
        assert!(!actual.contains_key(&placeholder_card_id));
        assert!(
            actual
                .values()
                .all(|runtime| runtime.id != active_history.superseded.id
                    && runtime.id != no_active_history.superseded.id
                    && runtime.status != RunStatus::Superseded)
        );
    }

    #[tokio::test]
    async fn worker_session_backed_reads_skip_deferred_spec_placeholder_without_runtime_row() {
        let repo = fresh_repo().await;
        let placeholder_id = format!("rt-read-flip-placeholder-{}", new_id());
        let mut tx = repo.pool().begin().await.expect("begin placeholder tx");
        let card_id = create_card_in_tx(&repo, &mut tx, "placeholder", "codex").await;
        session_prepare_deferred_spec_tx(
            &mut tx,
            &RuntimeInit {
                id: placeholder_id.clone(),
                card_id: card_id.clone(),
                kind: RuntimeKind::SharedSpec,
                agent_provider: Some(AgentProvider::Codex),
                status: RunStatus::Starting,
                terminal_run_id: None,
                thread_id: None,
                session_id: None,
                active_turn_id: None,
                handle_state_json: None,
                lease_owner: None,
                lease_until_ms: None,
                spawn_op_id: None,
                now_ms: 5_000,
            },
        )
        .await
        .expect("prepare deferred placeholder");
        tx.commit().await.expect("commit placeholder tx");

        let by_id = runtime_get_by_id_from_pool(repo.pool(), &placeholder_id)
            .await
            .expect("by-id read");
        assert_eq!(by_id, None);

        let active_for_card = runtime_get_active_for_card_from_pool(repo.pool(), &card_id)
            .await
            .expect("active-for-card read");
        assert_eq!(active_for_card, None);

        let active_for_kind =
            runtimes_active_for_kind_from_pool(repo.pool(), RuntimeKind::SharedSpec)
                .await
                .expect("active-for-kind read");
        assert_eq!(active_for_kind, Vec::<CardRuntime>::new());

        let mut tx = repo.pool().begin().await.expect("begin status tx");
        let err = runtime_current_status_tx(&mut tx, &placeholder_id)
            .await
            .expect_err("placeholder has no runtime row");
        tx.commit().await.expect("commit status tx");
        match err {
            RuntimeRepoError::Message { message } => {
                assert_eq!(message, format!("runtime {placeholder_id} not found"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn deferred_spec_placeholder_rejects_non_null_session_id() {
        let repo = fresh_repo().await;
        let placeholder_id = format!("rt-placeholder-session-key-{}", new_id());
        let mut tx = repo.pool().begin().await.expect("begin placeholder tx");
        let card_id = create_card_in_tx(&repo, &mut tx, "placeholder-session-key", "codex").await;
        let mut init = deferred_projectable_placeholder_init(&card_id, &placeholder_id, 5_000);
        init.session_id = Some("future-placeholder-session".to_string());

        let err = session_prepare_deferred_spec_tx(&mut tx, &init)
            .await
            .expect_err("non-null session_id must be rejected");
        tx.commit().await.expect("commit placeholder tx");

        match err {
            RuntimeRepoError::Message { message } => assert_eq!(
                message,
                "deferred spec session placeholders must not have a thread, terminal run, or session"
            ),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn cohort_b_reads_exclude_deferred_spec_placeholder_by_null_keys() {
        let repo = fresh_repo().await;
        let placeholder_id = format!("rt-cohort-b-placeholder-{}", new_id());
        let mut tx = repo.pool().begin().await.expect("begin placeholder tx");
        let card_id = create_card_in_tx(&repo, &mut tx, "cohort-b-placeholder", "codex").await;
        session_prepare_deferred_spec_tx(
            &mut tx,
            &RuntimeInit {
                id: placeholder_id.clone(),
                card_id,
                kind: RuntimeKind::SharedSpec,
                agent_provider: Some(AgentProvider::Codex),
                status: RunStatus::Starting,
                terminal_run_id: None,
                thread_id: None,
                session_id: None,
                active_turn_id: None,
                handle_state_json: None,
                lease_owner: None,
                lease_until_ms: None,
                spawn_op_id: None,
                now_ms: 5_000,
            },
        )
        .await
        .expect("prepare deferred placeholder");
        tx.commit().await.expect("commit placeholder tx");

        let runtime_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runtimes WHERE id = ?1")
            .bind(&placeholder_id)
            .fetch_one(repo.pool())
            .await
            .expect("count runtime rows");
        let session_keys: (Option<String>, Option<String>, Option<String>) = sqlx::query_as(
            r#"SELECT thread_id, agent_session_id, terminal_run_id
               FROM worker_sessions
               WHERE id = ?1"#,
        )
        .bind(&placeholder_id)
        .fetch_one(repo.pool())
        .await
        .expect("placeholder worker session");
        assert_eq!(runtime_count, 0);
        assert_eq!(session_keys, (None, None, None));

        let by_thread = runtime_get_active_by_thread_from_pool(
            repo.pool(),
            AgentProvider::Codex,
            "missing-placeholder-thread",
        )
        .await
        .expect("by-thread read");
        assert_eq!(by_thread, None);

        let by_session = runtime_get_active_by_session_from_pool(
            repo.pool(),
            AgentProvider::Claude,
            "missing-placeholder-session",
        )
        .await
        .expect("by-session read");
        assert_eq!(by_session, None);

        let attribution = runtime_active_shared_thread_attribution_from_pool(repo.pool())
            .await
            .expect("attribution read");
        assert_eq!(attribution, Vec::<(String, String)>::new());

        let mut tx = repo
            .pool()
            .begin()
            .await
            .expect("begin terminal placeholder tx");
        let by_terminal =
            runtime_get_active_for_terminal_tx(&mut tx, "missing-placeholder-terminal")
                .await
                .expect("terminal read");
        tx.commit().await.expect("commit terminal placeholder tx");
        assert_eq!(by_terminal, None);
    }
}

#[cfg(test)]
mod worker_flow_items_tests {
    //! #695 PR2 — storage-layer tests for `worker_flow_items`. Mirrors the
    //! harness-item db coverage: insert via the `_tx` free fn, list/page by
    //! card, delete-by-card, and the durability guarantee that a card delete
    //! turns `card_id` NULL (FK `ON DELETE SET NULL`) instead of cascading
    //! the row away.
    use super::{
        SqlxRepo, card_create_with_id_tx, cove_create_tx, session_insert_tx, wave_create_tx,
        worker_flow_item_insert_tx, worker_flow_items_delete_by_card_tx,
    };
    use crate::db::RepoRead;
    use crate::model::{CardRole, NewCard, NewCove, NewWave, RequestTheme};
    use calm_types::worker::{
        LivenessTag, SessionMode, WorkerContract, WorkerProviderKind, WorkerSession,
        WorkerSessionId, WorkerSessionState,
    };

    /// Seed a real cove → wave → card chain through the typed `_tx` helpers
    /// (so the FKs target genuine rows) and return the card/wave ids.
    async fn seed_card_and_session(repo: &SqlxRepo, session_id: &str) -> (String, String) {
        let mut tx = repo.pool().begin().await.unwrap();
        let cove = cove_create_tx(
            &mut tx,
            NewCove {
                name: "c".into(),
                color: "#fff".into(),
                sort: None,
            },
        )
        .await
        .unwrap();
        let wave = wave_create_tx(
            &mut tx,
            NewWave {
                cove_id: cove.id.clone(),
                title: "w".into(),
                sort: None,
                cwd: "/tmp".into(),
                attach_folder: false,
                theme: RequestTheme::default_dark(),
            },
            repo.wave_cove_cache(),
        )
        .await
        .unwrap();
        let card = card_create_with_id_tx(
            &mut tx,
            "card-1".into(),
            NewCard {
                wave_id: wave.id.clone(),
                kind: "worker".into(),
                sort: None,
                payload: serde_json::json!({}),
            },
            CardRole::Worker,
            true,
            repo.card_role_cache(),
        )
        .await
        .unwrap();
        session_insert_tx(
            &mut tx,
            WorkerSession {
                id: WorkerSessionId::from(session_id),
                wave_id: wave.id.clone(),
                provider: WorkerProviderKind::Codex,
                mode: SessionMode::Resumable,
                contract: WorkerContract::Executor,
                parent_session_id: None,
                requester_session_id: None,
                state: WorkerSessionState::Running,
                mcp_token_hash: None,
                thread_id: Some(format!("thread-{session_id}")),
                agent_session_id: Some(format!("agent-{session_id}")),
                active_turn_id: None,
                terminal_run_id: None,
                handle_state_json: None,
                liveness: LivenessTag::Alive,
                liveness_probed_at_ms: None,
                exit_code: None,
                exit_interpretation: None,
                spawn_op_id: None,
                last_activity_ms: None,
                last_thread_status: None,
                created_at_ms: 1,
                updated_at_ms: 1,
                completed_at_ms: None,
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        (card.id.to_string(), wave.id.to_string())
    }

    #[tokio::test]
    async fn insert_list_paging_delete_and_set_null_on_card_delete() {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let session_id = "rt-flow-item-1";
        let (card_id, wave_id) = seed_card_and_session(&repo, session_id).await;

        // Insert three flow items for the card via the `_tx` free fn.
        let mut ids = Vec::new();
        for (n, kind) in [
            (1_i64, "user_message"),
            (2, "assistant_message"),
            (3, "tool_call"),
        ] {
            let mut tx = repo.pool().begin().await.unwrap();
            let id = worker_flow_item_insert_tx(
                &mut tx,
                Some(&card_id),
                Some(session_id),
                Some(&wave_id),
                Some(session_id),
                kind,
                &format!(r#"{{"kind":"{kind}","seq":{n}}}"#),
                1_000 + n,
            )
            .await
            .unwrap();
            tx.commit().await.unwrap();
            ids.push(id);
        }

        // Ascending list returns all three in id order.
        let asc = repo
            .worker_flow_item_list_by_card(&card_id, 0, 100, false)
            .await
            .unwrap();
        assert_eq!(asc.iter().map(|r| r.id).collect::<Vec<_>>(), ids);
        assert_eq!(asc[0].kind, "user_message");
        assert_eq!(asc[0].card_id.as_deref(), Some(card_id.as_str()));
        assert_eq!(asc[0].runtime_id.as_deref(), Some(session_id));
        assert_eq!(asc[0].worker_session_id.as_deref(), Some(session_id));

        // Ascending paging: after the first id, limit 1 -> the second row.
        let page = repo
            .worker_flow_item_list_by_card(&card_id, ids[0], 1, false)
            .await
            .unwrap();
        assert_eq!(page.iter().map(|r| r.id).collect::<Vec<_>>(), vec![ids[1]]);

        // Descending: newest-first cursor (after_id = 0 -> from the tip),
        // but rows still come back in ascending id order (reversed in-fn).
        let desc = repo
            .worker_flow_item_list_by_card(&card_id, 0, 2, true)
            .await
            .unwrap();
        assert_eq!(
            desc.iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![ids[1], ids[2]]
        );

        // Durability guarantee: deleting the card must NOT destroy the rows;
        // `ON DELETE SET NULL` leaves them present with `card_id = NULL`.
        {
            let mut tx = repo.pool().begin().await.unwrap();
            super::card_delete_tx(&mut tx, &card_id, repo.card_role_cache())
                .await
                .unwrap();
            tx.commit().await.unwrap();
        }
        // The card-scoped query no longer matches (card_id is now NULL)...
        let after_card_delete = repo
            .worker_flow_item_list_by_card(&card_id, 0, 100, false)
            .await
            .unwrap();
        assert!(
            after_card_delete.is_empty(),
            "card_id should be NULL, not match"
        );
        // ...but the rows survive with NULL card_id.
        let (surviving, null_cards): (i64, i64) = sqlx::query_as(
            "SELECT COUNT(*), COUNT(*) FILTER (WHERE card_id IS NULL) FROM worker_flow_items",
        )
        .fetch_one(repo.pool())
        .await
        .unwrap();
        assert_eq!(surviving, 3, "rows must survive card delete");
        assert_eq!(null_cards, 3, "FK ON DELETE SET NULL must null card_id");
    }

    #[tokio::test]
    async fn delete_by_card_tx_purges_rows() {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let session_id = "rt-flow-item-delete";
        let (card_id, wave_id) = seed_card_and_session(&repo, session_id).await;
        for n in 1..=2 {
            let mut tx = repo.pool().begin().await.unwrap();
            worker_flow_item_insert_tx(
                &mut tx,
                Some(&card_id),
                Some(session_id),
                Some(&wave_id),
                Some(session_id),
                "user_message",
                &format!(r#"{{"seq":{n}}}"#),
                n,
            )
            .await
            .unwrap();
            tx.commit().await.unwrap();
        }
        let mut tx = repo.pool().begin().await.unwrap();
        worker_flow_items_delete_by_card_tx(&mut tx, &card_id)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let rows = repo
            .worker_flow_item_list_by_card(&card_id, 0, 100, false)
            .await
            .unwrap();
        assert!(rows.is_empty(), "explicit delete-by-card must purge rows");
    }
}

#[cfg(test)]
mod worker_flow_cursor_tests {
    use super::{SqlxRepo, card_create_with_id_tx, card_delete_tx, cove_create_tx, wave_create_tx};
    use crate::db::{RepoOutOfDomain, RepoRead};
    use crate::model::{CardRole, NewCard, NewCove, NewWave, RequestTheme};

    async fn seed_card(repo: &SqlxRepo) -> String {
        let mut tx = repo.pool().begin().await.unwrap();
        let cove = cove_create_tx(
            &mut tx,
            NewCove {
                name: "c".into(),
                color: "#fff".into(),
                sort: None,
            },
        )
        .await
        .unwrap();
        let wave = wave_create_tx(
            &mut tx,
            NewWave {
                cove_id: cove.id.clone(),
                title: "w".into(),
                sort: None,
                cwd: "/tmp".into(),
                attach_folder: false,
                theme: RequestTheme::default_dark(),
            },
            repo.wave_cove_cache(),
        )
        .await
        .unwrap();
        let card = card_create_with_id_tx(
            &mut tx,
            "card-cursor".into(),
            NewCard {
                wave_id: wave.id.clone(),
                kind: "worker".into(),
                sort: None,
                payload: serde_json::json!({}),
            },
            CardRole::Worker,
            true,
            repo.card_role_cache(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        card.id.to_string()
    }

    #[tokio::test]
    async fn cursor_upsert_overwrites_allows_reset_and_cascades() {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let card_id = seed_card(&repo).await;

        repo.worker_flow_cursor_upsert(
            &card_id,
            "codex_rollout",
            "/tmp/rollout-a.jsonl",
            10,
            0,
            Some("uuid-a"),
            Some("hash-a"),
            100,
        )
        .await
        .unwrap();
        let first = repo
            .worker_flow_cursor_get(&card_id, "codex_rollout")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first.record_index, 10);
        assert_eq!(first.last_source_uuid.as_deref(), Some("uuid-a"));
        assert_eq!(first.last_line_hash.as_deref(), Some("hash-a"));

        repo.worker_flow_cursor_upsert(
            &card_id,
            "codex_rollout",
            "/tmp/rollout-b.jsonl",
            3,
            0,
            None,
            None,
            200,
        )
        .await
        .unwrap();
        let reset = repo
            .worker_flow_cursor_get(&card_id, "codex_rollout")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(reset.source_path, "/tmp/rollout-b.jsonl");
        assert_eq!(reset.record_index, 3);
        assert!(reset.last_source_uuid.is_none());
        assert!(reset.last_line_hash.is_none());
        assert_eq!(reset.updated_at_ms, 200);

        repo.worker_flow_cursor_upsert(
            &card_id,
            "codex_rollout",
            "/tmp/rollout-b.jsonl",
            14,
            0,
            Some("uuid-b"),
            Some("hash-b"),
            300,
        )
        .await
        .unwrap();
        assert_eq!(
            repo.worker_flow_cursor_get(&card_id, "codex_rollout")
                .await
                .unwrap()
                .unwrap()
                .record_index,
            14
        );

        let mut tx = repo.pool().begin().await.unwrap();
        card_delete_tx(&mut tx, &card_id, repo.card_role_cache())
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert!(
            repo.worker_flow_cursor_get(&card_id, "codex_rollout")
                .await
                .unwrap()
                .is_none(),
            "cursor must cascade with its card"
        );
    }
}

#[cfg(test)]
mod session_record_activity_tests {
    //! #741 §1.3 — storage-layer coverage for the durable codex
    //! worker-liveness feeder `session_record_activity`. Asserts the two
    //! push-fed columns land on an active session WITHOUT bumping
    //! `updated_at_ms` (worker_sessions-only, like `liveness`), and that a
    //! terminal/missing session is a benign `Ok` no-op.
    use super::{SqlxRepo, cove_create_tx, session_insert_tx, wave_create_tx};
    use crate::model::{NewCove, NewWave, RequestTheme};
    use crate::session_repo::SessionRepo;
    use calm_types::worker::{
        LivenessTag, SessionMode, WorkerContract, WorkerProviderKind, WorkerSession,
        WorkerSessionId, WorkerSessionState,
    };

    /// Seed a real cove → wave and insert one worker session in `state` with a
    /// fixed `updated_at_ms`. Returns the session id.
    async fn seed_session(
        repo: &SqlxRepo,
        session_id: &str,
        state: WorkerSessionState,
        updated_at_ms: i64,
    ) -> WorkerSessionId {
        seed_session_with_thread(repo, session_id, None, state, updated_at_ms).await
    }

    /// Like [`seed_session`] but lets the test pin a codex `thread_id` so the
    /// thread-keyed feeder path can be exercised.
    async fn seed_session_with_thread(
        repo: &SqlxRepo,
        session_id: &str,
        thread_id: Option<&str>,
        state: WorkerSessionState,
        updated_at_ms: i64,
    ) -> WorkerSessionId {
        let mut tx = repo.pool().begin().await.unwrap();
        let cove = cove_create_tx(
            &mut tx,
            NewCove {
                name: "c".into(),
                color: "#fff".into(),
                sort: None,
            },
        )
        .await
        .unwrap();
        let wave = wave_create_tx(
            &mut tx,
            NewWave {
                cove_id: cove.id.clone(),
                title: "w".into(),
                sort: None,
                cwd: "/tmp".into(),
                attach_folder: false,
                theme: RequestTheme::default_dark(),
            },
            repo.wave_cove_cache(),
        )
        .await
        .unwrap();
        let id = WorkerSessionId::from(session_id);
        let completed_at_ms = state.is_terminal().then_some(updated_at_ms);
        session_insert_tx(
            &mut tx,
            WorkerSession {
                id: id.clone(),
                wave_id: wave.id.clone(),
                provider: WorkerProviderKind::Codex,
                mode: SessionMode::Resumable,
                contract: WorkerContract::Executor,
                parent_session_id: None,
                requester_session_id: None,
                state,
                mcp_token_hash: None,
                thread_id: thread_id.map(str::to_string),
                agent_session_id: None,
                active_turn_id: None,
                terminal_run_id: None,
                handle_state_json: None,
                liveness: LivenessTag::Alive,
                liveness_probed_at_ms: None,
                exit_code: None,
                exit_interpretation: None,
                spawn_op_id: None,
                last_activity_ms: None,
                last_thread_status: None,
                created_at_ms: 1,
                updated_at_ms,
                completed_at_ms,
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        id
    }

    #[tokio::test]
    async fn records_activity_on_active_session_without_bumping_updated_at_ms() {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let updated_at_ms = 1_000;
        let id = seed_session(
            &repo,
            "ws-active",
            WorkerSessionState::Running,
            updated_at_ms,
        )
        .await;

        // Pre-condition: both new columns start NULL.
        let before = repo.session_get(&id).await.unwrap().unwrap();
        assert!(before.last_activity_ms.is_none());
        assert!(before.last_thread_status.is_none());

        repo.session_record_activity(&id, 5_000, "active")
            .await
            .unwrap();

        let after = repo.session_get(&id).await.unwrap().unwrap();
        assert_eq!(after.last_activity_ms, Some(5_000));
        assert_eq!(after.last_thread_status.as_deref(), Some("active"));
        // The crux: ws-only columns must NOT touch updated_at_ms (parity).
        assert_eq!(
            after.updated_at_ms, updated_at_ms,
            "session_record_activity must not bump updated_at_ms"
        );
    }

    #[tokio::test]
    async fn record_activity_on_terminal_session_is_benign_noop() {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let updated_at_ms = 2_000;
        let id = seed_session(
            &repo,
            "ws-exited",
            WorkerSessionState::Exited,
            updated_at_ms,
        )
        .await;

        // Terminal session: Ok, but no columns change.
        repo.session_record_activity(&id, 9_000, "idle")
            .await
            .unwrap();

        let after = repo.session_get(&id).await.unwrap().unwrap();
        assert!(
            after.last_activity_ms.is_none(),
            "terminal session must not record activity"
        );
        assert!(after.last_thread_status.is_none());
        assert_eq!(after.updated_at_ms, updated_at_ms);
    }

    #[tokio::test]
    async fn record_activity_on_missing_session_is_ok() {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let missing = WorkerSessionId::from("ws-nope");
        // Missing row: 0 rows affected is benign and returns Ok.
        repo.session_record_activity(&missing, 7_000, "idle")
            .await
            .unwrap();
        assert!(repo.session_get(&missing).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn records_activity_by_thread_on_active_session_without_bumping_updated_at_ms() {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let updated_at_ms = 1_000;
        let id = seed_session_with_thread(
            &repo,
            "ws-active-thread",
            Some("th-active"),
            WorkerSessionState::Running,
            updated_at_ms,
        )
        .await;

        repo.session_record_activity_by_thread("th-active", 5_000, "waitingOnUserInput")
            .await
            .unwrap();

        let after = repo.session_get(&id).await.unwrap().unwrap();
        assert_eq!(after.last_activity_ms, Some(5_000));
        assert_eq!(
            after.last_thread_status.as_deref(),
            Some("waitingOnUserInput")
        );
        // The crux: ws-only columns must NOT touch updated_at_ms (parity).
        assert_eq!(
            after.updated_at_ms, updated_at_ms,
            "session_record_activity_by_thread must not bump updated_at_ms"
        );
    }

    #[tokio::test]
    async fn record_activity_by_thread_on_terminal_session_is_benign_noop() {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let updated_at_ms = 2_000;
        let id = seed_session_with_thread(
            &repo,
            "ws-exited-thread",
            Some("th-exited"),
            WorkerSessionState::Exited,
            updated_at_ms,
        )
        .await;

        // Terminal session: Ok, but no columns change.
        repo.session_record_activity_by_thread("th-exited", 9_000, "idle")
            .await
            .unwrap();

        let after = repo.session_get(&id).await.unwrap().unwrap();
        assert!(
            after.last_activity_ms.is_none(),
            "terminal session must not record activity by thread"
        );
        assert!(after.last_thread_status.is_none());
        assert_eq!(after.updated_at_ms, updated_at_ms);
    }

    #[tokio::test]
    async fn record_activity_by_thread_on_unknown_thread_is_ok() {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        // Seed an active session under a *different* thread id; the unknown
        // thread must not touch it and must return Ok.
        let id = seed_session_with_thread(
            &repo,
            "ws-other-thread",
            Some("th-known"),
            WorkerSessionState::Running,
            3_000,
        )
        .await;

        repo.session_record_activity_by_thread("th-unknown", 7_000, "active")
            .await
            .unwrap();

        let after = repo.session_get(&id).await.unwrap().unwrap();
        assert!(
            after.last_activity_ms.is_none(),
            "unknown thread must not bleed onto another session"
        );
        assert!(after.last_thread_status.is_none());
    }
}
