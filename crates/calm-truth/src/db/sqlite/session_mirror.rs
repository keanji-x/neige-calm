use sqlx::Row;

use super::session_row::{
    runtime_message, runtime_status_transition_allowed, session_state_transition_at_tx,
};
use super::{
    derive_session_identity, session_get_tx, session_insert_tx, session_mark_wave_root_tx,
    session_mcp_token_set_tx, session_projection_by_id_tx,
};
use crate::error::CalmError;
use crate::ids::{CardId, WaveId};
use crate::session_projection_repo::{
    Result as WorkerSessionProjectionResult, RuntimeId, ThreadAttribution,
    Tx as WorkerSessionProjectionTx, WorkerSessionInit, WorkerSessionKind, WorkerSessionProjection,
    WorkerSessionProjectionRepoError,
};
use calm_types::worker::{
    LivenessTag, WorkerContract, WorkerSession, WorkerSessionId, WorkerSessionState,
};

pub(super) fn ensure_runtime_status_transition(
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

pub(super) async fn session_repoint_current_links_tx(
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

pub(super) async fn session_set_status_mirror_tx(
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

pub(super) async fn session_complete_mirror_tx(
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

pub(super) async fn session_bind_attribution_mirror_tx(
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

pub(super) async fn session_clear_terminal_run_id_mirror_tx(
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

pub(super) async fn session_set_handle_state_mirror_tx(
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

pub(super) async fn session_set_active_turn_mirror_tx(
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

pub(super) async fn session_set_harness_observation_tx(
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

pub(super) async fn session_fail_if_active_tx(
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

pub(super) async fn session_mark_superseded_tx(
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

pub(super) async fn session_restore_from_superseded_tx(
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
