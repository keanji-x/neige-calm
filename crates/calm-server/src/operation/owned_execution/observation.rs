//! Notification handshake and read-only execution facts, independent of owner policy.
use super::*;
use crate::model::now_ms;
use std::time::Duration;

/// Error/claim-contention backoff, never a healthy execution's observation cadence.
const RETRY_DELAY: Duration = Duration::from_secs(1);

// Preserve durable reads even when subscribing fails: an offline transport must
// not hide a terminal operation or prevent an owner action already due.
pub(crate) struct ExecutionWatch(Result<ExecutionChanges>);
impl ExecutionWatch {
    /// Subscribe before reading owner state or execution facts to avoid missed wakes.
    pub(crate) async fn subscribe(ctx: &SpawnCtx) -> Self {
        Self(
            async {
                let engine = ctx
                    .execution
                    .engine()
                    .ok_or(CalmError::ExecutionUnavailable)?;
                tokio::time::timeout(RETRY_DELAY, engine.changes())
                    .await
                    .map_err(conflict)?
                    .map_err(conflict)
            }
            .await,
        )
    }

    /// A change only asks the caller to reload. Owners supply any meaningful
    /// deadline; stopping executions must retain real drain evidence.
    pub(crate) async fn wait(self, deadline_ms: Option<i64>) -> Result<()> {
        let changes = self.0?;
        let timer = async {
            match deadline_ms {
                Some(at) => {
                    tokio::time::sleep(Duration::from_millis(
                        at.saturating_sub(now_ms()).max(0) as u64
                    ))
                    .await
                }
                None => std::future::pending::<()>().await,
            }
        };
        tokio::select! {
            result = changes.changed() => result.map_err(conflict),
            _ = timer => Ok(()),
        }
    }

    pub(crate) async fn retry() {
        tokio::time::sleep(RETRY_DELAY).await;
    }
}

/// Facts that require owner reconciliation, shared by live observers and sweeps.
/// A committed Stop already belongs to Engine; only the first leader exit needs
/// an owner claim to request descendant drain. Notifications alone give no authority.
pub(crate) async fn execution_ready(ctx: &SpawnCtx, id: &ExecutionId) -> Result<bool> {
    let engine = ctx
        .execution
        .engine()
        .ok_or(CalmError::ExecutionUnavailable)?;
    let row = engine
        .store()
        .load(id)
        .await
        .map_err(conflict)?
        .ok_or_else(|| conflict("owned execution disappeared"))?;
    if row.is_settled() {
        return Ok(true);
    }
    if matches!(
        row,
        Stored::Active {
            desired: Desired::Stop | Desired::Retire,
            ..
        }
    ) {
        return Ok(false);
    }
    Ok(matches!(
        engine.output(id, None).await.map_err(conflict)?,
        ExecutionReply::Observation {
            state: ExecutionState::Exited { .. },
            ..
        }
    ))
}
