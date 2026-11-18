//! Typed worker-session CAS. Only the owning Worker role may interpret this JSON branch.
use super::session_get_tx;
use crate::error::{CalmError, Result};
use crate::session_repo::Tx as SessionTx;
use calm_types::{
    acp_worker::AcpWorkerState, provider_worker::ProviderWorkerCapabilities,
    runtime::AgentProvider, worker::WorkerSessionId,
};

pub async fn acp_worker_state_tx(
    tx: &mut SessionTx<'_>,
    session_id: &str,
    attempt_id: &str,
) -> Result<AcpWorkerState> {
    let session = session_get_tx(tx, &WorkerSessionId::from(session_id))
        .await?
        .ok_or_else(|| CalmError::Conflict("ACP worker session missing"))?;
    let allowed = AgentProvider::try_from(session.provider)
        .ok()
        .and_then(ProviderWorkerCapabilities::for_provider)
        .is_some_and(|cap| {
            cap.admits(session.contract, session.mode)
                && cap.operation == calm_types::provider_worker::WorkerOperationKind::AcpWorker
        });
    if !allowed {
        return Err(CalmError::Conflict(
            "session is not a resumable task worker",
        ));
    }
    let state: AcpWorkerState = serde_json::from_value(
        session
            .handle_state_json
            .ok_or_else(|| CalmError::Conflict("ACP worker state missing"))?,
    )?;
    if state.version != 1 || state.attempt_id != attempt_id {
        return Err(CalmError::Conflict("ACP worker attempt binding changed"));
    }
    Ok(state)
}
/// Compare both the durable phase and the native turn identity. A late predecessor cannot settle
/// or overwrite a successor's state. Caller also rechecks operation/attempt authority in its tx.
pub async fn acp_worker_state_cas_tx(
    tx: &mut SessionTx<'_>,
    session_id: &str,
    attempt_id: &str,
    active_turn_id: Option<&str>,
    expected: &AcpWorkerState,
    next: &AcpWorkerState,
) -> Result<bool> {
    if expected.attempt_id != attempt_id || next.attempt_id != attempt_id {
        return Err(CalmError::Conflict("ACP worker state crosses attempts"));
    }
    if acp_worker_state_tx(tx, session_id, attempt_id).await? != *expected {
        return Ok(false);
    }
    let stored: String =
        sqlx::query_scalar("SELECT handle_state_json FROM worker_sessions WHERE id=?1")
            .bind(session_id)
            .fetch_one(&mut **tx)
            .await?;
    Ok(sqlx::query("UPDATE worker_sessions SET handle_state_json=?1,updated_at_ms=?2 WHERE id=?3 AND active_turn_id IS ?4 AND handle_state_json=?5")
        .bind(serde_json::to_value(next)?.to_string()).bind(crate::model::now_ms()).bind(session_id).bind(active_turn_id).bind(stored).execute(&mut **tx).await?.rows_affected() == 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::sqlite::{
        runtime_read_flip_support::{create_card_in_tx, fresh_repo},
        session_insert_tx,
    };
    use calm_types::{
        acp_worker::{WorkerRequestedSelection, WorkerTurnPhase},
        worker::{
            LivenessTag, SessionMode, WorkerContract, WorkerProviderKind, WorkerSession,
            WorkerSessionState,
        },
    };
    use serde_json::json;
    #[tokio::test]
    async fn session_row_worker_state_cas_uses_normal_insert_and_rejects_stale_turn_attempt() {
        let repo = fresh_repo().await;
        let db = repo.db().clone();
        db.writer()
            .with_writer(|writer| {
                Box::pin(async move {
                    let mut tx = writer.begin_immediate().await.unwrap();
                    let card = create_card_in_tx(
                        &repo,
                        &mut tx,
                        "worker",
                        "codex",
                        json!({"schemaVersion":1}),
                    )
                    .await;
                    let state = AcpWorkerState::prepared(
                        "attempt".into(),
                        WorkerRequestedSelection {
                            provider: AgentProvider::Antigravity,
                            model: None,
                            reasoning_effort: None,
                        },
                    );
                    let track: String =
                        sqlx::query_scalar("SELECT track_id FROM cards WHERE id=?1")
                            .bind(&card)
                            .fetch_one(&mut **tx)
                            .await
                            .unwrap();
                    session_insert_tx(
                        &mut tx,
                        WorkerSession {
                            id: WorkerSessionId::from("worker-session"),
                            track_id: calm_types::ids::TrackId::from(track),
                            provider: WorkerProviderKind::Antigravity,
                            mode: SessionMode::Resumable,
                            contract: WorkerContract::Validator,
                            parent_session_id: None,
                            requester_session_id: None,
                            state: WorkerSessionState::Starting,
                            mcp_token_hash: None,
                            thread_id: None,
                            agent_session_id: None,
                            active_turn_id: Some("turn".into()),
                            terminal_run_id: None,
                            card_id: Some(card.into()),
                            handle_state_json: Some(serde_json::to_value(&state).unwrap()),
                            liveness: LivenessTag::Unknown,
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
                    let mut submitting = state.clone();
                    submitting.turn_phase = WorkerTurnPhase::Submitting;
                    assert!(
                        !acp_worker_state_cas_tx(
                            &mut tx,
                            "worker-session",
                            "attempt",
                            Some("old-turn"),
                            &state,
                            &submitting
                        )
                        .await
                        .unwrap()
                    );
                    assert!(
                        acp_worker_state_cas_tx(
                            &mut tx,
                            "worker-session",
                            "other-attempt",
                            Some("turn"),
                            &state,
                            &submitting
                        )
                        .await
                        .is_err()
                    );
                    assert!(
                        acp_worker_state_cas_tx(
                            &mut tx,
                            "worker-session",
                            "attempt",
                            Some("turn"),
                            &state,
                            &submitting
                        )
                        .await
                        .unwrap()
                    );
                    assert!(
                        !acp_worker_state_cas_tx(
                            &mut tx,
                            "worker-session",
                            "attempt",
                            Some("turn"),
                            &state,
                            &submitting
                        )
                        .await
                        .unwrap()
                    );
                    assert_eq!(
                        acp_worker_state_tx(&mut tx, "worker-session", "attempt")
                            .await
                            .unwrap(),
                        submitting
                    );
                    use super::super::{WorkerCleanupVerdict, worker_operation_cleanup_tx};
                    use calm_types::provider_worker::WorkerOperationKind;
                    let mut clean = submitting.clone();
                    clean.cleanup_confirmed = true;
                    assert!(acp_worker_state_cas_tx(&mut tx, "worker-session", "attempt", Some("turn"), &submitting, &clean).await.unwrap());
                    // A native-close bit cannot hide an active round or a retained credential.
                    assert_eq!(worker_operation_cleanup_tx(&mut tx, WorkerOperationKind::AcpWorker, Some("worker-session"), Some("attempt")).await.unwrap(), WorkerCleanupVerdict::Pending);
                    sqlx::query("UPDATE worker_sessions SET active_turn_id=NULL,mcp_token_hash='retained' WHERE id='worker-session'").execute(&mut **tx).await.unwrap();
                    assert_eq!(worker_operation_cleanup_tx(&mut tx, WorkerOperationKind::AcpWorker, Some("worker-session"), Some("attempt")).await.unwrap(), WorkerCleanupVerdict::Pending);
                    sqlx::query("UPDATE worker_sessions SET mcp_token_hash=NULL WHERE id='worker-session'").execute(&mut **tx).await.unwrap();
                    assert_eq!(worker_operation_cleanup_tx(&mut tx, WorkerOperationKind::AcpWorker, Some("worker-session"), Some("attempt")).await.unwrap(), WorkerCleanupVerdict::Confirmed);
                    assert!(worker_operation_cleanup_tx(&mut tx, WorkerOperationKind::AcpWorker, Some("worker-session"), Some("other-attempt")).await.is_err());

                })
            })
            .await
            .unwrap();
    }
}
