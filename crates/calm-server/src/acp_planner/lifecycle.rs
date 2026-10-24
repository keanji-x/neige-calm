//! ACP runtime Stop follows durable provider ownership, never process markers.
use super::config::AcpPlannerHost;
use crate::{
    error::{CalmError, Result},
    provider_execution::StopReason,
};

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
