//! Read-only scheduling hints over the Gate owner's durable deadline guard.
//! Only claimed recovery authorizes release, stop, sampling, or completion.
use super::*;
use crate::model::now_ms;
use crate::operation::owned_execution::observation::{ExecutionWatch, execution_ready};

pub(crate) struct GateWatch(ExecutionWatch);
impl GateWatch {
    pub(crate) async fn subscribe(ctx: &SpawnCtx) -> Self {
        Self(ExecutionWatch::subscribe(ctx).await)
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
        // Boot recovery may already have persisted Stop before its observer
        // starts. Its startup flags must never force another claim while draining.
        if (force && record(op)?.decision == Decision::Observing)
            || recovery_ready(ctx, op, timeout_secs).await?
        {
            return Ok(true);
        }
        self.wait(op, timeout_secs).await?;
        Ok(false)
    }

    /// The persisted parked deadline remains the recovery backstop. Stopping
    /// executions wait for real drain evidence rather than spin on an old deadline.
    pub(crate) async fn wait(self, op: &Operation, timeout_secs: i64) -> Result<()> {
        self.0.wait(deadline(op, timeout_secs)?).await
    }

    pub(crate) async fn retry() {
        ExecutionWatch::retry().await;
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
    execution_ready(ctx, &record.execution_id).await
}
