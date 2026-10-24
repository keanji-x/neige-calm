//! Shared extra-worktree policy for close ticks and post-delete sweeps.
use crate::{error::WorkspaceError, track_worktree::ObservePoint};
use async_trait::async_trait;
use workspace::reclaim::ReclaimRequest;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ExtraWorktree {
    pub operation_id: String,
    pub provision_id: i64,
    pub track_id: String,
    pub card_id: String,
    pub area_id: String,
    pub request: ReclaimRequest,
}

#[async_trait]
pub trait ReclaimReceipts: Send + Sync {
    type Error: std::fmt::Display + Send;
    async fn reclaim(
        &self,
        target: &ExtraWorktree,
        observe: ObservePoint,
    ) -> Result<(), Self::Error>;
}

/// Delegate effects and durable receipt recovery to the owning operations adapter. Failures retain
/// the event claim; close ticks retry independently of the ignored-clean history.
pub async fn reclaim_extra_worktrees<P: ReclaimReceipts>(
    targets: Vec<ExtraWorktree>,
    ports: &P,
    observe: ObservePoint,
) -> Result<(), WorkspaceError> {
    for target in targets {
        if let Err(error) = ports.reclaim(&target, observe).await {
            tracing::warn!(path=%target.request.path.display(), %error, "extra worktree retained");
        }
    }
    Ok(())
}
