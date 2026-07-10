use sqlx::Row;
use sqlx::Sqlite;
use sqlx::SqlitePool;
use sqlx::Transaction;
use sqlx::sqlite::SqliteRow;

use crate::error::{CalmError, Result};
use crate::ids::{CardId, WaveId};
use crate::model::*;
use crate::session_projection_repo::{
    AgentProvider, WorkerSessionKind, WorkerSessionProjectionRepoError,
};
use crate::session_repo::Tx as SessionTx;
use calm_types::worker::{
    Liveness, LivenessTag, SessionMode, WorkerContract, WorkerProviderKind, WorkerSession,
    WorkerSessionId, WorkerSessionState,
};

pub(super) enum WorkerSessionDeleteScope<'a> {
    Wave { wave_id: &'a str },
    Card { card_id: &'a str },
}

pub(super) async fn clear_wave_root_session_refs_for_worker_session_delete_tx(
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

pub(super) fn agent_provider_to_db(provider: &AgentProvider) -> &'static str {
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

pub(super) fn runtime_message(message: impl Into<String>) -> WorkerSessionProjectionRepoError {
    WorkerSessionProjectionRepoError::Message {
        message: message.into(),
    }
}

pub(super) fn runtime_status_transition_allowed(
    from: &WorkerSessionState,
    to: &WorkerSessionState,
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

pub(super) async fn session_state_transition_at_tx(
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
