//! ACP runtime Stop follows durable provider ownership, never process markers.
use super::config::AcpPlannerHost;
use crate::{
    error::{CalmError, Result},
    provider_execution::StopReason,
};

pub(crate) use crate::provider_execution::quiescence::Quiescence;
pub(crate) enum AbandonmentProof {
    Track(Quiescence),
    Resident(ResidentQuiescence),
}

pub(crate) struct ResidentQuiescence {
    worker: String,
    submission: String,
    provider: calm_types::runtime::AgentProvider,
    execution: workspace_runtime::execution::ExecutionId,
}

impl AbandonmentProof {
    pub(crate) async fn recheck_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    ) -> Result<()> {
        match self {
            Self::Track(proof) => proof.recheck_tx(tx).await,
            Self::Resident(proof) => proof.recheck_tx(tx).await,
        }
    }
}

impl ResidentQuiescence {
    async fn recheck_tx(&self, tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>) -> Result<()> {
        use crate::db::sqlite::{AcpSharedRestartOwner, acp_shared_receipt_process_tx};
        let owner = acp_shared_receipt_process_tx(tx, &self.worker, &self.submission).await?;
        // Recovery fences tool calls before quiescence. The carrier replacement
        // transaction compares ingress and revokes any remaining credentials.
        let bound: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM acp_resident_workspace_bindings WHERE worker_session_id=?1)",
        )
        .bind(&self.worker)
        .fetch_one(&mut **tx)
        .await?;
        if !matches!(owner, Some(AcpSharedRestartOwner::Execution { execution_id }) if execution_id == self.execution)
            || bound
            || !super::resident_execution::has_drained_tx(tx, &self.provider, &self.execution)
                .await?
        {
            return Err(CalmError::Conflict(
                "Resident ACP execution quiescence is unproven".into(),
            ));
        }
        Ok(())
    }
}

pub(crate) async fn quiesce_for_abandonment(
    host: &AcpPlannerHost,
    provider: &calm_types::runtime::AgentProvider,
    session: &str,
    track: &str,
    submission: &str,
) -> Result<AbandonmentProof> {
    stop_session(host, session, StopReason::Shutdown).await?;
    let repo = host.completion.repo()?;
    let pool = repo.sqlite_db().ok_or(CalmError::ExecutionUnavailable)?;
    let (worker, client, provider) = (session.to_owned(), submission.to_owned(), provider.clone());
    let resident = crate::db::write_in_tx_typed(repo.as_ref(), move |tx| {
        Box::pin(async move {
            use crate::db::sqlite::{AcpSharedRestartOwner, acp_shared_receipt_process_tx};
            let Some(AcpSharedRestartOwner::Execution { execution_id }) =
                acp_shared_receipt_process_tx(tx, &worker, &client).await?
            else {
                return Ok(None);
            };
            let proof = ResidentQuiescence {
                worker,
                submission: client,
                provider,
                execution: execution_id,
            };
            proof.recheck_tx(tx).await?;
            Ok(Some(proof))
        })
    })
    .await?;
    if let Some(proof) = resident {
        return Ok(AbandonmentProof::Resident(proof));
    }
    Quiescence::capture(
        pool.read_pool(),
        session,
        track,
        super::execution::CONSUMER,
        crate::provider_execution::AliasPrefix::planner(),
    )
    .await
    .map(AbandonmentProof::Track)
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
    super::session::detach_worker(host, session).await?;
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
