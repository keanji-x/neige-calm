//! ACP runtime Stop follows durable provider ownership, never process markers.
use super::config::AcpPlannerHost;
use crate::{
    error::{CalmError, Result},
    provider_execution::StopReason,
};

use workspace_runtime::execution::{ExecutionMetadata, Settlement};

/// Captured from the execution owner; absence of an owner claim alone is not proof.
pub(crate) struct Quiescence {
    rows: Vec<(String, String, Option<String>)>,
    session: String,
    track: String,
}
impl Quiescence {
    pub(crate) async fn recheck_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    ) -> Result<()> {
        let rows = execution_rows(&mut **tx, &self.session).await?;
        if rows != self.rows {
            return Err(CalmError::Conflict("ACP execution evidence changed".into()));
        }
        validate_rows(&rows, &self.session, &self.track)
    }
}
async fn execution_rows<'a>(
    executor: impl sqlx::Executor<'a, Database = sqlx::Sqlite>,
    session: &str,
) -> Result<Vec<(String, String, Option<String>)>> {
    let prefix = crate::provider_execution::AliasPrefix::planner().alias(session, "");
    Ok(sqlx::query_as(concat!(
        "SELECT execution_id,intent_json,settlement_json FROM workspace_executions WHERE ",
        "substr(alias,1,length(?1))=?1 ORDER BY execution_id"
    ))
    .bind(prefix)
    .fetch_all(executor)
    .await?)
}
fn validate_rows(
    rows: &[(String, String, Option<String>)],
    session: &str,
    track: &str,
) -> Result<()> {
    if rows.is_empty() {
        return Err(CalmError::Conflict(
            "ACP execution absence is unproven".into(),
        ));
    }
    for (id, intent, settlement) in rows {
        let metadata: ExecutionMetadata = serde_json::from_str(intent)?;
        let settled: Option<Settlement> = settlement
            .as_deref()
            .map(serde_json::from_str)
            .transpose()?;
        if metadata.id.as_str() != id
            || metadata.consumer.0 != super::execution::CONSUMER
            || metadata.track.0 != track
            || metadata.alias.0
                != crate::provider_execution::AliasPrefix::planner().alias(session, id)
            || !settled.is_some_and(|s| s.descendants_complete())
        {
            return Err(CalmError::Conflict(
                "ACP execution quiescence is unproven".into(),
            ));
        }
    }
    Ok(())
}
pub(crate) async fn quiesce_for_abandonment(
    host: &AcpPlannerHost,
    session: &str,
    track: &str,
) -> Result<Quiescence> {
    stop_session(host, session, StopReason::Shutdown).await?;
    let repo = host.completion.repo()?;
    let pool = repo.sqlite_db().ok_or(CalmError::ExecutionUnavailable)?;
    let rows = execution_rows(pool.read_pool(), session).await?;
    validate_rows(&rows, session, track)?;
    Ok(Quiescence {
        rows,
        session: session.into(),
        track: track.into(),
    })
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
