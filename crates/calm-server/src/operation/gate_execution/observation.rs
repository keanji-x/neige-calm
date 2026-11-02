//! Read-only scheduling hints over the Gate owner's durable deadline guard.
//! Only claimed recovery authorizes release, stop, sampling, or completion.
use super::*;
use crate::model::now_ms;
use std::time::Duration;

/// Failure/claim-contention backoff, never the cadence for healthy executions.
pub(crate) const RETRY_DELAY: Duration = Duration::from_secs(1);

pub(crate) struct GateWatch(ExecutionChanges);
impl GateWatch {
    pub(crate) async fn subscribe(ctx: &SpawnCtx) -> Result<Self> {
        let engine = ctx
            .execution
            .engine()
            .ok_or(CalmError::ExecutionUnavailable)?;
        tokio::time::timeout(RETRY_DELAY, engine.changes())
            .await
            .map_err(conflict)?
            .map(Self)
            .map_err(conflict)
    }

    /// All Gate consumers share the subscribe/read/wait handshake. Adapter boot
    /// policies may force one recovery pass without changing normal readiness.
    pub(crate) async fn ready_or_wait(
        self,
        ctx: &SpawnCtx,
        op: &Operation,
        timeout_secs: i64,
        force: bool,
    ) -> Result<bool> {
        if force || recovery_ready(ctx, op, timeout_secs).await? {
            return Ok(true);
        }
        self.wait(op, timeout_secs).await?;
        Ok(false)
    }

    /// The persisted parked deadline remains the recovery backstop. Stopping
    /// executions wait for real drain evidence rather than spin on an old deadline.
    pub(crate) async fn wait(self, op: &Operation, timeout_secs: i64) -> Result<()> {
        let deadline = deadline(op, timeout_secs)?;
        let timer = async {
            match deadline {
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
            result = self.0.changed() => result.map_err(conflict),
            _ = timer => Ok(()),
        }
    }

    pub(crate) async fn retry() {
        tokio::time::sleep(RETRY_DELAY).await;
    }
}

fn deadline(op: &Operation, timeout_secs: i64) -> Result<Option<i64>> {
    let record = record(op)?;
    if record.decision != Decision::Observing {
        return Ok(None);
    }
    let live = match record.release {
        Release::Held => None,
        Release::Releasing { authorized_at_ms } | Release::Released { authorized_at_ms } => {
            Some(authorized_at_ms.saturating_add(timeout_secs.saturating_mul(1000)))
        }
    };
    Ok(match (op.parked_deadline_ms, live) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    })
}

/// Shared by live observers and steady recovery sweeps. Unrelated execution
/// notifications may cause reads, but never acquire an operation claim by themselves.
pub(crate) async fn recovery_ready(
    ctx: &SpawnCtx,
    op: &Operation,
    timeout_secs: i64,
) -> Result<bool> {
    let record = record(op)?;
    if record.release == Release::Held && record.decision == Decision::Observing {
        return Ok(true);
    }
    if deadline(op, timeout_secs)?.is_some_and(|at| now_ms() >= at) {
        return Ok(true);
    }
    let engine = ctx
        .execution
        .engine()
        .ok_or(CalmError::ExecutionUnavailable)?;
    let row = engine
        .store()
        .load(&record.execution_id)
        .await
        .map_err(conflict)?
        .ok_or_else(|| conflict("owned execution disappeared"))?;
    if row.is_settled() {
        return Ok(true);
    }
    // A committed Stop is already driven by Engine; only the first leader-exit
    // observation needs the owner to request descendant drain.
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
        engine
            .output(&record.execution_id, None)
            .await
            .map_err(conflict)?,
        ExecutionReply::Observation {
            state: ExecutionState::Exited { .. },
            ..
        }
    ))
}
