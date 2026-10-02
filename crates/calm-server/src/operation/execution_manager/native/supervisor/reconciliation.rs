//! Bounded native recovery owned by the execution runtime, independent of Task state.
use super::{SharedCodexAppServer, execution_backend::CodexBackend};
use crate::error::{CalmError, Result};
use crate::ids::TrackId;
use crate::operation::execution_manager::{ExecutionManager, storage};
use std::time::Duration;

pub(crate) async fn reconcile_native_executions(
    service: &SharedCodexAppServer,
) -> Result<Vec<TrackId>> {
    let pool = service
        .repo
        .sqlite_pool()
        .ok_or_else(|| CalmError::Conflict("execution storage unavailable".into()))?;
    let manager = ExecutionManager::new(pool.clone());
    let backend = CodexBackend::for_service(service).await;
    let mut released = Vec::new();
    for (execution, track) in storage::native_recovery_candidates(&pool).await? {
        match tokio::time::timeout(
            Duration::from_secs(5),
            manager.recover(&backend, &execution),
        )
        .await
        {
            Ok(Ok(true)) => released.push(TrackId::from(track)),
            Ok(Err(error)) => {
                tracing::debug!(%error, %execution, "native execution retained for recovery")
            }
            _ => {}
        }
    }
    Ok(released)
}
