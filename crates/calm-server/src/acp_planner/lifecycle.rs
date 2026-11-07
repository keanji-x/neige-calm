//! ACP runtime Stop follows durable provider ownership, never process markers.
use super::config::AcpPlannerHost;
use crate::{
    error::{CalmError, Result},
    provider_execution::StopReason,
};

pub(crate) use crate::provider_execution::quiescence::Quiescence;
pub(crate) async fn quiesce_for_abandonment(
    host: &AcpPlannerHost,
    session: &str,
    track: &str,
) -> Result<Quiescence> {
    stop_session(host, session, StopReason::Shutdown).await?;
    let repo = host.completion.repo()?;
    let pool = repo.sqlite_db().ok_or(CalmError::ExecutionUnavailable)?;
    Quiescence::capture(
        pool.read_pool(),
        session,
        track,
        super::execution::CONSUMER,
        crate::provider_execution::AliasPrefix::planner(),
    )
    .await
}

pub(crate) async fn stop_session(
    host: &AcpPlannerHost,
    session: &str,
    reason: StopReason,
) -> Result<()> {
    let repo = host.completion.repo()?;
    let pool = repo.sqlite_db().ok_or(CalmError::ExecutionUnavailable)?;
    host.provider_execution
        .stop_session(
            &pool,
            host.execution.engine(),
            session,
            reason,
            std::time::Duration::from_secs(12),
        )
        .await?;
    // Registered settlement cannot prove absence of an unregistered pre-C4 process.
    if !host
        .pre_c4
        .get()
        .is_some_and(|sweep| sweep.proves_absent(session))
    {
        return Err(CalmError::Conflict(format!(
            "ACP pre-C4 process absence unproven for session {session}"
        )));
    }
    Ok(())
}

pub(crate) async fn abandonment_request_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    carrier: &crate::db::sqlite::conversation::CurrentInputCarrier,
) -> Result<Option<calm_types::conversation::AbandonConversationRequest>> {
    let request = crate::db::sqlite::conversation::abandonment_request_tx(
        tx,
        carrier,
        crate::db::sqlite::conversation::AbandonmentMode::UnknownInput,
    )
    .await?;
    let Some(request) = request else {
        return Ok(None);
    };
    let receipt = crate::db::sqlite::acp_submission_get(
        &mut **tx,
        carrier.worker_session_id(),
        &request.submission_id,
    )
    .await?;
    Ok(receipt
        .filter(|r| {
            matches!(r.state, crate::db::sqlite::AcpSubmissionState::Unknown)
                && r.outcome_json.is_none()
                && r.kept_cleanup.is_none()
        })
        .map(|_| request))
}
