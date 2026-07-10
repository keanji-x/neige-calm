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
use sqlx::ConnectOptions;
use sqlx::Executor;
use sqlx::Row;
use sqlx::Sqlite;
use sqlx::SqlitePool;
use sqlx::Transaction;
use sqlx::sqlite::SqliteRow;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::collections::HashMap;
use std::str::FromStr;

use super::{
    Repo, RepoOutOfDomain, RepoRead, RepoSyncDomainRaw, SessionCardIdentity,
    SharedCodexDaemonRecord, SharedCodexDaemonUpdate, WorkspaceLease,
};
use crate::card_role_cache::CardRoleCache;
use crate::error::{CalmError, Result};
use crate::ids::{CardId, CoveId, WaveId};
use crate::model::*;
use crate::session_projection_repo::{
    AgentProvider, CardId as RuntimeCardId, Result as WorkerSessionProjectionResult, RuntimeId,
    ThreadAttribution, Tx as WorkerSessionProjectionTx, WorkerSessionInit, WorkerSessionKind,
    WorkerSessionProjection, WorkerSessionProjectionRepo, WorkerSessionProjectionRepoError,
};
use crate::session_projection_row::{
    WS_BACKED_CARD_RUNTIME_SELECT, WS_CARD_KEYED_RUNTIME_SELECT, card_runtime_from_ws_join_row,
    projectable_runtimes_for_cards_from_rows, projectable_runtimes_for_cards_query,
    run_status_from_db,
};
use crate::session_repo::{CommitExitOutcome, DeadRootCandidate, SessionRepo, Tx as SessionTx};
use crate::wave_cove_cache::WaveCoveCache;
use crate::wave_vcs;
use calm_types::worker::{
    Liveness, LivenessTag, SessionMode, WorkerContract, WorkerProviderKind, WorkerSession,
    WorkerSessionId, WorkerSessionState,
};

mod card;
mod card_composite;
mod cove;
mod events;
mod infra;
mod overlay;
mod task;
mod wave;

pub use card::{
    card_body_crdt_get_tx, card_create_tx, card_create_with_id_tx, card_delete_tx, card_update_tx,
    card_update_with_crdt_tx, terminal_create_tx, terminal_delete_tx, terminal_get_by_card_tx,
};
pub use card_composite::{
    card_mcp_token_set_tx, card_with_claude_create_tx, card_with_claude_worker_create_tx,
    card_with_codex_create_tx, card_with_terminal_create_tx, card_with_terminal_rollback_tx,
};
pub use cove::{
    cove_create_system_tx, cove_create_tx, cove_delete_tx, cove_folder_create_tx,
    cove_folders_list_all_tx, cove_update_tx,
};
pub use events::{append_decision_event_in_tx, append_decision_events_in_tx};
pub use infra::{begin_immediate_tx, is_sqlite_busy};
pub use overlay::{
    overlay_delete_by_entity_tx, overlay_delete_card_overlays_by_wave_tx,
    overlay_delete_subtree_by_cove_tx, overlay_delete_tx, overlay_upsert_tx,
};
pub use task::{
    SuccessReportFlip, TaskReporter, require_wave_exists_tx, task_apply_gate_result_tx,
    task_cancel_tx, task_claim_pending_tx, task_complete_from_worker_tx, task_fail_from_worker_tx,
    task_gate_attempt_bump_tx, task_get_tx, task_insert_tx, task_mark_running_tx,
    task_report_success_from_worker_tx, task_stamp_missing_running_deadline_tx,
    task_start_verifying_from_worker_tx, task_update_pending_tx, tasks_by_wave_tx,
    wave_lifecycle_and_budget_tx, wave_require_task_gates_tx, worker_op_targets_card_tx,
};
pub use wave::{wave_create_tx, wave_delete_tx, wave_update_tx};

use infra::check_no_unknown_future_migrations;
use task::TASK_COLUMNS;

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

    /// #234 — borrow the repo's wave→cove cache. Mirrors
    /// [`card_role_cache`](Self::card_role_cache). `AppState::new`
    /// re-seeds its own clone from the same pool.
    pub fn wave_cove_cache(&self) -> &WaveCoveCache {
        &self.wave_cove_cache
    }
}

pub async fn assert_worker_sessions_card_id_complete(pool: &SqlitePool) -> Result<()> {
    let count: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*) FROM worker_sessions
            WHERE card_id IS NULL
              AND state IN ('starting','running','idle','turn_pending')"#,
    )
    .fetch_one(pool)
    .await?;

    if count > 0 {
        return Err(CalmError::Internal(format!(
            "worker_sessions.card_id boot assertion failed: {count} active worker_sessions rows have NULL card_id"
        )));
    }

    Ok(())
}

impl Repo for SqlxRepo {
    fn sqlite_pool(&self) -> Option<SqlitePool> {
        Some(self.pool.clone())
    }
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
                        SELECT id FROM worker_sessions WHERE card_id = ?1
                    )"#,
            )
            .bind(card_id)
            .execute(&mut **tx)
            .await?;
        }
    }
    Ok(())
}

/// PR6b (#679) — mirror the per-card MCP hash onto the same-id worker_sessions
/// row. POPULATE-ONLY: never read for authz (the handshake reads
/// card_mcp_tokens). Fail-closed: the same-id mirror row MUST exist
/// (created by session_start_runtime_tx -> session_start_mirror_tx in the same spawn);
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
                  agent_session_id, active_turn_id, terminal_run_id, card_id,
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
                  agent_session_id, active_turn_id, terminal_run_id, card_id,
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

fn agent_provider_to_db(provider: &AgentProvider) -> &'static str {
    match provider {
        AgentProvider::Codex => "codex",
        AgentProvider::Claude => "claude",
    }
}

// PR3b-i (#679): derives the provisional NOT NULL worker-session identity
// from the runtime row's own kind. PR6 overwrites these in place at mint.
pub(crate) fn derive_session_identity(
    kind: &WorkerSessionKind,
) -> (WorkerProviderKind, SessionMode, WorkerContract) {
    let provider = match kind {
        WorkerSessionKind::Terminal => WorkerProviderKind::Terminal,
        WorkerSessionKind::CodexCard | WorkerSessionKind::SharedSpec => WorkerProviderKind::Codex,
        WorkerSessionKind::ClaudeCard => WorkerProviderKind::Claude,
    };
    let mode = match provider {
        WorkerProviderKind::Codex => SessionMode::Resumable,
        WorkerProviderKind::Claude | WorkerProviderKind::Terminal => SessionMode::Ephemeral,
    };
    let contract = match kind {
        WorkerSessionKind::SharedSpec => WorkerContract::Planner,
        _ => WorkerContract::Executor,
    };
    (provider, mode, contract)
}

fn runtime_message(message: impl Into<String>) -> WorkerSessionProjectionRepoError {
    WorkerSessionProjectionRepoError::Message {
        message: message.into(),
    }
}

fn runtime_status_transition_allowed(from: &WorkerSessionState, to: &WorkerSessionState) -> bool {
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
        card_id: row.try_get::<Option<String>, _>("card_id")?.map(CardId),
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
                  agent_session_id, active_turn_id, terminal_run_id, card_id,
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
/// Like `session_set_liveness_tx` these are observation columns on
/// `worker_sessions`, so this MUST NOT touch `updated_at_ms`: projection reads
/// select the active session per card with `ORDER BY ws.updated_at_ms DESC`, and
/// an observation-only bump could reorder which session wins. 0 rows affected
/// is benign — the session is terminal or missing — and returns `Ok(())`.
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
/// Like [`session_record_activity_tx`] these are observation columns on
/// `worker_sessions`, so this MUST NOT touch `updated_at_ms`: projection reads
/// select the active session per card with `ORDER BY ws.updated_at_ms DESC`, and
/// an observation-only bump could reorder which session wins. The match is also
/// pinned to `provider='codex'` (thread ids are codex-scoped). 0 rows affected
/// is benign — no active codex session owns the thread — and returns `Ok(())`.
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
               completed_at_ms, card_id
           )
           VALUES (
               ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
               ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25
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
    .bind(session.card_id.as_ref().map(|c| c.0.as_str()))
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
    from: &WorkerSessionState,
    to: &WorkerSessionState,
) -> WorkerSessionProjectionResult<()> {
    if runtime_status_transition_allowed(from, to) {
        Ok(())
    } else {
        Err(WorkerSessionProjectionRepoError::IllegalStatusTransition {
            id: id.clone(),
            attempted: *to,
        })
    }
}

fn runtime_session_error(err: CalmError) -> WorkerSessionProjectionRepoError {
    runtime_message(err.to_string())
}

async fn worker_session_wave_id_for_card_tx(
    tx: &mut WorkerSessionProjectionTx<'_>,
    card_id: &str,
) -> WorkerSessionProjectionResult<WaveId> {
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

fn worker_session_from_runtime_init(init: &WorkerSessionInit, wave_id: WaveId) -> WorkerSession {
    let (provider, mode, contract) = derive_session_identity(&init.kind);
    WorkerSession {
        id: WorkerSessionId(init.id.clone()),
        wave_id,
        provider,
        mode,
        contract,
        parent_session_id: None,
        requester_session_id: None,
        state: init.status,
        mcp_token_hash: None,
        thread_id: init.thread_id.clone(),
        agent_session_id: init.session_id.clone(),
        active_turn_id: init.active_turn_id.clone(),
        terminal_run_id: init.terminal_run_id.clone(),
        card_id: Some(CardId(init.card_id.clone())),
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

async fn session_refresh_deferred_planner_tx(
    tx: &mut WorkerSessionProjectionTx<'_>,
    existing: WorkerSession,
    desired: WorkerSession,
) -> WorkerSessionProjectionResult<WorkerSession> {
    let refreshable_state = existing.state == WorkerSessionState::Starting
        || existing.state == WorkerSessionState::Superseded;
    let refreshable_completed =
        existing.completed_at_ms.is_none() || existing.state == WorkerSessionState::Superseded;
    if desired.contract != WorkerContract::Planner
        || existing.contract != WorkerContract::Planner
        || !refreshable_state
        || existing.wave_id != desired.wave_id
        || existing.provider != desired.provider
        || existing.mode != desired.mode
        || existing.parent_session_id.is_some()
        || existing.requester_session_id.is_some()
        || !refreshable_completed
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
              AND state IN ('starting', 'superseded')"#,
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
    tx: &mut WorkerSessionProjectionTx<'_>,
    session: WorkerSession,
) -> WorkerSessionProjectionResult<WorkerSession> {
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
    tx: &mut WorkerSessionProjectionTx<'_>,
    card_id: &str,
    session_id: &WorkerSessionId,
) -> WorkerSessionProjectionResult<()> {
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
    tx: &mut WorkerSessionProjectionTx<'_>,
    card_id: &str,
    session: &WorkerSession,
) -> WorkerSessionProjectionResult<()> {
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
    tx: &mut WorkerSessionProjectionTx<'_>,
    card_id: &str,
    session: &WorkerSession,
) -> WorkerSessionProjectionResult<()> {
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
    tx: &mut WorkerSessionProjectionTx<'_>,
    init: &WorkerSessionInit,
) -> WorkerSessionProjectionResult<WorkerSession> {
    let wave_id = worker_session_wave_id_for_card_tx(tx, &init.card_id).await?;
    let session = worker_session_from_runtime_init(init, wave_id);
    let session = session_insert_or_refresh_start_mirror_tx(tx, session).await?;
    session_repoint_current_links_tx(tx, &init.card_id, &session).await?;
    Ok(session)
}

pub async fn session_prepare_deferred_spec_tx(
    tx: &mut WorkerSessionProjectionTx<'_>,
    init: &WorkerSessionInit,
) -> WorkerSessionProjectionResult<WorkerSession> {
    if init.kind != WorkerSessionKind::SharedSpec || init.status != WorkerSessionState::Starting {
        return Err(runtime_message(
            "deferred spec session placeholders require a starting shared-spec runtime init",
        ));
    }
    if init.thread_id.is_some() || init.terminal_run_id.is_some() || init.session_id.is_some() {
        return Err(runtime_message(
            "deferred spec session placeholders must not have a thread, terminal run, or session",
        ));
    }
    let existing_active_id: Option<String> = sqlx::query_scalar(
        r#"SELECT ws.id
             FROM cards c
             JOIN worker_sessions ws ON ws.id = c.session_id
            WHERE c.id = ?1
              AND ws.state IN ('starting', 'running', 'idle', 'turn_pending')"#,
    )
    .bind(&init.card_id)
    .fetch_optional(&mut **tx)
    .await?;
    if let Some(existing_id) = existing_active_id {
        session_supersede_active_tx(tx, &existing_id, init.now_ms).await?;
    }
    let wave_id = worker_session_wave_id_for_card_tx(tx, &init.card_id).await?;
    let session = worker_session_from_runtime_init(init, wave_id);
    let session = session_insert_or_refresh_start_mirror_tx(tx, session).await?;
    session_repoint_current_links_tx(tx, &init.card_id, &session).await?;
    Ok(session)
}

pub async fn session_supersede_active_tx(
    tx: &mut WorkerSessionProjectionTx<'_>,
    id: &RuntimeId,
    now: i64,
) -> WorkerSessionProjectionResult<()> {
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

pub async fn session_start_runtime_tx(
    tx: &mut WorkerSessionProjectionTx<'_>,
    init: WorkerSessionInit,
) -> WorkerSessionProjectionResult<WorkerSessionProjection> {
    session_start_mirror_tx(tx, &init).await?;
    session_projection_by_id_tx(tx, &init.id)
        .await?
        .ok_or_else(|| runtime_message(format!("worker session {} missing after insert", init.id)))
}

pub async fn session_supersede_and_start_tx(
    tx: &mut WorkerSessionProjectionTx<'_>,
    old_id: &RuntimeId,
    new_init: WorkerSessionInit,
) -> WorkerSessionProjectionResult<WorkerSessionProjection> {
    session_supersede_active_tx(tx, old_id, new_init.now_ms).await?;
    session_start_runtime_tx(tx, new_init).await
}

pub async fn session_delete_tx(
    tx: &mut WorkerSessionProjectionTx<'_>,
    id: &RuntimeId,
) -> WorkerSessionProjectionResult<()> {
    sqlx::query("UPDATE waves SET root_session_id = NULL WHERE root_session_id = ?1")
        .bind(id)
        .execute(&mut **tx)
        .await?;
    sqlx::query("DELETE FROM worker_sessions WHERE id = ?1")
        .bind(id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn session_set_status_mirror_tx(
    tx: &mut WorkerSessionProjectionTx<'_>,
    id: &RuntimeId,
    status: WorkerSessionState,
    now: i64,
) -> WorkerSessionProjectionResult<()> {
    session_state_transition_at_tx(tx, &WorkerSessionId(id.clone()), status, now, None)
        .await
        .map(|_| ())
        .map_err(runtime_session_error)
}

async fn session_complete_mirror_tx(
    tx: &mut WorkerSessionProjectionTx<'_>,
    id: &RuntimeId,
    terminal_status: WorkerSessionState,
    now: i64,
) -> WorkerSessionProjectionResult<()> {
    session_state_transition_at_tx(
        tx,
        &WorkerSessionId(id.clone()),
        terminal_status,
        now,
        Some(now),
    )
    .await
    .map(|_| ())
    .map_err(runtime_session_error)
}

async fn session_bind_attribution_mirror_tx(
    tx: &mut WorkerSessionProjectionTx<'_>,
    id: &RuntimeId,
    attr: &ThreadAttribution,
    now: i64,
) -> WorkerSessionProjectionResult<()> {
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
    tx: &mut WorkerSessionProjectionTx<'_>,
    id: &RuntimeId,
    now: i64,
) -> WorkerSessionProjectionResult<()> {
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
    tx: &mut WorkerSessionProjectionTx<'_>,
    id: &RuntimeId,
    state_text: &Option<String>,
    now: i64,
) -> WorkerSessionProjectionResult<()> {
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
    tx: &mut WorkerSessionProjectionTx<'_>,
    id: &RuntimeId,
    turn_id: Option<&str>,
    now: i64,
) -> WorkerSessionProjectionResult<()> {
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
    tx: &mut WorkerSessionProjectionTx<'_>,
    id: &RuntimeId,
    status: WorkerSessionState,
    thread_id: Option<&str>,
    active_turn_id: Option<&str>,
    now: i64,
) -> WorkerSessionProjectionResult<()> {
    sqlx::query(
        r#"UPDATE worker_sessions
              SET state = ?1,
                  thread_id = COALESCE(?2, thread_id),
                  active_turn_id = ?3,
                  updated_at_ms = ?4
            WHERE id = ?5
              AND state IN ('starting', 'running', 'idle', 'turn_pending')"#,
    )
    .bind(status.as_db_str())
    .bind(thread_id)
    .bind(active_turn_id)
    .bind(now)
    .bind(id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn session_fail_if_active_tx(
    tx: &mut WorkerSessionProjectionTx<'_>,
    id: &RuntimeId,
    now: i64,
) -> WorkerSessionProjectionResult<()> {
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
    tx: &mut WorkerSessionProjectionTx<'_>,
    id: &RuntimeId,
    now: i64,
) -> WorkerSessionProjectionResult<()> {
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
    tx: &mut WorkerSessionProjectionTx<'_>,
    id: &RuntimeId,
    context: &str,
) -> WorkerSessionProjectionResult<WorkerSession> {
    session_get_tx(tx, &WorkerSessionId(id.clone()))
        .await
        .map_err(runtime_session_error)?
        .ok_or_else(|| runtime_message(format!("worker session {id} missing while {context}")))
}

async fn session_restore_from_superseded_tx(
    tx: &mut WorkerSessionProjectionTx<'_>,
    id: &RuntimeId,
    status: WorkerSessionState,
    now: i64,
) -> WorkerSessionProjectionResult<WorkerSession> {
    let state_db = status.as_db_str();
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
    tx: &mut WorkerSessionProjectionTx<'_>,
    id: &RuntimeId,
) -> WorkerSessionProjectionResult<WorkerSessionState> {
    let row = sqlx::query(
        r#"SELECT state FROM worker_sessions ws
           WHERE ws.id = ?1"#,
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
) -> WorkerSessionProjectionResult<Option<WorkerSessionProjection>> {
    let sql = format!(
        r#"{WS_BACKED_CARD_RUNTIME_SELECT}
           WHERE ws.id = ?1"#
    );
    let row = sqlx::query(&sql).bind(id).fetch_optional(pool).await?;
    row.as_ref().map(card_runtime_from_ws_join_row).transpose()
}

async fn runtime_get_active_for_card_from_pool(
    pool: &SqlitePool,
    card_id: &str,
) -> WorkerSessionProjectionResult<Option<WorkerSessionProjection>> {
    let sql = format!(
        r#"{WS_BACKED_CARD_RUNTIME_SELECT}
           WHERE c.id = ?1
             AND ws.state IN ('starting', 'running', 'idle', 'turn_pending')
           ORDER BY ws.updated_at_ms DESC, ws.created_at_ms DESC, ws.id DESC
           LIMIT 1"#,
    );
    let row = sqlx::query(&sql).bind(card_id).fetch_optional(pool).await?;
    row.as_ref().map(card_runtime_from_ws_join_row).transpose()
}

async fn runtime_get_projectable_for_card_from_pool(
    pool: &SqlitePool,
    card_id: &str,
) -> WorkerSessionProjectionResult<Option<WorkerSessionProjection>> {
    let sql = format!(
        r#"{WS_BACKED_CARD_RUNTIME_SELECT}
           WHERE c.id = ?1
             AND ws.state != 'superseded'
           LIMIT 1"#,
    );
    let row = sqlx::query(&sql).bind(card_id).fetch_optional(pool).await?;
    row.as_ref().map(card_runtime_from_ws_join_row).transpose()
}

async fn runtime_get_projectable_for_cards_from_pool(
    pool: &SqlitePool,
    card_ids: &[RuntimeCardId],
) -> WorkerSessionProjectionResult<HashMap<RuntimeCardId, WorkerSessionProjection>> {
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
) -> WorkerSessionProjectionResult<Option<WorkerSessionProjection>> {
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
) -> WorkerSessionProjectionResult<Option<WorkerSessionProjection>> {
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
) -> WorkerSessionProjectionResult<Vec<(String, String)>> {
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
    kind: WorkerSessionKind,
) -> WorkerSessionProjectionResult<Vec<WorkerSessionProjection>> {
    let (provider, _mode, contract) = derive_session_identity(&kind);
    let sql = format!(
        r#"{WS_BACKED_CARD_RUNTIME_SELECT}
           WHERE ws.provider = ?1
             AND ws.contract = ?2
             AND ws.state IN ('starting', 'running', 'idle', 'turn_pending')
           ORDER BY ws.created_at_ms ASC, c.id ASC"#
    );
    let rows = sqlx::query(&sql)
        .bind(provider.as_db_str())
        .bind(contract.as_db_str())
        .fetch_all(pool)
        .await?;
    rows.iter().map(card_runtime_from_ws_join_row).collect()
}

pub async fn session_projection_by_id_tx(
    tx: &mut WorkerSessionProjectionTx<'_>,
    id: &RuntimeId,
) -> WorkerSessionProjectionResult<Option<WorkerSessionProjection>> {
    let sql = format!(
        r#"{WS_CARD_KEYED_RUNTIME_SELECT}
           WHERE ws.id = ?1"#
    );
    let row = sqlx::query(&sql).bind(id).fetch_optional(&mut **tx).await?;
    row.as_ref().map(card_runtime_from_ws_join_row).transpose()
}

pub async fn session_projection_active_for_card_tx(
    tx: &mut WorkerSessionProjectionTx<'_>,
    card_id: &str,
) -> WorkerSessionProjectionResult<Option<WorkerSessionProjection>> {
    let sql = format!(
        r#"{WS_CARD_KEYED_RUNTIME_SELECT}
           WHERE ws.card_id = ?1
             AND ws.state IN ('starting', 'running', 'idle', 'turn_pending')
           ORDER BY ws.updated_at_ms DESC, ws.created_at_ms DESC, ws.id DESC
           LIMIT 1"#,
    );
    let row = sqlx::query(&sql)
        .bind(card_id)
        .fetch_optional(&mut **tx)
        .await?;
    row.as_ref().map(card_runtime_from_ws_join_row).transpose()
}

pub async fn session_set_status_tx(
    tx: &mut WorkerSessionProjectionTx<'_>,
    id: &RuntimeId,
    status: WorkerSessionState,
) -> WorkerSessionProjectionResult<()> {
    if status == WorkerSessionState::Superseded {
        return Err(WorkerSessionProjectionRepoError::IllegalStatusTransition {
            id: id.clone(),
            attempted: status,
        });
    }

    let current = runtime_current_status_tx(tx, id).await?;
    ensure_runtime_status_transition(id, &current, &status)?;

    let now = now_ms();
    session_set_status_mirror_tx(tx, id, status, now).await?;
    Ok(())
}

pub async fn session_set_status_for_card_tx(
    tx: &mut WorkerSessionProjectionTx<'_>,
    card_id: &str,
    status: WorkerSessionState,
) -> WorkerSessionProjectionResult<()> {
    let Some(runtime) = session_projection_active_for_card_tx(tx, card_id).await? else {
        return Ok(());
    };
    session_set_status_tx(tx, &runtime.id, status).await
}

pub async fn session_bind_attribution_tx(
    tx: &mut WorkerSessionProjectionTx<'_>,
    id: &RuntimeId,
    attr: ThreadAttribution,
) -> WorkerSessionProjectionResult<()> {
    if &attr.runtime_id != id {
        return Err(runtime_message(format!(
            "runtime attribution id mismatch: arg={id}, attr={}",
            attr.runtime_id
        )));
    }

    let now = now_ms();
    session_bind_attribution_mirror_tx(tx, id, &attr, now).await?;
    Ok(())
}

pub async fn session_clear_terminal_run_id_tx(
    tx: &mut WorkerSessionProjectionTx<'_>,
    id: &RuntimeId,
) -> WorkerSessionProjectionResult<()> {
    let now = now_ms();
    session_clear_terminal_run_id_mirror_tx(tx, id, now).await?;
    Ok(())
}

pub async fn session_set_handle_state_tx(
    tx: &mut WorkerSessionProjectionTx<'_>,
    id: &RuntimeId,
    state: Option<serde_json::Value>,
) -> WorkerSessionProjectionResult<()> {
    let state_text = state.as_ref().map(serde_json::to_string).transpose()?;
    let now = now_ms();
    session_set_handle_state_mirror_tx(tx, id, &state_text, now).await?;
    Ok(())
}

pub async fn session_set_active_turn_tx(
    tx: &mut WorkerSessionProjectionTx<'_>,
    id: &RuntimeId,
    turn_id: Option<&str>,
) -> WorkerSessionProjectionResult<()> {
    let now = now_ms();
    session_set_active_turn_mirror_tx(tx, id, turn_id, now).await?;
    Ok(())
}

/// Tolerant harness phase-mirror / compensation write; deliberately skips the
/// runtime status matrix and emits no event.
pub async fn session_set_harness_observation_runtime_tx(
    tx: &mut WorkerSessionProjectionTx<'_>,
    id: &RuntimeId,
    status: WorkerSessionState,
    thread_id: Option<&str>,
    active_turn_id: Option<&str>,
) -> WorkerSessionProjectionResult<()> {
    let now = now_ms();
    session_set_harness_observation_tx(tx, id, status, thread_id, active_turn_id, now).await?;
    Ok(())
}

/// Tolerant harness phase-mirror / compensation write; deliberately skips the
/// runtime status matrix and emits no event.
pub async fn session_fail_if_active_runtime_tx(
    tx: &mut WorkerSessionProjectionTx<'_>,
    id: &RuntimeId,
) -> WorkerSessionProjectionResult<()> {
    let now = now_ms();
    session_fail_if_active_tx(tx, id, now).await?;
    Ok(())
}

/// Tolerant harness phase-mirror / compensation write; deliberately skips the
/// runtime status matrix and emits no event.
pub async fn session_mark_superseded_runtime_tx(
    tx: &mut WorkerSessionProjectionTx<'_>,
    id: &RuntimeId,
) -> WorkerSessionProjectionResult<()> {
    let now = now_ms();
    session_mark_superseded_tx(tx, id, now).await?;
    Ok(())
}

/// Tolerant harness phase-mirror / compensation write; deliberately skips the
/// runtime status matrix and emits no event.
pub async fn session_restore_from_superseded_runtime_tx(
    tx: &mut WorkerSessionProjectionTx<'_>,
    id: &RuntimeId,
    status: WorkerSessionState,
) -> WorkerSessionProjectionResult<()> {
    let now = now_ms();
    let session = session_restore_from_superseded_tx(tx, id, status, now).await?;
    let runtime = session_projection_by_id_tx(tx, id)
        .await?
        .ok_or_else(|| runtime_message(format!("worker session {id} missing after restore")))?;
    session_repoint_current_links_tx(tx, &runtime.card_id, &session).await
}

pub async fn session_complete_tx(
    tx: &mut WorkerSessionProjectionTx<'_>,
    id: &RuntimeId,
    terminal_status: WorkerSessionState,
) -> WorkerSessionProjectionResult<()> {
    if !matches!(
        terminal_status,
        WorkerSessionState::Failed | WorkerSessionState::Exited
    ) {
        return Err(WorkerSessionProjectionRepoError::IllegalStatusTransition {
            id: id.clone(),
            attempted: terminal_status,
        });
    }

    let current = runtime_current_status_tx(tx, id).await?;
    ensure_runtime_status_transition(id, &current, &terminal_status)?;

    let now = now_ms();
    session_complete_mirror_tx(tx, id, terminal_status, now).await?;
    Ok(())
}

pub async fn session_complete_for_card_tx(
    tx: &mut WorkerSessionProjectionTx<'_>,
    card_id: &str,
    terminal_status: WorkerSessionState,
) -> WorkerSessionProjectionResult<()> {
    let Some(runtime) = session_projection_active_for_card_tx(tx, card_id).await? else {
        return Ok(());
    };
    session_complete_tx(tx, &runtime.id, terminal_status).await
}

pub async fn session_projection_active_for_terminal_tx(
    tx: &mut WorkerSessionProjectionTx<'_>,
    terminal_id: &str,
) -> WorkerSessionProjectionResult<Option<WorkerSessionProjection>> {
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

pub async fn session_complete_for_terminal_tx(
    tx: &mut WorkerSessionProjectionTx<'_>,
    terminal_id: &str,
    terminal_status: WorkerSessionState,
) -> WorkerSessionProjectionResult<()> {
    let Some(runtime) = session_projection_active_for_terminal_tx(tx, terminal_id).await? else {
        return Ok(());
    };
    session_complete_tx(tx, &runtime.id, terminal_status).await
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
            r#"SELECT id, cove_id, title, sort, archived_at, pinned_at, lifecycle, cwd, workflow_id, terminal_at, created_at, updated_at
               FROM waves WHERE cove_id = ?1 ORDER BY sort ASC"#,
        )
        .bind(cove_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Wave::from).collect())
    }

    async fn wave_get(&self, id: &str) -> Result<Option<Wave>> {
        let row = sqlx::query_as::<_, crate::db::rows::WaveRow>(
            r#"SELECT id, cove_id, title, sort, archived_at, pinned_at, lifecycle, cwd, workflow_id, terminal_at, created_at, updated_at
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
            "SELECT id, cove_id, title, sort, archived_at, pinned_at, lifecycle, cwd, workflow_id, \
             terminal_at, created_at, updated_at FROM waves",
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
            r#"SELECT id, cove_id, title, sort, archived_at, pinned_at, lifecycle, cwd, workflow_id, terminal_at, created_at, updated_at
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
        // Orphan: this terminal's card has no active worker_session, AND the row
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
                   SELECT 1 FROM worker_sessions ws
                   WHERE ws.card_id = t.card_id
                     AND ws.state IN ('starting', 'running', 'idle', 'turn_pending')
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
        let (provider, _mode, contract) = derive_session_identity(&WorkerSessionKind::SharedSpec);
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

    async fn workspace_lease_for_card(&self, card_id: &str) -> Result<Option<WorkspaceLease>> {
        let row = sqlx::query(
            r#"SELECT lease_id, card_id, wave_id, path, state
               FROM workspace_leases
               WHERE card_id = ?1
                 AND state = 'held'
               ORDER BY created_at_ms DESC, lease_id DESC
               LIMIT 1"#,
        )
        .bind(card_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| {
            Ok(WorkspaceLease {
                lease_id: row.try_get("lease_id")?,
                card_id: row.try_get("card_id")?,
                wave_id: row.try_get("wave_id")?,
                path: row.try_get("path")?,
                state: row.try_get("state")?,
            })
        })
        .transpose()
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
impl WorkerSessionProjectionRepo for SqlxRepo {
    async fn session_projection_active_by_thread(
        &self,
        provider: AgentProvider,
        thread_id: &str,
    ) -> WorkerSessionProjectionResult<Option<WorkerSessionProjection>> {
        runtime_get_active_by_thread_from_pool(&self.pool, provider, thread_id).await
    }

    async fn session_projection_active_by_session(
        &self,
        provider: AgentProvider,
        session_id: &str,
    ) -> WorkerSessionProjectionResult<Option<WorkerSessionProjection>> {
        runtime_get_active_by_session_from_pool(&self.pool, provider, session_id).await
    }

    async fn session_projection_active_for_card(
        &self,
        card_id: &crate::session_projection_repo::CardId,
    ) -> WorkerSessionProjectionResult<Option<WorkerSessionProjection>> {
        runtime_get_active_for_card_from_pool(&self.pool, card_id).await
    }

    async fn session_projection_projectable_for_card(
        &self,
        card_id: &crate::session_projection_repo::CardId,
    ) -> WorkerSessionProjectionResult<Option<WorkerSessionProjection>> {
        runtime_get_projectable_for_card_from_pool(&self.pool, card_id).await
    }

    async fn session_projection_projectable_for_cards(
        &self,
        card_ids: &[crate::session_projection_repo::CardId],
    ) -> WorkerSessionProjectionResult<
        HashMap<crate::session_projection_repo::CardId, WorkerSessionProjection>,
    > {
        runtime_get_projectable_for_cards_from_pool(&self.pool, card_ids).await
    }

    async fn session_projection_active_shared_thread_attribution(
        &self,
    ) -> WorkerSessionProjectionResult<Vec<(String, String)>> {
        runtime_active_shared_thread_attribution_from_pool(&self.pool).await
    }

    async fn session_projection_active_for_kind(
        &self,
        kind: WorkerSessionKind,
    ) -> WorkerSessionProjectionResult<Vec<WorkerSessionProjection>> {
        runtimes_active_for_kind_from_pool(&self.pool, kind).await
    }

    async fn session_projection_by_id(
        &self,
        id: &RuntimeId,
    ) -> WorkerSessionProjectionResult<Option<WorkerSessionProjection>> {
        runtime_get_by_id_from_pool(&self.pool, id).await
    }

    async fn session_projection_set_status_for_card(
        &self,
        card_id: &str,
        status: WorkerSessionState,
    ) -> WorkerSessionProjectionResult<()> {
        let mut tx = self.pool.begin().await?;
        session_set_status_for_card_tx(&mut tx, card_id, status).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn session_projection_complete_for_card(
        &self,
        card_id: &str,
        terminal_status: WorkerSessionState,
    ) -> WorkerSessionProjectionResult<()> {
        let mut tx = self.pool.begin().await?;
        session_complete_for_card_tx(&mut tx, card_id, terminal_status).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn session_projection_complete_for_terminal(
        &self,
        terminal_id: &str,
        terminal_status: WorkerSessionState,
    ) -> WorkerSessionProjectionResult<()> {
        let mut tx = self.pool.begin().await?;
        session_complete_for_terminal_tx(&mut tx, terminal_id, terminal_status).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn session_projection_recover_harnesses_on_boot(
        &self,
    ) -> WorkerSessionProjectionResult<Vec<WorkerSessionProjection>> {
        let (provider, _mode, contract) = derive_session_identity(&WorkerSessionKind::SharedSpec);
        let sql = format!(
            r#"{WS_BACKED_CARD_RUNTIME_SELECT}
               JOIN waves w ON w.id = c.wave_id
               WHERE ws.provider = ?1
                 AND ws.contract = ?2
                 AND ws.state IN ('starting','running','idle','turn_pending')
                 AND ws.thread_id IS NOT NULL
                 AND ws.handle_state_json IS NOT NULL
                 AND json_extract(ws.handle_state_json, '$.mode') = 'harness'
                 -- Keep harness boot recovery aligned with the legacy
                 -- takeover filters above: terminal waves must stay inert.
                 AND w.lifecycle NOT IN ('done', 'canceled', 'failed')
               ORDER BY ws.created_at_ms ASC, c.id ASC"#
        );
        let rows = sqlx::query(&sql)
            .bind(provider.as_db_str())
            .bind(contract.as_db_str())
            .fetch_all(&self.pool)
            .await?;
        rows.iter()
            .map(card_runtime_from_ws_join_row)
            .collect::<WorkerSessionProjectionResult<Vec<_>>>()
    }
}

fn is_session_conflict(err: &CalmError) -> bool {
    matches!(
        err,
        CalmError::Core(calm_types::error::CoreError::Conflict(_))
    )
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
                      agent_session_id, active_turn_id, terminal_run_id, card_id,
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
                      agent_session_id, active_turn_id, terminal_run_id, card_id,
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

        tx.commit().await?;
        Ok(CommitExitOutcome::Committed(session))
    }

    async fn session_list_by_wave(&self, wave_id: &WaveId) -> Result<Vec<WorkerSession>> {
        let rows = sqlx::query(
            r#"SELECT id, wave_id, provider, mode, contract, parent_session_id,
                      requester_session_id, state, mcp_token_hash, thread_id,
                      agent_session_id, active_turn_id, terminal_run_id, card_id,
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

#[cfg(test)]
mod tests;

#[cfg(test)]
mod task_liveness_deadline_tests;

#[cfg(test)]
mod workspace_lease_lookup_tests;

#[cfg(test)]
mod write_path_gate_wiring_tests;

#[cfg(test)]
mod runtime_read_flip_parity_tests;
#[cfg(test)]
mod runtime_read_flip_projection_tests;
#[cfg(test)]
mod runtime_read_flip_support;

#[cfg(test)]
mod worker_flow_items_tests;

#[cfg(test)]
mod worker_flow_cursor_tests;

#[cfg(test)]
mod session_record_activity_tests;
